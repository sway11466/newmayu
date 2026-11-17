//! 本体。フックを張り、`.mayu` の設定でキーを差し替え、タスクトレイに常駐する。
//!
//! ```text
//! newmayu [-Dシンボル…] [--log <path>] [--test-input <印>] [--test-mayu <file.mayu>]
//! ```
//!
//! 読む `.mayu` は exe と同じフォルダの `yamy.ini` の設定の一覧で、選択中のもの（yamy と同じ。
//! [`newmayu::setting`]）。`-Dシンボル` はそのシンボルを `define` 済みにする（yamy と同じ）。
//!
//! `--log` があればログをそのファイルに追記する。無ければ標準エラー。
//!
//! `--test-mayu` はテスト用。`yamy.ini` を見ずにその `.mayu` を読む。
//!
//! `--test-input <印>`（16 進。例 `0x6e6d`）はテスト入力モード。`dwExtraInfo` がその印の
//! 注入入力（`SendInput` されたもの）を物理入力として扱う。印の無い注入入力は今までどおり
//! 素通し。テスト用の実行ファイル（`src/bin/selftest.rs`）が使う。
//!
//! 構成は yamy と同じ。コールバックはイベントをキューに積んで `1` を返し（抑制）、
//! 別スレッドのエンジンが処理して `SendInput` する。
//! フック DLL（`newmayu_hook.dll`、同じフォルダ）が各プロセスから送る IME の状態も
//! 同じキューに積み、エンジンに渡す。
//!
//! トレイのメニュー:
//! - 「設定の再読込」: `.mayu` を読み直し、通ればエンジンを作り直す。通らなければ今のまま
//! - 「選択」: 設定の一覧から選び、その設定を読む
//! - 「設定」: 設定ダイアログ。OK なら設定を読み直す
//! - 「一時停止」: フックのコールバックで何もせず次へ流す。もう一度で再開
//! - 「ログ」: ログダイアログを出す
//! - 「終了」: メッセージループを抜け、フックを外し、エンジンが押したままにしている
//!   キーを離し、フック DLL とアイコンを外して終わる

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, HC_ACTION, IsDialogMessageW, KBDLLHOOKSTRUCT,
    MB_ICONERROR, MB_OK, MSG, MessageBoxW, PostQuitMessage, SetWindowsHookExW, TranslateMessage,
    UnhookWindowsHookEx, WH_KEYBOARD_LL,
};
use windows::core::HSTRING;

use newmayu::config::parser;
use newmayu::engine::{Engine, Input, Os, Output};
use newmayu::help::Help;
use newmayu::investigate;
use newmayu::log_dialog;
use newmayu::model::{Config, Key, LockMod};
use newmayu::notify::{HookDll, Mailslot, SyncShare};
use newmayu::platform;
use newmayu::setting::{self, Ini, Lookup};
use newmayu::setting_dialog;
use newmayu::ui::{self, Command, Tray};
use newmayu::window::{self, WindowOps};
use newmayu::{log, log_detail};
use newmayu_ipc::{
    CAPS_LOCK, IME_COMPOSING, IME_OPEN, KANA_LOCK, Kind, NUM_LOCK, Notify, SCROLL_LOCK,
};

/// 終了時、エンジンスレッドが押下中のキーを離し終えるのを待つ上限。
const QUIT_WAIT: Duration = Duration::from_secs(2);
/// `&Sync` で同期キーの通知を待つ上限（yamy と同じ 5 秒）。
const SYNC_WAIT: Duration = Duration::from_secs(5);

/// エンジンスレッドに届くもの。
enum Event {
    Key(Input),
    /// フック DLL からの通知。
    Notify(Notify),
    /// 設定の再読込。エンジンを作り直す。
    Reload(Box<Config>),
    /// 終了。押下中のキーを離してから `Sender` に知らせる。
    Quit(Sender<()>),
}

/// フックのコールバックからエンジンスレッドへ渡すキュー。
/// コールバックは C の関数でユーザーポインタを受け取れないため static に置く。
static TX: OnceLock<Sender<Event>> = OnceLock::new();

/// newmayu の窓（トレイ・ダイアログ）のスレッド。`&InvestigateCommand` で自分の分を出さないため。
static UI_THREAD: AtomicU32 = AtomicU32::new(0);

/// 一時停止中。コールバックは何もせず次のフックへ流す。
static PAUSED: AtomicBool = AtomicBool::new(false);

/// テスト入力モードの印（`--test-input`）。フックを掛ける前に設定する。
static TEST_MARK: OnceLock<Option<usize>> = OnceLock::new();

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && !PAUSED.load(Ordering::Relaxed) {
        // SAFETY: WH_KEYBOARD_LL では lparam は KBDLLHOOKSTRUCT を指す。
        let k = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let test_mark = TEST_MARK.get().copied().flatten();
        if let Some(input) = platform::input_from_hook(k, test_mark) {
            // エンジンスレッドが居なくなっていたら（送れなかったら）抑制せず流す。
            // そうしないとキーボードが死ぬ。
            if let Some(tx) = TX.get()
                && tx.send(Event::Key(input)).is_ok()
            {
                return LRESULT(1);
            }
        }
    }
    // SAFETY: 引数はそのまま次のフックへ渡す。
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// コマンドライン引数。
#[derive(Debug, PartialEq, Eq)]
struct Args {
    /// `-Dシンボル` のシンボル（yamy と同じ）。
    symbols: Vec<String>,
    log: Option<PathBuf>,
    /// テスト入力モードの印。
    test_mark: Option<usize>,
    /// テスト用。`yamy.ini` を見ずに読む `.mayu`。
    test_mayu: Option<PathBuf>,
}

const USAGE: &str = "使い方: newmayu [-Dシンボル…] [--log <path>] [--test-input <印(16進)>] [--test-mayu <file.mayu>]";

/// `--test-input` の値。`0x` は付いていてもいなくてもよい。0 は普通の注入入力と
/// 区別が付かない（`SendInput` の既定の `dwExtraInfo` が 0）ので受け付けない。
fn parse_test_mark(s: &str) -> Option<usize> {
    let digits = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    match usize::from_str_radix(digits, 16) {
        Ok(0) | Err(_) => None,
        Ok(n) => Some(n),
    }
}

/// プログラム名を除いた引数を読む。形が違えば `Err(使い方)`。
fn parse_args<I: IntoIterator<Item = String>>(args: I) -> Result<Args, String> {
    let mut symbols = Vec::new();
    let mut log = None;
    let mut test_mark = None;
    let mut test_mayu = None;
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        if a == "--log" {
            match (it.next(), &log) {
                (Some(p), None) => log = Some(PathBuf::from(p)),
                _ => return Err(USAGE.to_string()),
            }
        } else if a == "--test-input" {
            match (it.next().as_deref().and_then(parse_test_mark), &test_mark) {
                (Some(m), None) => test_mark = Some(m),
                _ => return Err(USAGE.to_string()),
            }
        } else if a == "--test-mayu" {
            match (it.next(), &test_mayu) {
                (Some(p), None) => test_mayu = Some(PathBuf::from(p)),
                _ => return Err(USAGE.to_string()),
            }
        } else if let Some(sym) = a.strip_prefix("-D") {
            symbols.push(sym.to_string());
        } else {
            return Err(USAGE.to_string());
        }
    }
    Ok(Args {
        symbols,
        log,
        test_mark,
        test_mayu,
    })
}

/// 設定を読む（yamy の `load`）。読むたびに `yamy.ini` を見直す。
/// 読んだ `.mayu` のパスも返す。
///
/// 選択中の設定も exe のフォルダの `.mayu` も無ければ、yamy と同じく設定ダイアログを出し、
/// OK ならもう一度探す。
fn load_config(symbols: &[String], test_mayu: Option<&Path>) -> Result<(Config, PathBuf), String> {
    if let Some(p) = test_mayu {
        return parser::load(p, symbols)
            .map(|c| (c, p.to_path_buf()))
            .map_err(|e| e.to_string());
    }
    let ini = Ini::beside_exe()?;
    let dir = setting::exe_dir()?;
    loop {
        match setting::lookup(ini.selected().as_deref(), &dir, |p| {
            std::fs::File::open(p).is_ok()
        }) {
            Lookup::Load { path, symbols: own } => {
                let all: Vec<String> = symbols.iter().cloned().chain(own).collect();
                return parser::load(&path, &all)
                    .map(|c| (c, path))
                    .map_err(|e| e.to_string());
            }
            Lookup::Unreadable(p) => {
                return Err(format!("{}: 読めない（選択中の設定）", p.display()));
            }
            Lookup::NoSetting => {
                if !setting_dialog::run(&ini) {
                    return Err(format!(
                        "設定が無い（{} に選択中の設定が無く、{} も無い）",
                        ini.path().display(),
                        dir.join(".mayu").display()
                    ));
                }
            }
        }
    }
}

/// 設定を読み直し、通ればエンジンを作り直す。通らなければ今のまま。
fn reload(tx: &Sender<Event>, symbols: &[String], test_mayu: Option<&Path>) {
    match load_config(symbols, test_mayu) {
        Ok((c, path)) => {
            log!("再読込: {}", path.display());
            let _ = tx.send(Event::Reload(Box::new(c)));
        }
        Err(e) => {
            log!("再読込に失敗。今の設定のまま: {e}");
            // yamy と同じく、読み込みに失敗したらログを出す
            log_dialog::show();
        }
    }
}

/// `&LoadSetting([名前])`（yamy の `funcLoadSetting`）。名前があれば設定の一覧でその名前の設定を選び、
/// 再読込をトレイのスレッドでさせる。名前が一覧に無ければ何もしない。
fn load_setting(name: &str) {
    if !name.is_empty() {
        let selected = match Ini::beside_exe() {
            Ok(ini) => ini.select_by_name(name),
            Err(e) => {
                log!("{e}");
                false
            }
        };
        if !selected {
            log!("unknown setting name: {name}");
            return;
        }
    }
    ui::post_reload();
}

/// エンジンが OS 側で押したままにしているキー。終了時にまとめて離す。
#[derive(Default)]
struct Pressed {
    keys: HashSet<Key>,
    /// `&VK` で押したもの（仮想キーコード, extended）。
    vks: HashSet<(u8, bool)>,
}

impl Pressed {
    fn track(&mut self, outputs: &[Output]) {
        for o in outputs {
            match *o {
                Output::Press(k) => {
                    self.keys.insert(k);
                }
                Output::Release(k) => {
                    self.keys.remove(&k);
                }
                Output::Vk { vk, extended, up } => {
                    if up {
                        self.vks.remove(&(vk, extended));
                    } else {
                        self.vks.insert((vk, extended));
                    }
                }
                Output::Func { .. }
                | Output::Sync
                | Output::Help { .. }
                | Output::ShellExecute(_)
                | Output::MayuDialog(_)
                | Output::LogClear
                | Output::InvestigateCommand
                | Output::LoadSetting(_)
                | Output::DescribeBindings(_) => {}
            }
        }
    }

    /// 押したままのキーを離す出力。
    fn releases(&self) -> Vec<Output> {
        let keys = self.keys.iter().map(|k| Output::Release(*k));
        let vks = self.vks.iter().map(|&(vk, extended)| Output::Vk {
            vk,
            extended,
            up: true,
        });
        keys.chain(vks).collect()
    }
}

/// `&Sync`: 同期キーを出し、フォーカス窓のスレッドがそれを取り出した通知を待つ（yamy の funcSync）。
/// 待っている間に届いた他のイベントは `pending` に積み、後で処理する。
fn wait_sync(sync: Option<Key>, rx: &Receiver<Event>, pending: &mut VecDeque<Event>) {
    let Some(key) = sync else {
        log!("&Sync: def sync が無いので待たない");
        return;
    };
    platform::send(&[Output::Release(key)]);
    let deadline = Instant::now() + SYNC_WAIT;
    loop {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        match rx.recv_timeout(deadline - now) {
            Ok(Event::Notify(n)) if n.header.kind == Kind::Sync => {
                log_detail!(
                    "&Sync: 届いた (thread={} hwnd={:#x})",
                    n.header.thread_id,
                    n.header.hwnd
                );
                return;
            }
            Ok(ev) => pending.push_back(ev),
            Err(_) => break,
        }
    }
    log!("&Sync: {SYNC_WAIT:?} 待っても届かなかった");
}

/// エンジンの出力を出た順に実行する OS 窓口。キーは溜めておき、関数・`&Sync`・クリップボードの前と
/// 1 入力の最後にまとめて `SendInput` する。
struct Exec<'a> {
    sync: Option<Key>,
    /// まだ送っていないキー。
    keys: Vec<Output>,
    /// ログ用に、この入力で出た全部。
    all: Vec<Output>,
    window_ops: &'a mut WindowOps,
    help: &'a Help,
    share: &'a SyncShare,
    pressed: &'a mut Pressed,
    rx: &'a Receiver<Event>,
    pending: &'a mut VecDeque<Event>,
}

impl Exec<'_> {
    fn flush(&mut self) {
        platform::send(&self.keys);
        self.keys.clear();
    }
}

impl Os for Exec<'_> {
    fn output(&mut self, o: Output) {
        self.all.push(o.clone());
        self.pressed.track(std::slice::from_ref(&o));
        match o {
            Output::Press(_) | Output::Release(_) | Output::Vk { .. } => self.keys.push(o),
            Output::Func { .. } | Output::Help { .. } => {
                self.flush();
                let one = std::slice::from_ref(&o);
                self.window_ops.run(one);
                self.help.run(one);
            }
            Output::Sync => {
                self.flush();
                wait_sync(self.sync, self.rx, self.pending);
            }
            Output::ShellExecute(s) => {
                self.flush();
                window::shell_execute(s);
            }
            Output::MayuDialog(show) => {
                self.flush();
                log_dialog::post_show(show);
            }
            Output::LogClear => {
                self.flush();
                log_dialog::post_clear();
            }
            Output::InvestigateCommand => {
                self.flush();
                // yamy と同じく、切り替えた後の状態を出す
                if self.share.toggle_notify_command() {
                    log!(" begin");
                } else {
                    log!(" end");
                }
            }
            Output::LoadSetting(name) => {
                self.flush();
                load_setting(&name);
            }
            Output::DescribeBindings(lines) => {
                self.flush();
                // yamy と同じく、詳細のときだけ前に空行を出す
                log_detail!("");
                for l in lines {
                    log!("{l}");
                }
            }
        }
    }
    fn clipboard_text(&mut self) -> Option<String> {
        self.flush();
        window::clipboard_text()
    }
    fn empty_clipboard(&mut self) {
        self.flush();
        window::empty_clipboard();
    }
    fn set_clipboard_text(&mut self, text: &str) {
        self.flush();
        window::set_clipboard_text(text);
    }
}

/// エンジンスレッドの本体。`Quit` で押下中のキーを離して抜ける。
fn engine_thread(config: Config, rx: Receiver<Event>, sync_share: SyncShare, help: Help) {
    let mut engine = Engine::new(config);
    let mut window_ops = WindowOps::new();
    let mut pressed = Pressed::default();
    let mut pending: VecDeque<Event> = VecDeque::new();
    loop {
        let ev = match pending.pop_front() {
            Some(ev) => ev,
            None => match rx.recv() {
                Ok(ev) => ev,
                Err(_) => break,
            },
        };
        match ev {
            Event::Reload(config) => {
                // 前のエンジンが押したままのキーは離してから作り直す
                let releases = pressed.releases();
                if !releases.is_empty() {
                    log!("再読込: 押下中のキーを離す {releases:?}");
                    platform::send(&releases);
                }
                pressed = Pressed::default();
                sync_share.set(config.sync);
                engine = Engine::new(*config);
                log!("再読込: エンジンを作り直した");
            }
            Event::Key(input) => {
                let mut exec = Exec {
                    sync: engine.config().sync,
                    keys: Vec::new(),
                    all: Vec::new(),
                    window_ops: &mut window_ops,
                    help: &help,
                    share: &sync_share,
                    pressed: &mut pressed,
                    rx: &rx,
                    pending: &mut pending,
                };
                engine.feed(input, &mut exec);
                exec.flush();
                log_detail!("{input:?} -> {:?}", exec.all);
            }
            Event::Notify(n) => {
                let h = n.header;
                match h.kind {
                    Kind::Lock => {
                        log_detail!(
                            "IME: on={} 変換中={} NL={} CL={} SL={} KL={} (thread={} hwnd={:#x} cause={})",
                            h.ime_open(),
                            h.ime_composing(),
                            h.lock(NUM_LOCK),
                            h.lock(CAPS_LOCK),
                            h.lock(SCROLL_LOCK),
                            h.lock(KANA_LOCK),
                            h.thread_id,
                            h.hwnd,
                            h.cause
                        );
                        for (flag, m) in [
                            (IME_OPEN, LockMod::ImeLock),
                            (IME_COMPOSING, LockMod::ImeComp),
                            (NUM_LOCK, LockMod::NumLock),
                            (CAPS_LOCK, LockMod::CapsLock),
                            (SCROLL_LOCK, LockMod::ScrollLock),
                            (KANA_LOCK, LockMod::KanaLock),
                        ] {
                            engine.set_lock(m, h.lock(flag));
                        }
                    }
                    Kind::Sync => log_detail!(
                        "同期キーが届いた（待っていない） (thread={} hwnd={:#x} cause={})",
                        h.thread_id,
                        h.hwnd,
                        h.cause
                    ),
                    Kind::Name => investigate::log_name(h.hwnd, h.thread_id, &n.class, &n.title),
                    Kind::Command => investigate::log_command(
                        engine.focus_hwnd(),
                        UI_THREAD.load(Ordering::Relaxed),
                        h.hwnd,
                        h.flags,
                        n.params.0,
                        n.params.1,
                    ),
                    Kind::Focus => {
                        engine.set_focus(h.hwnd, &n.class, &n.title);
                        let names: Vec<&str> = engine
                            .focus()
                            .iter()
                            .map(|k| engine.config().keymap(*k).name.as_str())
                            .collect();
                        log_detail!(
                            "FOCUS: {} | {} (thread={} hwnd={:#x} cause={}) -> {:?}",
                            n.class,
                            n.title,
                            h.thread_id,
                            h.hwnd,
                            h.cause,
                            names
                        );
                    }
                }
            }
            Event::Quit(done) => {
                let releases = pressed.releases();
                if !releases.is_empty() {
                    log!("終了: 押下中のキーを離す {releases:?}");
                    platform::send(&releases);
                }
                let _ = done.send(());
                break;
            }
        }
    }
}

/// 起動できないときの終わり方。ログに書き、メッセージボックスにも出す
/// （常駐物なので、ログだけだと「何も起きない」に見える）。
fn fatal(msg: &str) -> ExitCode {
    log!("{msg}");
    // SAFETY: 文字列は NUL 終端の UTF-16（HSTRING）。親窓なし。
    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(msg),
            &HSTRING::from("newmayu: 起動できない"),
            MB_OK | MB_ICONERROR,
        );
    }
    ExitCode::FAILURE
}

fn main() -> ExitCode {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(e) => return fatal(&e),
    };
    if let Err(e) = log::init(args.log.as_deref()) {
        return fatal(&e);
    }
    // SAFETY: 引数なし。
    UI_THREAD.store(
        unsafe { windows::Win32::System::Threading::GetCurrentThreadId() },
        Ordering::Relaxed,
    );
    // ログダイアログ。隠したまま作り、ここからのログを溜める（yamy と同じく、見出しの後に設定を読む）
    log::start_view();
    log_dialog::banner(false);
    if let Err(e) = Ini::beside_exe().and_then(log_dialog::create) {
        return fatal(&e);
    }
    let (config, path) = match load_config(&args.symbols, args.test_mayu.as_deref()) {
        Ok(c) => c,
        Err(e) => return fatal(&e),
    };
    log!(
        "{}: キーマップ {} 個、割付 {} 件",
        path.display(),
        config.keymaps.len(),
        config
            .keymaps
            .iter()
            .map(|k| k.assignments.len())
            .sum::<usize>()
    );

    let (tx, rx) = channel::<Event>();
    TX.set(tx.clone()).expect("TX は一度だけ設定する");
    TEST_MARK
        .set(args.test_mark)
        .expect("TEST_MARK は一度だけ設定する");
    if let Some(m) = args.test_mark {
        log!("テスト入力モード: 印 {m:#x} の注入入力を物理入力として扱う");
    }

    // フック DLL からの通知を受けるメールスロット。DLL を入れる前に作る
    let mailslot = match Mailslot::create() {
        Ok(m) => m,
        Err(e) => return fatal(&e),
    };
    let notify_tx = tx.clone();
    thread::spawn(move || {
        while let Some(n) = mailslot.read() {
            if notify_tx.send(Event::Notify(n)).is_err() {
                break;
            }
        }
    });

    // &Sync の同期キーを DLL に知らせる共有メモリ。DLL を入れる前に作る
    let sync_share = match SyncShare::create(config.sync) {
        Ok(s) => s,
        Err(e) => return fatal(&e),
    };
    // 案内の窓（&HelpMessage）。専用のスレッドで窓を出す
    let help = match Help::start() {
        Ok(h) => h,
        Err(e) => return fatal(&e),
    };
    thread::spawn(move || engine_thread(config, rx, sync_share, help));

    // フック DLL を各プロセスに入れる
    let hook_dll = match HookDll::install() {
        Ok(h) => h,
        Err(e) => return fatal(&e),
    };

    // フック設置スレッド（= このスレッド）。メッセージループが要る。
    // SAFETY: hook_proc は WH_KEYBOARD_LL の規約どおりの関数。
    let hook = match unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), None, 0) } {
        Ok(h) => h,
        Err(e) => return fatal(&format!("フックを掛けられない: {e}")),
    };

    // トレイ。メニューの操作はこのスレッド（メッセージループ）で届く。
    let menu_tx = tx.clone();
    let symbols = args.symbols.clone();
    let test_mayu = args.test_mayu.clone();
    let tray = match Tray::new(
        "newmayu",
        Box::new(move |cmd| match cmd {
            Command::Reload => reload(&menu_tx, &symbols, test_mayu.as_deref()),
            Command::Select(i) => {
                match Ini::beside_exe() {
                    Ok(ini) => {
                        ini.set_index(i);
                    }
                    Err(e) => log!("{e}"),
                }
                reload(&menu_tx, &symbols, test_mayu.as_deref());
            }
            Command::Setting => match Ini::beside_exe() {
                Ok(ini) => {
                    if setting_dialog::run(&ini) {
                        reload(&menu_tx, &symbols, test_mayu.as_deref());
                    }
                }
                Err(e) => log!("{e}"),
            },
            Command::Log => log_dialog::show(),
            Command::Pause(p) => {
                PAUSED.store(p, Ordering::Relaxed);
                log!("{}", if p { "一時停止" } else { "再開" });
            }
            Command::Exit => {
                log!("終了");
                // SAFETY: メッセージループを回しているスレッドから呼ぶ。
                unsafe { PostQuitMessage(0) };
            }
        }),
    ) {
        Ok(t) => t,
        Err(e) => {
            // SAFETY: SetWindowsHookExW が返した有効なハンドル。
            let _ = unsafe { UnhookWindowsHookEx(hook) };
            return fatal(&e);
        }
    };
    log!("フック設置。トレイのメニューから再読込・一時停止・終了できる。");

    let mut msg = MSG::default();
    // SAFETY: msg は有効な MSG。
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
        // ログダイアログのキー操作（Tab など）はダイアログに任せる（yamy と同じ）
        if let Some(h) = log_dialog::hwnd()
            // SAFETY: h はこのスレッドで作ったダイアログ。msg は GetMessageW が埋めた MSG。
            && unsafe { IsDialogMessageW(h, &msg) }.as_bool()
        {
            continue;
        }
        // SAFETY: GetMessageW が埋めた MSG。
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    // 先にフックを外す。以後の入力はエンジンを通らない。
    // SAFETY: SetWindowsHookExW が返した有効なハンドル。
    let _ = unsafe { UnhookWindowsHookEx(hook) };
    // エンジンに押下中のキーを離させ、終わるのを待つ
    let (done_tx, done_rx) = channel();
    if tx.send(Event::Quit(done_tx)).is_ok() && done_rx.recv_timeout(QUIT_WAIT).is_err() {
        log!("エンジンスレッドが {QUIT_WAIT:?} 以内に終わらなかった");
    }
    drop(hook_dll);
    log_dialog::destroy();
    drop(tray);
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;
    use newmayu::model::ScanPrefix;

    fn a(v: &[&str]) -> Result<Args, String> {
        parse_args(v.iter().map(|s| s.to_string()))
    }

    fn args() -> Args {
        Args {
            symbols: vec![],
            log: None,
            test_mark: None,
            test_mayu: None,
        }
    }

    #[test]
    fn 引数なし() {
        assert_eq!(a(&[]), Ok(args()));
    }

    #[test]
    fn d_はシンボル() {
        assert_eq!(
            a(&["-DUSE109", "--log", "n.log", "-D"]),
            Ok(Args {
                symbols: vec!["USE109".to_string(), String::new()],
                log: Some(PathBuf::from("n.log")),
                ..args()
            })
        );
    }

    #[test]
    fn test_input_は16進の印() {
        let want = Ok(Args {
            test_mark: Some(0x6e6d),
            ..args()
        });
        assert_eq!(a(&["--test-input", "0x6e6d"]), want);
        assert_eq!(a(&["--test-input", "6E6D"]), want);
        assert_eq!(
            a(&[
                "--log",
                "n.log",
                "--test-input",
                "0x1",
                "--test-mayu",
                "k.mayu"
            ]),
            Ok(Args {
                log: Some(PathBuf::from("n.log")),
                test_mark: Some(1),
                test_mayu: Some(PathBuf::from("k.mayu")),
                ..args()
            })
        );
    }

    #[test]
    fn test_input_の形が違えば使い方() {
        assert_eq!(a(&["--test-input"]), Err(USAGE.to_string()));
        assert_eq!(a(&["--test-input", "0"]), Err(USAGE.to_string()));
        assert_eq!(a(&["--test-input", "0x"]), Err(USAGE.to_string()));
        assert_eq!(a(&["--test-input", "xyz"]), Err(USAGE.to_string()));
        assert_eq!(
            a(&["--test-input", "1", "--test-input", "2"]),
            Err(USAGE.to_string())
        );
    }

    #[test]
    fn 形が違えば使い方() {
        assert_eq!(a(&["--log"]), Err(USAGE.to_string()));
        assert_eq!(a(&["k.mayu"]), Err(USAGE.to_string()));
        assert_eq!(a(&["--foo"]), Err(USAGE.to_string()));
        assert_eq!(a(&["--test-mayu"]), Err(USAGE.to_string()));
        assert_eq!(a(&["--log", "a", "--log", "b"]), Err(USAGE.to_string()));
        assert_eq!(
            a(&["--test-mayu", "a", "--test-mayu", "b"]),
            Err(USAGE.to_string())
        );
    }

    #[test]
    fn 押したままのキーだけ離す() {
        let a = Key::new(ScanPrefix::None, 0x1e);
        let b = Key::new(ScanPrefix::None, 0x30);
        let mut p = Pressed::default();
        p.track(&[Output::Press(a), Output::Press(b), Output::Release(a)]);
        p.track(&[Output::Vk {
            vk: 0x41,
            extended: false,
            up: false,
        }]);
        let mut r = p.releases();
        r.sort_by_key(|o| format!("{o:?}"));
        assert_eq!(
            r,
            vec![
                Output::Release(b),
                Output::Vk {
                    vk: 0x41,
                    extended: false,
                    up: true
                }
            ]
        );
        p.track(&[
            Output::Release(b),
            Output::Vk {
                vk: 0x41,
                extended: false,
                up: true,
            },
        ]);
        assert!(p.releases().is_empty());
    }
}

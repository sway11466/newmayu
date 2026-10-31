//! 本体。フックを張り、`.mayu` の設定でキーを差し替え、タスクトレイに常駐する。
//!
//! ```text
//! newmayu <file.mayu> [--log <path>] [--test-input <印>]
//! ```
//!
//! `--log` があればログをそのファイルに追記する。無ければ標準エラー。
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
//! - 「一時停止」: フックのコールバックで何もせず次へ流す。もう一度で再開
//! - 「終了」: メッセージループを抜け、フックを外し、エンジンが押したままにしている
//!   キーを離し、フック DLL とアイコンを外して終わる

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;
use std::time::Duration;

use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, HC_ACTION, KBDLLHOOKSTRUCT, MB_ICONERROR, MB_OK,
    MSG, MessageBoxW, PostQuitMessage, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx,
    WH_KEYBOARD_LL,
};
use windows::core::HSTRING;

use newmayu::config::parser;
use newmayu::engine::{Engine, Input, Output};
use newmayu::log;
use newmayu::model::{Config, Key, LockMod};
use newmayu::notify::{HookDll, Mailslot};
use newmayu::platform;
use newmayu::ui::{Command, Tray};
use newmayu::window::WindowOps;
use newmayu_ipc::{
    CAPS_LOCK, IME_COMPOSING, IME_OPEN, KANA_LOCK, Kind, NUM_LOCK, Notify, SCROLL_LOCK,
};

/// 終了時、エンジンスレッドが押下中のキーを離し終えるのを待つ上限。
const QUIT_WAIT: Duration = Duration::from_secs(2);

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
    mayu: PathBuf,
    log: Option<PathBuf>,
    /// テスト入力モードの印。
    test_mark: Option<usize>,
}

const USAGE: &str = "使い方: newmayu <file.mayu> [--log <path>] [--test-input <印(16進)>]";

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
    let mut mayu = None;
    let mut log = None;
    let mut test_mark = None;
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
        } else if mayu.is_none() && !a.starts_with("--") {
            mayu = Some(PathBuf::from(a));
        } else {
            return Err(USAGE.to_string());
        }
    }
    let mayu = mayu.ok_or_else(|| USAGE.to_string())?;
    Ok(Args {
        mayu,
        log,
        test_mark,
    })
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
                Output::Func { .. } => {}
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

/// エンジンスレッドの本体。`Quit` で押下中のキーを離して抜ける。
fn engine_thread(config: Config, rx: Receiver<Event>) {
    let mut engine = Engine::new(config);
    let mut window_ops = WindowOps::new();
    let mut pressed = Pressed::default();
    for ev in rx {
        match ev {
            Event::Reload(config) => {
                // 前のエンジンが押したままのキーは離してから作り直す
                let releases = pressed.releases();
                if !releases.is_empty() {
                    log!("再読込: 押下中のキーを離す {releases:?}");
                    platform::send(&releases);
                }
                pressed = Pressed::default();
                engine = Engine::new(*config);
                log!("再読込: エンジンを作り直した");
            }
            Event::Key(input) => {
                let outputs = engine.feed(input);
                log!("{input:?} -> {outputs:?}");
                pressed.track(&outputs);
                platform::send(&outputs);
                window_ops.run(&outputs);
            }
            Event::Notify(n) => {
                let h = n.header;
                match h.kind {
                    Kind::Lock => {
                        log!(
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
                    Kind::Focus => {
                        engine.set_focus(h.hwnd, &n.class, &n.title);
                        let names: Vec<&str> = engine
                            .focus()
                            .iter()
                            .map(|k| engine.config().keymap(*k).name.as_str())
                            .collect();
                        log!(
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
    let path = args.mayu.display().to_string();
    let config = match parser::load(&args.mayu) {
        Ok(c) => c,
        Err(e) => return fatal(&e.to_string()),
    };
    log!(
        "{path}: キーマップ {} 個、割付 {} 件",
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

    thread::spawn(move || engine_thread(config, rx));

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
    let mayu = args.mayu.clone();
    let tray = match Tray::new(
        "newmayu",
        Box::new(move |cmd| match cmd {
            Command::Reload => match parser::load(&mayu) {
                Ok(c) => {
                    log!("再読込: {}", mayu.display());
                    let _ = menu_tx.send(Event::Reload(Box::new(c)));
                }
                Err(e) => log!("再読込に失敗。今の設定のまま: {e}"),
            },
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

    #[test]
    fn mayu_だけ() {
        assert_eq!(
            a(&["k.mayu"]),
            Ok(Args {
                mayu: PathBuf::from("k.mayu"),
                log: None,
                test_mark: None,
            })
        );
    }

    #[test]
    fn log_は前でも後でもよい() {
        let want = Ok(Args {
            mayu: PathBuf::from("k.mayu"),
            log: Some(PathBuf::from("n.log")),
            test_mark: None,
        });
        assert_eq!(a(&["k.mayu", "--log", "n.log"]), want);
        assert_eq!(a(&["--log", "n.log", "k.mayu"]), want);
    }

    #[test]
    fn test_input_は16進の印() {
        let want = Ok(Args {
            mayu: PathBuf::from("k.mayu"),
            log: None,
            test_mark: Some(0x6e6d),
        });
        assert_eq!(a(&["k.mayu", "--test-input", "0x6e6d"]), want);
        assert_eq!(a(&["--test-input", "6E6D", "k.mayu"]), want);
        assert_eq!(
            a(&["k.mayu", "--log", "n.log", "--test-input", "0x1"]),
            Ok(Args {
                mayu: PathBuf::from("k.mayu"),
                log: Some(PathBuf::from("n.log")),
                test_mark: Some(1),
            })
        );
    }

    #[test]
    fn test_input_の形が違えば使い方() {
        assert_eq!(a(&["k.mayu", "--test-input"]), Err(USAGE.to_string()));
        assert_eq!(a(&["k.mayu", "--test-input", "0"]), Err(USAGE.to_string()));
        assert_eq!(a(&["k.mayu", "--test-input", "0x"]), Err(USAGE.to_string()));
        assert_eq!(
            a(&["k.mayu", "--test-input", "xyz"]),
            Err(USAGE.to_string())
        );
        assert_eq!(
            a(&["k.mayu", "--test-input", "1", "--test-input", "2"]),
            Err(USAGE.to_string())
        );
    }

    #[test]
    fn 形が違えば使い方() {
        assert_eq!(a(&[]), Err(USAGE.to_string()));
        assert_eq!(a(&["--log"]), Err(USAGE.to_string()));
        assert_eq!(a(&["k.mayu", "--log"]), Err(USAGE.to_string()));
        assert_eq!(a(&["a.mayu", "b.mayu"]), Err(USAGE.to_string()));
        assert_eq!(a(&["--foo", "a.mayu"]), Err(USAGE.to_string()));
        assert_eq!(
            a(&["k.mayu", "--log", "a", "--log", "b"]),
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

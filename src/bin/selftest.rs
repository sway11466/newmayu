//! 実機テスト。人がキーを押さずに、フック → エンジン → `SendInput` の通しを
//! 「A を押すと B が出る」の形で確かめる。
//!
//! ```text
//! cargo run --bin selftest
//! ```
//!
//! やること:
//! 1. 一時フォルダに小さな `.mayu` を書く（`key A = B`、`key C-H = BackSpace`）
//! 2. 同じフォルダの `newmayu.exe` を `--test-input <印>` 付きの子プロセスで起動し、
//!    ログに「フック設置」が出るまで待つ
//! 3. 自分も `WH_KEYBOARD_LL` を掛け、届いたイベントのうち「注入で、印が付いていないもの」
//!    （= newmayu の出力）だけをスキャンコードと上下で記録する。印付き（自分の注入）と
//!    物理入力は記録しない
//! 4. シナリオごとに印付きの入力を `SendInput` で流し、記録と期待を比べる
//! 5. 結果を 1 シナリオ 1 行で出し、全部通れば終了コード 0、違えば 1。子プロセスは
//!    `TerminateProcess` で終える
//!
//! デスクトップセッションが要るので CI では動かさず、手元で実行する。
//! 実行中は newmayu が本当にフックを掛けているので、物理キーボードもテスト用の
//! `.mayu` で変換される（A を押すと B が出る）。終わるまでキーを触らないこと。

use std::fs;
use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, ExitCode};
use std::sync::mpsc::channel;
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP,
    KEYEVENTF_SCANCODE, SendInput, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_EXTENDED,
    LLKHF_INJECTED, LLKHF_UP, MSG, PostThreadMessageW, SetWindowsHookExW, TranslateMessage,
    UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_QUIT,
};

/// テスト入力の印。`dwExtraInfo` に入れ、newmayu には `--test-input` で渡す。
const MARK: usize = 0x6e6d;

/// newmayu がフックを掛けるまで待つ上限。
const HOOK_WAIT: Duration = Duration::from_secs(10);
/// 1 シナリオの出力が揃うのを待つ上限。
const OUTPUT_WAIT: Duration = Duration::from_secs(2);
/// 期待した数が揃った後、余計な出力が無いか見るために待つ時間。
const SETTLE: Duration = Duration::from_millis(200);
/// 注入 1 件ごとの間隔。
const KEY_INTERVAL: Duration = Duration::from_millis(20);

/// テスト用 `.mayu` のキー定義。
struct KeyDef {
    name: &'static str,
    code: u8,
    e0: bool,
}

const KEYS: &[KeyDef] = &[
    KeyDef {
        name: "A",
        code: 0x1e,
        e0: false,
    },
    KeyDef {
        name: "B",
        code: 0x30,
        e0: false,
    },
    KeyDef {
        name: "H",
        code: 0x23,
        e0: false,
    },
    KeyDef {
        name: "BackSpace",
        code: 0x0e,
        e0: false,
    },
    KeyDef {
        name: "Space",
        code: 0x39,
        e0: false,
    },
    KeyDef {
        name: "LControl",
        code: 0x1d,
        e0: false,
    },
    KeyDef {
        name: "RControl",
        code: 0x1d,
        e0: true,
    },
];

/// `.mayu` の割付部分。
const ASSIGNMENTS: &str = "
def mod control = LControl RControl
key A = B
key C-H = BackSpace
";

/// 1 イベント。`up` は離し。
#[derive(Clone, Copy, PartialEq, Eq)]
struct Ev {
    name: &'static str,
    up: bool,
}

const fn down(name: &'static str) -> Ev {
    Ev { name, up: false }
}
const fn up(name: &'static str) -> Ev {
    Ev { name, up: true }
}

/// 流す列と期待する出力列の組。
struct Scenario {
    name: &'static str,
    send: &'static [Ev],
    expect: &'static [Ev],
}

const SCENARIOS: &[Scenario] = &[
    Scenario {
        name: "A 押し離し → B 押し離し",
        send: &[down("A"), up("A")],
        expect: &[down("B"), up("B")],
    },
    Scenario {
        // 修飾キー自身は素通し。BackSpace は修飾なしで出すので OS 側の Control を先に離す。
        // 離すときの Control は既に離れているので何も出ない（engine::tests::control_h_to_backspace）
        name: "LControl 押し、H 押し離し、LControl 離し → LControl 押し離し、BackSpace 押し離し",
        send: &[down("LControl"), down("H"), up("H"), up("LControl")],
        expect: &[
            down("LControl"),
            up("LControl"),
            down("BackSpace"),
            up("BackSpace"),
        ],
    },
    Scenario {
        name: "割付の無い Space はそのまま",
        send: &[down("Space"), up("Space")],
        expect: &[down("Space"), up("Space")],
    },
    Scenario {
        name: "修飾の無い H はそのまま",
        send: &[down("H"), up("H")],
        expect: &[down("H"), up("H")],
    },
];

/// フックが記録した newmayu の出力（スキャンコード, E0, 離し）。
static RECORDED: OnceLock<Mutex<Vec<(u8, bool, bool)>>> = OnceLock::new();

fn recorded() -> &'static Mutex<Vec<(u8, bool, bool)>> {
    RECORDED.get_or_init(|| Mutex::new(Vec::new()))
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        // SAFETY: WH_KEYBOARD_LL では lparam は KBDLLHOOKSTRUCT を指す。
        let k = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        if k.flags.contains(LLKHF_INJECTED) && k.dwExtraInfo != MARK {
            recorded().lock().unwrap().push((
                (k.scanCode & 0xff) as u8,
                k.flags.contains(LLKHF_EXTENDED),
                k.flags.contains(LLKHF_UP),
            ));
        }
    }
    // SAFETY: 引数はそのまま次のフックへ渡す。
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// フックを掛けてメッセージループを回すスレッドを起こす。返すのはそのスレッド ID
/// （`WM_QUIT` を送って止める）。
fn start_hook_thread() -> Result<u32, String> {
    let (tx, rx) = channel();
    thread::spawn(move || {
        // SAFETY: hook_proc は WH_KEYBOARD_LL の規約どおりの関数。
        let hook = match unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), None, 0) } {
            Ok(h) => h,
            Err(e) => {
                let _ = tx.send(Err(format!("フックを掛けられない: {e}")));
                return;
            }
        };
        // SAFETY: 単純な API。
        let _ = tx.send(Ok(unsafe { GetCurrentThreadId() }));
        let mut msg = MSG::default();
        // SAFETY: msg は有効な MSG。
        while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
            // SAFETY: GetMessageW が埋めた MSG。
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        // SAFETY: SetWindowsHookExW が返した有効なハンドル。
        let _ = unsafe { UnhookWindowsHookEx(hook) };
    });
    rx.recv()
        .map_err(|_| "フックスレッドが落ちた".to_string())?
}

fn key_def(name: &str) -> &'static KeyDef {
    KEYS.iter()
        .find(|k| k.name == name)
        .unwrap_or_else(|| panic!("KEYS に無いキー名: {name}"))
}

/// 印付きの入力を 1 件注入する。
fn inject(ev: Ev) {
    let def = key_def(ev.name);
    let mut flags = KEYEVENTF_SCANCODE;
    if def.e0 {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    if ev.up {
        flags |= KEYEVENTF_KEYUP;
    }
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: def.code as u16,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: MARK,
            },
        },
    };
    // SAFETY: 有効な INPUT 1 件で、サイズも正しく渡している。
    let sent = unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) };
    if sent != 1 {
        eprintln!(
            "SendInput に失敗: {}{}",
            ev.name,
            if ev.up { "↑" } else { "↓" }
        );
    }
}

/// 記録をキー名に直す。定義に無いコードは `0x..` のまま。
fn describe(rec: &[(u8, bool, bool)]) -> Vec<String> {
    rec.iter()
        .map(|&(code, e0, up)| {
            let name = KEYS
                .iter()
                .find(|k| k.code == code && k.e0 == e0)
                .map(|k| k.name.to_string())
                .unwrap_or_else(|| format!("{}{code:#04x}", if e0 { "E0-" } else { "" }));
            format!("{name}{}", if up { "↑" } else { "↓" })
        })
        .collect()
}

fn describe_expected(evs: &[Ev]) -> Vec<String> {
    evs.iter()
        .map(|e| format!("{}{}", e.name, if e.up { "↑" } else { "↓" }))
        .collect()
}

/// 1 シナリオを流して比べる。通れば `Ok(())`、違えば `Err(説明)`。
fn run_scenario(s: &Scenario) -> Result<(), String> {
    recorded().lock().unwrap().clear();
    for ev in s.send {
        inject(*ev);
        thread::sleep(KEY_INTERVAL);
    }
    let start = Instant::now();
    while recorded().lock().unwrap().len() < s.expect.len() && start.elapsed() < OUTPUT_WAIT {
        thread::sleep(Duration::from_millis(10));
    }
    thread::sleep(SETTLE);
    let got = describe(&recorded().lock().unwrap());
    let want = describe_expected(s.expect);
    if got == want {
        Ok(())
    } else {
        Err(format!("期待 {want:?} 実際 {got:?}"))
    }
}

/// テスト用 `.mayu` の中身。
fn mayu_source() -> String {
    let mut s = String::new();
    for k in KEYS {
        s.push_str(&format!(
            "def key {} = {}{:#04x}\n",
            k.name,
            if k.e0 { "E0-" } else { "" },
            k.code
        ));
    }
    s.push_str(ASSIGNMENTS);
    s
}

/// 子プロセス。落ちても必ず終える。
struct Newmayu(Child);

impl Drop for Newmayu {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// newmayu を起動し、ログに「フック設置」が出るまで待つ。
fn start_newmayu(dir: &Path, mayu: &Path) -> Result<Newmayu, String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("自分のパスが分からない: {e}"))?
        .with_file_name("newmayu.exe");
    if !exe.exists() {
        return Err(format!("{} が無い。先に cargo build する", exe.display()));
    }
    let log = dir.join("newmayu.log");
    let child = Command::new(&exe)
        .arg(mayu)
        .arg("--log")
        .arg(&log)
        .arg("--test-input")
        .arg(format!("{MARK:#x}"))
        .spawn()
        .map_err(|e| format!("{} を起動できない: {e}", exe.display()))?;
    let mut child = Newmayu(child);
    let start = Instant::now();
    loop {
        if let Some(status) = child.0.try_wait().map_err(|e| e.to_string())? {
            let mut body = String::new();
            if let Ok(mut f) = fs::File::open(&log) {
                let _ = f.read_to_string(&mut body);
            }
            return Err(format!("newmayu が先に終わった ({status})。ログ:\n{body}"));
        }
        let mut body = String::new();
        if let Ok(mut f) = fs::File::open(&log) {
            let _ = f.read_to_string(&mut body);
        }
        if body.contains("フック設置") {
            return Ok(child);
        }
        if start.elapsed() > HOOK_WAIT {
            return Err(format!(
                "{HOOK_WAIT:?} 待っても newmayu がフックを掛けない。ログ:\n{body}"
            ));
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn run() -> Result<bool, String> {
    let dir = std::env::temp_dir().join(format!("newmayu-selftest-{}", std::process::id()));
    fs::create_dir_all(&dir).map_err(|e| format!("{} を作れない: {e}", dir.display()))?;
    let mayu = dir.join("selftest.mayu");
    fs::write(&mayu, mayu_source()).map_err(|e| format!("{} を書けない: {e}", mayu.display()))?;

    // 記録側のフックを先に掛ける。後から掛けた newmayu のフックが先に呼ばれるので、
    // 自分の注入は newmayu が抑制し、ここには newmayu の出力だけが届く
    let hook_thread = start_hook_thread()?;
    let result = (|| {
        let child = start_newmayu(&dir, &mayu)?;
        // 設置直後の取りこぼしを避けるため少し置く
        thread::sleep(Duration::from_millis(300));
        let mut all_ok = true;
        for s in SCENARIOS {
            match run_scenario(s) {
                Ok(()) => println!("ok  {}", s.name),
                Err(e) => {
                    all_ok = false;
                    println!("NG  {}: {e}", s.name);
                }
            }
        }
        drop(child);
        Ok(all_ok)
    })();
    // SAFETY: start_hook_thread が返した生きているスレッドの ID。
    let _ = unsafe { PostThreadMessageW(hook_thread, WM_QUIT, WPARAM(0), LPARAM(0)) };
    let _ = fs::remove_dir_all(&dir);
    result
}

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

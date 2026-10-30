//! PoC: フックを張り、`.mayu` の設定でキーを差し替える。
//!
//! ```text
//! newmayu <file.mayu>
//! ```
//!
//! 保険（PoC 専用）:
//! - 起動から 60 秒で必ず終了する
//! - Esc の物理押下はフックのコールバック内で検知し、エンジンを通さず即終了する
//!
//! 構成は yamy と同じ。コールバックはイベントをキューに積んで `1` を返し（抑制）、
//! 別スレッドのエンジンが処理して `SendInput` する。
//! フック DLL（`newmayu_hook.dll`、同じフォルダ）が各プロセスから送る IME の状態も
//! 同じキューに積み、エンジンに渡す。

use std::path::Path;
use std::process::ExitCode;
use std::sync::OnceLock;
use std::sync::mpsc::{Sender, channel};
use std::thread;
use std::time::Duration;

use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG,
    SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL,
};

use newmayu::config::parser;
use newmayu::engine::{Engine, Input};
use newmayu::model::LockMod;
use newmayu::notify::{HookDll, Mailslot};
use newmayu::platform;
use newmayu::window::WindowOps;
use newmayu_ipc::{
    CAPS_LOCK, IME_COMPOSING, IME_OPEN, KANA_LOCK, Kind, NUM_LOCK, Notify, SCROLL_LOCK,
};

const AUTO_EXIT_SECS: u64 = 60;
const ESC_SCAN: u32 = 0x01;

/// エンジンスレッドに届くもの。
enum Event {
    Key(Input),
    /// フック DLL からの通知。
    Notify(Notify),
}

/// フックのコールバックからエンジンスレッドへ渡すキュー。
/// コールバックは C の関数でユーザーポインタを受け取れないため static に置く。
static TX: OnceLock<Sender<Event>> = OnceLock::new();

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        // SAFETY: WH_KEYBOARD_LL では lparam は KBDLLHOOKSTRUCT を指す。
        let k = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };

        // 保険: 物理 Esc で即終了。プロセスが消えればフックも外れる。
        if k.scanCode == ESC_SCAN && !k.flags.contains(LLKHF_INJECTED) {
            eprintln!("Esc: 終了");
            std::process::exit(0);
        }

        if let Some(input) = platform::input_from_hook(k) {
            if let Some(tx) = TX.get() {
                let _ = tx.send(Event::Key(input));
            }
            return LRESULT(1);
        }
    }
    // SAFETY: 引数はそのまま次のフックへ渡す。
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

fn main() -> ExitCode {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("使い方: newmayu <file.mayu>");
        return ExitCode::FAILURE;
    };
    let config = match parser::load(Path::new(&path)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    eprintln!(
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

    // フック DLL からの通知を受けるメールスロット。DLL を入れる前に作る
    let mailslot = match Mailslot::create() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    thread::spawn(move || {
        while let Some(n) = mailslot.read() {
            if tx.send(Event::Notify(n)).is_err() {
                break;
            }
        }
    });

    // 保険: 自動終了
    thread::spawn(|| {
        thread::sleep(Duration::from_secs(AUTO_EXIT_SECS));
        eprintln!("{AUTO_EXIT_SECS} 秒経過: 終了");
        std::process::exit(0);
    });

    // エンジンスレッド
    thread::spawn(move || {
        let mut engine = Engine::new(config);
        let mut window_ops = WindowOps::new();
        for ev in rx {
            match ev {
                Event::Key(input) => {
                    let outputs = engine.feed(input);
                    eprintln!("{input:?} -> {outputs:?}");
                    platform::send(&outputs);
                    window_ops.run(&outputs);
                }
                Event::Notify(n) => {
                    let h = n.header;
                    match h.kind {
                        Kind::Lock => {
                            eprintln!(
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
                            eprintln!(
                                "FOCUS: {} | {} (thread={} hwnd={:#x} cause={}) -> {:?}",
                                n.class, n.title, h.thread_id, h.hwnd, h.cause, names
                            );
                        }
                    }
                }
            }
        }
    });

    // フック DLL を各プロセスに入れる
    let hook_dll = match HookDll::install() {
        Ok(h) => h,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };

    // フック設置スレッド（= このスレッド）。メッセージループが要る。
    // SAFETY: hook_proc は WH_KEYBOARD_LL の規約どおりの関数。
    let hook = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), None, 0) }
        .expect("SetWindowsHookEx に失敗");
    eprintln!("フック設置。Esc で即終了、{AUTO_EXIT_SECS} 秒で自動終了。");

    let mut msg = MSG::default();
    // SAFETY: msg は有効な MSG。
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {}

    // SAFETY: SetWindowsHookExW が返した有効なハンドル。
    let _ = unsafe { UnhookWindowsHookEx(hook) };
    drop(hook_dll);
    ExitCode::SUCCESS
}

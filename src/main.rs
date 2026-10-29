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
use newmayu::platform;

const AUTO_EXIT_SECS: u64 = 60;
const ESC_SCAN: u32 = 0x01;

/// フックのコールバックからエンジンスレッドへ渡すキュー。
/// コールバックは C の関数でユーザーポインタを受け取れないため static に置く。
static TX: OnceLock<Sender<Input>> = OnceLock::new();

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
                let _ = tx.send(input);
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

    let (tx, rx) = channel::<Input>();
    TX.set(tx).expect("TX は一度だけ設定する");

    // 保険: 自動終了
    thread::spawn(|| {
        thread::sleep(Duration::from_secs(AUTO_EXIT_SECS));
        eprintln!("{AUTO_EXIT_SECS} 秒経過: 終了");
        std::process::exit(0);
    });

    // エンジンスレッド
    thread::spawn(move || {
        let mut engine = Engine::new(config);
        for input in rx {
            let outputs = engine.feed(input);
            eprintln!("{input:?} -> {outputs:?}");
            platform::send(&outputs);
        }
    });

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
    ExitCode::SUCCESS
}

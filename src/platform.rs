//! Win32 との結線。`unsafe` はこのモジュールに閉じ込める。
//!
//! - フックから届く `KBDLLHOOKSTRUCT` を [`Input`] に直す
//! - [`Output`] を `SendInput` に流す

use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, SendInput, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    KBDLLHOOKSTRUCT, LLKHF_EXTENDED, LLKHF_INJECTED, LLKHF_UP,
};

use crate::engine::{Input, Output};
use crate::model::{Edge, Key, ScanPrefix};

/// フックの 1 イベントをエンジンの入力に直す。
///
/// 自分（や他のツール）が `SendInput` したイベントは `LLKHF_INJECTED` が立つので
/// `None` を返し、呼び元はそのまま次のフックへ流す（yamy と同じ自己ループ回避）。
/// E1 前置（Pause）はまだ扱わない。
pub fn input_from_hook(k: &KBDLLHOOKSTRUCT) -> Option<Input> {
    if k.flags.contains(LLKHF_INJECTED) {
        return None;
    }
    let prefix = if k.flags.contains(LLKHF_EXTENDED) {
        ScanPrefix::E0
    } else {
        ScanPrefix::None
    };
    let code = u8::try_from(k.scanCode & 0xff).ok()?;
    let edge = if k.flags.contains(LLKHF_UP) {
        Edge::Up
    } else {
        Edge::Down
    };
    Some(Input {
        key: Key::new(prefix, code),
        edge,
    })
}

fn keybd_input(key: Key, up: bool) -> Option<INPUT> {
    let mut flags: KEYBD_EVENT_FLAGS = KEYEVENTF_SCANCODE;
    match key.prefix {
        ScanPrefix::None => {}
        ScanPrefix::E0 => flags |= KEYEVENTF_EXTENDEDKEY,
        ScanPrefix::E1 => return None,
    }
    if up {
        flags |= KEYEVENTF_KEYUP;
    }
    Some(INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: key.code as u16,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    })
}

/// エンジンの出力をまとめて OS に注入する。
pub fn send(outputs: &[Output]) {
    let inputs: Vec<INPUT> = outputs
        .iter()
        .filter_map(|o| match o {
            Output::Press(k) => keybd_input(*k, false),
            Output::Release(k) => keybd_input(*k, true),
            Output::Func { .. } => None,
        })
        .collect();
    if inputs.is_empty() {
        return;
    }
    // SAFETY: `inputs` は有効な INPUT 配列で、サイズも正しく渡している。
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent as usize != inputs.len() {
        eprintln!("SendInput: {} / {} 件しか送れなかった", sent, inputs.len());
    }
}

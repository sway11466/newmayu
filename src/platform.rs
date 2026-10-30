//! Win32 との結線。`unsafe` はこのモジュールに閉じ込める。
//!
//! - フックから届く `KBDLLHOOKSTRUCT` を [`Input`] に直す
//! - [`Output`] を `SendInput` に流す

use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MAPVK_VK_TO_VSC,
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
    MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT,
    MapVirtualKeyW, SendInput, VIRTUAL_KEY, VK_LBUTTON, VK_MBUTTON, VK_RBUTTON, VK_XBUTTON1,
    VK_XBUTTON2,
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

/// `&VK` の出力。マウスボタンはマウス入力、それ以外は仮想キーコードのキー入力
/// （スキャンコードは `MapVirtualKey` で補う。yamy の `funcVK` と同じ）。
fn vk_input(vk: u8, extended: bool, up: bool) -> INPUT {
    let button = [
        (VK_LBUTTON, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, 0),
        (VK_RBUTTON, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, 0),
        (VK_MBUTTON, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, 0),
        (VK_XBUTTON1, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, 1),
        (VK_XBUTTON2, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, 2),
    ]
    .into_iter()
    .find(|(v, _, _, _)| v.0 == vk as u16);
    if let Some((_, down_flag, up_flag, xbutton)) = button {
        return INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx: 0,
                    dy: 0,
                    mouseData: xbutton,
                    dwFlags: if up { up_flag } else { down_flag },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
    }
    let mut flags = KEYBD_EVENT_FLAGS(0);
    if extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    if up {
        flags |= KEYEVENTF_KEYUP;
    }
    // SAFETY: 単純な変換 API。
    let scan = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC) } as u16;
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk as u16),
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// エンジンの出力をまとめて OS に注入する。
pub fn send(outputs: &[Output]) {
    let inputs: Vec<INPUT> = outputs
        .iter()
        .filter_map(|o| match o {
            Output::Press(k) => keybd_input(*k, false),
            Output::Release(k) => keybd_input(*k, true),
            Output::Vk { vk, extended, up } => Some(vk_input(*vk, *extended, *up)),
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

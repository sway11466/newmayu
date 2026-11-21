//! 調査用の `&関数` の Win32 側（yamy の `funcWindowIdentify`、mayu.cpp の `Type_name`、
//! engine.h の `commandNotify`）。結果はログに出す（ログダイアログで見る）。
//!
//! - `&WindowIdentify`: フォーカス窓がコンソールならここで名前と位置を出す。それ以外はその窓に命令を送り、
//!   フック DLL が返す名前（[`Kind::Name`](newmayu_ipc::Kind::Name)）を [`log_name`] で出す
//! - `&InvestigateCommand` の間: フック DLL が知らせる `WM_COMMAND` / `WM_SYSCOMMAND` を、
//!   [`log_command`] で `&PostMessage(…)` の形にして出す

use newmayu_ipc::COMMAND_NOTIFY_NAME;
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_STYLE, GetClassNameW, GetParent, GetWindowLongPtrW, GetWindowRect, GetWindowTextW,
    GetWindowThreadProcessId, SPI_GETWORKAREA, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
    SystemParametersInfoW, WS_CHILD,
};

use crate::window::{post_command, toplevel};
use crate::{log, log_detail};

const WM_COMMAND: u32 = 0x0111;
const WM_SYSCOMMAND: u32 = 0x0112;

fn hwnd_of(h: u64) -> HWND {
    HWND(h as usize as *mut _)
}

fn class_name(hwnd: HWND) -> Option<String> {
    let mut buf = [0u16; 256];
    // SAFETY: 書き込み先は配列。
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    (n > 0).then(|| String::from_utf16_lossy(&buf[..n as usize]))
}

fn window_text(hwnd: HWND) -> String {
    let mut buf = [0u16; 1024];
    // SAFETY: 書き込み先は配列。
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) }.max(0) as usize;
    String::from_utf16_lossy(&buf[..n])
}

/// 位置と大きさの行（yamy と同じ形）。
fn rect_line(what: &str, rc: &RECT) -> String {
    format!(
        "{what} Window Position/Size: ({}, {}) / ({}x{})",
        rc.left,
        rc.top,
        rc.right - rc.left,
        rc.bottom - rc.top
    )
}

fn window_rect(hwnd: HWND) -> RECT {
    let mut rc = RECT::default();
    // SAFETY: rc は書ける RECT。無効な窓なら失敗して 0 のまま。
    let _ = unsafe { GetWindowRect(hwnd, &mut rc) };
    rc
}

/// 親のクライアント座標での位置（yamy の `getChildWindowRect`）。
fn child_rect(hwnd: HWND) -> RECT {
    let mut rc = window_rect(hwnd);
    // SAFETY: 窓ハンドルを渡すだけ。親が無ければ失敗する。
    if let Ok(parent) = unsafe { GetParent(hwnd) } {
        let mut a = POINT {
            x: rc.left,
            y: rc.top,
        };
        let mut b = POINT {
            x: rc.right,
            y: rc.bottom,
        };
        // SAFETY: parent は窓、a と b は書ける POINT。
        unsafe {
            let _ = ScreenToClient(parent, &mut a);
            let _ = ScreenToClient(parent, &mut b);
        }
        rc = RECT {
            left: a.x,
            top: a.y,
            right: b.x,
            bottom: b.y,
        };
    }
    rc
}

/// 主モニターの作業領域。
fn work_area() -> RECT {
    let mut rc = RECT::default();
    // SAFETY: rc は書ける RECT。
    let _ = unsafe {
        SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(&mut rc as *mut RECT as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    rc
}

/// `&WindowIdentify`（yamy の `funcWindowIdentify`）。
pub fn window_identify(focus: HWND) {
    if let Some(class) = class_name(focus)
        && class.eq_ignore_ascii_case("ConsoleWindowClass")
    {
        // コンソールにはフック DLL が入らないので、ここで出す
        let title = window_text(focus);
        log_detail!("HWND:\t{:x}", focus.0 as usize as u32);
        log!("CLASS:\t{class}");
        log!("TITLE:\t{title}");
        if let (Some(top), _) = toplevel(focus, false) {
            log!("{}", rect_line("Toplevel", &window_rect(top)));
        }
        log!("{}", rect_line("Desktop", &work_area()));
        log!("");
        return;
    }
    post_command(focus, COMMAND_NOTIFY_NAME);
}

/// フック DLL が返した窓の名前（yamy の mayu.cpp の `Type_name`）。
pub fn log_name(hwnd: u64, thread_id: u32, class: &str, title: &str) {
    log_detail!("HWND:\t{:x}", hwnd as u32);
    log_detail!("THREADID:{thread_id}");
    log!("CLASS:\t{class}");
    log!("TITLE:\t{title}");
    let (found, is_mdi) = toplevel(hwnd_of(hwnd), true);
    let mut top = found;
    if is_mdi && let Some(mdi) = found {
        log!("{}", rect_line("MDI", &child_rect(mdi)));
        top = toplevel(hwnd_of(hwnd), false).0;
    }
    let rc = top.map(window_rect).unwrap_or_default();
    log!("{}", rect_line("Toplevel", &rc));
    log!("{}", rect_line("Desktop", &work_area()));
    log!("");
}

/// 窓に届いた `WM_COMMAND` / `WM_SYSCOMMAND` を、`&PostMessage` の形でログに出す（yamy の `commandNotify`）。
/// `focus` はいまのフォーカス窓、`ui_thread` は newmayu の窓のスレッド（newmayu 自身の分は出さない）。
pub fn log_command(focus: u64, ui_thread: u32, hwnd: u64, message: u32, wparam: u64, lparam: u64) {
    if focus == 0 {
        return;
    }
    let hf = hwnd_of(focus);
    // SAFETY: 窓ハンドルを渡すだけ。
    if unsafe { GetWindowThreadProcessId(hf, None) } == ui_thread {
        return;
    }
    let parent = |h: HWND| {
        // SAFETY: 窓ハンドルを渡すだけ。親が無ければ失敗する。
        unsafe { GetParent(h) }.unwrap_or_default()
    };
    let main = {
        let mut h = hf;
        loop {
            let p = parent(h);
            if p.0.is_null() {
                break h;
            }
            h = p;
        }
    };
    let overlapped = {
        let mut h = hf;
        while !h.0.is_null() {
            // SAFETY: 窓ハンドルを渡すだけ。
            let style = unsafe { GetWindowLongPtrW(h, GWL_STYLE) } as u32;
            if style & WS_CHILD.0 == 0 {
                break;
            }
            h = parent(h);
        }
        h
    };
    let as_u64 = |h: HWND| h.0 as usize as u64;
    // フォーカス窓から親をたどった列（番号の宛先用）
    let mut chain = Vec::new();
    let mut h = hf;
    while !h.0.is_null() && chain.len() < MAX_DEPTH {
        chain.push(as_u64(h));
        h = parent(h);
    }
    let Some(target) = command_target(
        hwnd,
        focus,
        as_u64(parent(hf)),
        as_u64(main),
        as_u64(overlapped),
        &chain,
    ) else {
        return;
    };
    log!("{}", command_line(&target, message, wparam, lparam, hwnd));
}

/// 親をたどる深さの上限。
const MAX_DEPTH: usize = 64;

/// 宛先。名前の付いた宛先でなければ、フォーカス窓から何段目の親か（`chain` はフォーカス窓から親をたどった列）。
/// どれでもなければ `None`。
///
/// yamy は番号を出す処理の直後で必ず抜けるので、番号の宛先は出ない。バグと想定し、番号を出すようにした。
fn command_target(
    hwnd: u64,
    focus: u64,
    parent: u64,
    main: u64,
    overlapped: u64,
    chain: &[u64],
) -> Option<String> {
    if hwnd == focus {
        Some("ToItself".to_string())
    } else if hwnd == parent {
        Some("ToParentWindow".to_string())
    } else if hwnd == main {
        Some("ToMainWindow".to_string())
    } else if hwnd == overlapped {
        Some("ToOverlappedWindow".to_string())
    } else {
        chain.iter().position(|&h| h == hwnd).map(|n| n.to_string())
    }
}

/// yamy と同じ形の 1 行。hwnd は yamy と同じく 32 ビットの 16 進。
fn command_line(target: &str, message: u32, wparam: u64, lparam: u64, hwnd: u64) -> String {
    let name = match message {
        WM_COMMAND => "WM_COMMAND".to_string(),
        WM_SYSCOMMAND => "WM_SYSCOMMAND".to_string(),
        m => m.to_string(),
    };
    format!(
        "&PostMessage({target}, {message}, 0x{wparam:x}, 0x{lparam:x}) # hwnd = {:x}, message = {name}, \
         wNotifyCode = {}, wID = {}, hwndCtrl = 0x{lparam:x}",
        hwnd as u32,
        (wparam >> 16) & 0xffff,
        wparam & 0xffff
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_target_follows_yamy_order() {
        // 自分 → 親 → 一番上 → 子窓でない窓の順に見る
        let t = |hwnd| command_target(hwnd, 1, 2, 9, 4, &[1, 2, 3, 4, 5, 9]);
        assert_eq!(t(1).as_deref(), Some("ToItself"));
        assert_eq!(t(2).as_deref(), Some("ToParentWindow"));
        assert_eq!(t(9).as_deref(), Some("ToMainWindow"));
        assert_eq!(t(4).as_deref(), Some("ToOverlappedWindow"));
        // どれでもなければ何段目の親か（yamy は出さない。バグとして直した）
        assert_eq!(t(3).as_deref(), Some("2"));
        assert_eq!(t(5).as_deref(), Some("4"));
        // 親の列に無ければ出さない
        assert_eq!(t(7), None);
    }

    #[test]
    fn command_line_is_like_yamy() {
        assert_eq!(
            command_line("ToMainWindow", 0x111, 0x0001_9c41, 0x2_0a3c, 0x1_0000_0abc),
            "&PostMessage(ToMainWindow, 273, 0x19c41, 0x20a3c) # hwnd = abc, message = WM_COMMAND, \
             wNotifyCode = 1, wID = 40001, hwndCtrl = 0x20a3c"
        );
        assert!(
            command_line("ToItself", 0x112, 0xf060, 0, 1).contains("message = WM_SYSCOMMAND, ")
        );
        assert!(command_line("ToItself", 5, 0, 0, 1).contains("message = 5, "));
    }

    #[test]
    fn rect_line_is_like_yamy() {
        let rc = RECT {
            left: -8,
            top: 10,
            right: 100,
            bottom: 60,
        };
        assert_eq!(
            rect_line("Toplevel", &rc),
            "Toplevel Window Position/Size: (-8, 10) / (108x50)"
        );
    }
}

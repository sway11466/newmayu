//! 窓を操作する `&関数`（`&WindowMove` など）と `&MouseMove`・`&MouseWheel`・`&ShellExecute` の Win32 側。
//! yamy の function.cpp の funcWindow* と windowstool.cpp を写している。
//!
//! エンジンは OS を触らず、[`Output::Func`] に「フォーカス窓」と「何をするか」を乗せて渡す。
//! ここがそれを受け、対象の窓（最上位の窓、または MDI 子窓）を探して動かす。

use newmayu_ipc::{COMMAND_MESSAGE_NAME, COMMAND_RECENTER};
use windows::Win32::Foundation::{COLORREF, HANDLE, HGLOBAL, HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow, RDW_ALLCHILDREN,
    RDW_ERASE, RDW_FRAME, RDW_INVALIDATE, RedrawWindow, ScreenToClient,
};
use windows::Win32::System::Com::{
    COINIT, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Diagnostics::Debug::MessageBeep;
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_WHEEL, MOUSEINPUT, SendInput,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GWL_STYLE, GetClientRect, GetCursorPos, GetParent, GetWindowLongPtrW,
    GetWindowRect, HWND_BOTTOM, HWND_NOTOPMOST, HWND_TOP, HWND_TOPMOST, IsIconic, IsWindow,
    IsZoomed, LWA_ALPHA, MB_OK, PostMessageW, RegisterWindowMessageW, SC_CLOSE, SC_MAXIMIZE,
    SC_MINIMIZE, SC_RESTORE, SHOW_WINDOW_CMD, SW_HIDE, SW_MAXIMIZE, SW_MINIMIZE, SW_RESTORE,
    SW_SHOW, SW_SHOWDEFAULT, SW_SHOWMAXIMIZED, SW_SHOWMINIMIZED, SW_SHOWMINNOACTIVE, SW_SHOWNA,
    SW_SHOWNOACTIVATE, SW_SHOWNORMAL, SWP_ASYNCWINDOWPOS, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER, SetCursorPos, SetLayeredWindowAttributes,
    SetWindowLongPtrW, SetWindowPos, WM_SYSCOMMAND, WS_CHILD, WS_EX_LAYERED, WS_EX_MDICHILD,
    WS_EX_TOPMOST,
};
use windows::core::PCWSTR;

use crate::engine::Output;
use crate::model::{Func, Gravity, ShellExecute, ShowCommand, Target, ToWindow};

/// `&WindowVMaximize` / `&WindowHMaximize` で覚えた元の位置（yamy の `WindowPosition`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    H,
    V,
    Hv,
}

struct Remembered {
    hwnd: HWND,
    rc: RECT,
    mode: Mode,
}

/// 関数の実行に必要な、呼び出しをまたぐ状態。
#[derive(Default)]
pub struct WindowOps {
    /// 縦／横最大化の前の位置。
    positions: Vec<Remembered>,
    /// `&WindowSetAlpha` で半透明にした窓。
    with_alpha: Vec<HWND>,
}

impl WindowOps {
    pub fn new() -> Self {
        WindowOps::default()
    }

    /// エンジンの出力のうち `Output::Func` だけを実行する。
    pub fn run(&mut self, outputs: &[Output]) {
        for o in outputs {
            if let Output::Func { hwnd, func } = o {
                self.exec(HWND(*hwnd as usize as *mut _), *func);
            }
        }
    }

    fn exec(&mut self, focus: HWND, func: Func) {
        if focus.0.is_null() {
            return;
        }
        match func {
            Func::WindowRaise(t) => {
                if let Some(w) = target_window(focus, t) {
                    set_pos_z(w.hwnd, HWND_TOP);
                }
            }
            Func::WindowLower(t) => {
                if let Some(w) = target_window(focus, t) {
                    set_pos_z(w.hwnd, HWND_BOTTOM);
                }
            }
            Func::WindowMinimize(t) => {
                if let Some(w) = target_window(focus, t) {
                    // SAFETY: 有効な窓ハンドル。
                    let iconic = unsafe { IsIconic(w.hwnd) }.as_bool();
                    sys_command(w.hwnd, if iconic { SC_RESTORE } else { SC_MINIMIZE });
                }
            }
            Func::WindowMaximize(t) => {
                if let Some(w) = target_window(focus, t) {
                    // SAFETY: 有効な窓ハンドル。
                    let zoomed = unsafe { IsZoomed(w.hwnd) }.as_bool();
                    sys_command(w.hwnd, if zoomed { SC_RESTORE } else { SC_MAXIMIZE });
                }
            }
            Func::WindowHMaximize(t) => self.hv_maximize(focus, t, true),
            Func::WindowVMaximize(t) => self.hv_maximize(focus, t, false),
            Func::WindowMove { dx, dy, target } => {
                move_to(focus, target, Gravity::Center, dx, dy);
            }
            Func::WindowMoveTo {
                gravity,
                dx,
                dy,
                target,
            } => move_to(focus, target, gravity, dx, dy),
            Func::WindowMoveVisibly(t) => {
                let Some(w) = target_window(focus, t) else {
                    return;
                };
                let (x, y) = moved_visibly(&w.rc, &w.area);
                async_move(w.hwnd, x, y);
            }
            Func::WindowClose(t) => {
                if let Some(w) = target_window(focus, t) {
                    sys_command(w.hwnd, SC_CLOSE);
                }
            }
            Func::WindowToggleTopMost => {
                let Some(hwnd) = toplevel(focus, false).0 else {
                    return;
                };
                // SAFETY: 有効な窓ハンドル。
                let ex = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };
                let topmost = ex as u32 & WS_EX_TOPMOST.0 != 0;
                set_pos_z(
                    hwnd,
                    if topmost {
                        HWND_NOTOPMOST
                    } else {
                        HWND_TOPMOST
                    },
                );
            }
            Func::WindowSetAlpha(alpha) => self.set_alpha(focus, alpha),
            Func::WindowRedraw => {
                if let Some(hwnd) = toplevel(focus, false).0 {
                    redraw(hwnd);
                }
            }
            Func::Recenter => recenter(focus),
            Func::WindowIdentify => crate::investigate::window_identify(focus),
            Func::WindowResizeTo {
                width: w,
                height: h,
                target,
            } => {
                let Some(t) = target_window(focus, target) else {
                    return;
                };
                let (w, h) = resized_to(&t.rc, &t.area, w, h);
                // SAFETY: 有効な窓ハンドル。非同期で位置はそのまま。
                let _ = unsafe {
                    SetWindowPos(
                        t.hwnd,
                        None,
                        0,
                        0,
                        w,
                        h,
                        SWP_ASYNCWINDOWPOS
                            | SWP_NOACTIVATE
                            | SWP_NOOWNERZORDER
                            | SWP_NOMOVE
                            | SWP_NOZORDER,
                    )
                };
            }
            Func::PostMessage {
                to,
                message,
                wparam,
                lparam,
            } => {
                let mut hwnd = focus;
                match to {
                    ToWindow::Itself => {}
                    // SAFETY: 有効な窓ハンドル（親が無ければ NULL）。
                    ToWindow::Parent => hwnd = unsafe { GetParent(hwnd) }.unwrap_or_default(),
                    ToWindow::Main => loop {
                        // SAFETY: 同上。
                        match unsafe { GetParent(hwnd) } {
                            Ok(p) if !p.0.is_null() => hwnd = p,
                            _ => break,
                        }
                    },
                    ToWindow::Overlapped => hwnd = toplevel(hwnd, false).0.unwrap_or_default(),
                }
                if !hwnd.0.is_null() {
                    // SAFETY: 有効な窓ハンドル。値は設定ファイルの指定どおり。
                    let _ = unsafe {
                        PostMessageW(
                            Some(hwnd),
                            message,
                            WPARAM(wparam as usize),
                            LPARAM(lparam as isize),
                        )
                    };
                }
            }
            Func::Undefined => {
                // SAFETY: 単純な API。
                let _ = unsafe { MessageBeep(MB_OK) };
            }
            Func::ClipboardUpcaseWord => clipboard_change_case(true),
            Func::ClipboardDowncaseWord => clipboard_change_case(false),
            Func::MouseMove { dx, dy } => {
                let mut pt = POINT::default();
                // SAFETY: pt は有効な POINT。
                if unsafe { GetCursorPos(&mut pt) }.is_ok() {
                    // SAFETY: 単純な API。
                    let _ = unsafe { SetCursorPos(pt.x + dx, pt.y + dy) };
                }
            }
            Func::MouseWheel(delta) => {
                // yamy の mouse_event(MOUSEEVENTF_WHEEL, 0, 0, delta, 0) と同じ
                let input = INPUT {
                    r#type: INPUT_MOUSE,
                    Anonymous: INPUT_0 {
                        mi: MOUSEINPUT {
                            dx: 0,
                            dy: 0,
                            mouseData: delta as u32,
                            dwFlags: MOUSEEVENTF_WHEEL,
                            time: 0,
                            dwExtraInfo: 0,
                        },
                    },
                };
                // SAFETY: input は有効な INPUT。
                let _ = unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) };
            }
        }
    }

    /// 縦／横だけ最大化。もう一度で元に戻す（yamy の `funcWindowHVMaximize`）。
    fn hv_maximize(&mut self, focus: HWND, t: Target, horizontal: bool) {
        let Some(w) = target_window(focus, t) else {
            return;
        };
        // SAFETY: 有効な窓ハンドルか確かめる。
        self.positions
            .retain(|p| unsafe { IsWindow(Some(p.hwnd)) }.as_bool());
        // SAFETY: 有効な窓ハンドル。
        if unsafe { IsZoomed(w.hwnd) }.as_bool() {
            sys_command(w.hwnd, SC_RESTORE);
            return;
        }
        let found = self.positions.iter().position(|p| p.hwnd == w.hwnd);
        // 覚えていれば、元の位置は覚えたときのもの
        let original = found.map_or(w.rc, |i| self.positions[i].rc);
        let mode = hv_next(found.map(|i| self.positions[i].mode), horizontal);
        match (found, mode) {
            (Some(i), Some(mode)) => self.positions[i].mode = mode,
            (Some(i), None) => {
                self.positions.remove(i);
            }
            (None, Some(mode)) => self.positions.insert(
                0,
                Remembered {
                    hwnd: w.hwnd,
                    rc: original,
                    mode,
                },
            ),
            (None, None) => {}
        }
        let rc = hv_rect(original, &w.area, mode);
        // SAFETY: 有効な窓ハンドル。非同期で移動と大きさ変更。
        let _ = unsafe {
            SetWindowPos(
                w.hwnd,
                None,
                rc.left,
                rc.top,
                width(&rc),
                height(&rc),
                SWP_ASYNCWINDOWPOS | SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_NOZORDER,
            )
        };
    }

    /// 半透明化のトグル。`alpha < 0` で自分が半透明にした窓を全部戻す。
    fn set_alpha(&mut self, focus: HWND, alpha: i32) {
        if alpha < 0 {
            for hwnd in self.with_alpha.drain(..) {
                // SAFETY: 有効な窓ハンドル（消えていれば失敗するだけ）。
                unsafe {
                    let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
                    SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex & !(WS_EX_LAYERED.0 as isize));
                }
                redraw(hwnd);
            }
            return;
        }
        let Some(hwnd) = toplevel(focus, false).0 else {
            return;
        };
        // SAFETY: 有効な窓ハンドル。
        let ex = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };
        if ex as u32 & WS_EX_LAYERED.0 != 0 {
            // 自分が半透明にした窓なら戻す。アプリ自身が layered にした窓は触らない
            let Some(i) = self.with_alpha.iter().position(|h| *h == hwnd) else {
                return;
            };
            self.with_alpha.remove(i);
            // SAFETY: 有効な窓ハンドル。
            unsafe { SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex & !(WS_EX_LAYERED.0 as isize)) };
        } else {
            // SAFETY: 有効な窓ハンドル。
            unsafe { SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | WS_EX_LAYERED.0 as isize) };
            // SAFETY: 有効な窓ハンドル。
            if unsafe {
                SetLayeredWindowAttributes(hwnd, COLORREF(0), alpha_byte(alpha), LWA_ALPHA)
            }
            .is_err()
            {
                crate::log!(
                    "&WindowSetAlpha({}): 失敗 (hwnd={:#x})",
                    alpha % 101,
                    hwnd.0 as usize
                );
                return;
            }
            self.with_alpha.insert(0, hwnd);
        }
        redraw(hwnd);
    }
}

/// 対象の窓と、その位置、動かせる範囲。
struct TargetWindow {
    hwnd: HWND,
    /// 窓の位置（最上位ならスクリーン座標、MDI 子窓なら親のクライアント座標）。
    rc: RECT,
    /// 動かせる範囲（最上位ならモニターの作業領域、MDI 子窓なら親のクライアント領域）。
    area: RECT,
}

/// フォーカス窓から対象の窓を決める（yamy の `getSuitableMdiWindow`）。
fn target_window(focus: HWND, t: Target) -> Option<TargetWindow> {
    let (hwnd, is_mdi) = toplevel(focus, t == Target::Mdi);
    let hwnd = hwnd?;
    let mut rc = RECT::default();
    let mut area = RECT::default();
    if is_mdi {
        // SAFETY: 有効な窓ハンドル。rc は有効な RECT。
        unsafe { GetWindowRect(hwnd, &mut rc) }.ok()?;
        // SAFETY: 同上。MDI 子窓には親がある。
        let parent = unsafe { GetParent(hwnd) }.ok()?;
        let mut p = POINT {
            x: rc.left,
            y: rc.top,
        };
        // SAFETY: parent は有効な窓、p は有効な POINT。
        if !unsafe { ScreenToClient(parent, &mut p) }.as_bool() {
            return None;
        }
        rc.left = p.x;
        rc.top = p.y;
        p = POINT {
            x: rc.right,
            y: rc.bottom,
        };
        // SAFETY: 同上。
        if !unsafe { ScreenToClient(parent, &mut p) }.as_bool() {
            return None;
        }
        rc.right = p.x;
        rc.bottom = p.y;
        // SAFETY: 同上。
        unsafe { GetClientRect(parent, &mut area) }.ok()?;
    } else {
        // SAFETY: 有効な窓ハンドル。rc は有効な RECT。
        unsafe { GetWindowRect(hwnd, &mut rc) }.ok()?;
        // SAFETY: フォーカス窓に最も近いモニター。
        let hm = unsafe { MonitorFromWindow(focus, MONITOR_DEFAULTTONEAREST) };
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        // SAFETY: mi は cbSize を設定済み。
        if !unsafe { GetMonitorInfoW(hm, &mut mi) }.as_bool() {
            return None;
        }
        area = mi.rcWork;
    }
    Some(TargetWindow { hwnd, rc, area })
}

/// 子窓でなくなるまで親をたどる（yamy の `getToplevelWindow`）。
/// `want_mdi` なら MDI 子窓で止まり、戻り値の 2 つ目が true になる。
pub(crate) fn toplevel(mut hwnd: HWND, want_mdi: bool) -> (Option<HWND>, bool) {
    while !hwnd.0.is_null() {
        // SAFETY: 有効な窓ハンドル（無効なら 0 が返る）。
        let style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) } as u32;
        if style & WS_CHILD.0 == 0 {
            break;
        }
        if want_mdi {
            // SAFETY: 同上。
            let ex = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
            if ex & WS_EX_MDICHILD.0 != 0 {
                return (Some(hwnd), true);
            }
        }
        // SAFETY: 同上。
        hwnd = unsafe { GetParent(hwnd) }.unwrap_or_default();
    }
    if hwnd.0.is_null() {
        (None, false)
    } else {
        (Some(hwnd), false)
    }
}

/// `&WindowMove` / `&WindowMoveTo` / `&WindowClingTo…`（yamy の `funcWindowMoveTo`）。
fn move_to(focus: HWND, t: Target, gravity: Gravity, dx: i32, dy: i32) {
    let Some(w) = target_window(focus, t) else {
        return;
    };
    let (x, y) = moved_to(&w.rc, &w.area, gravity, dx, dy);
    async_move(w.hwnd, x, y);
}

// ---- 位置と大きさの計算（OS に触れない） ------------------------------------
//
// `rc` は窓の位置、`area` は動かせる範囲（[`TargetWindow`]）。

/// `&WindowMove` / `&WindowMoveTo` の移動先の左上（yamy の `funcWindowMoveTo`）。
fn moved_to(rc: &RECT, area: &RECT, gravity: Gravity, dx: i32, dy: i32) -> (i32, i32) {
    let mut x = rc.left + dx;
    let mut y = rc.top + dy;
    if gravity.north() {
        y = dy + area.top;
    }
    if gravity.east() {
        x = dx + area.right - width(rc);
    }
    if gravity.west() {
        x = dx + area.left;
    }
    if gravity.south() {
        y = dy + area.bottom - height(rc);
    }
    (x, y)
}

/// `&WindowMoveVisibly` の移動先の左上（yamy の `funcWindowMoveVisibly`）。
fn moved_visibly(rc: &RECT, area: &RECT) -> (i32, i32) {
    let mut x = rc.left;
    let mut y = rc.top;
    if rc.left < area.left {
        x = area.left;
    } else if area.right < rc.right {
        x = area.right - width(rc);
    }
    if rc.top < area.top {
        y = area.top;
    } else if area.bottom < rc.bottom {
        y = area.bottom - height(rc);
    }
    (x, y)
}

/// `&WindowResizeTo` の大きさ。0 はそのまま、負は範囲の大きさからの差（yamy の `funcWindowResizeTo`）。
fn resized_to(rc: &RECT, area: &RECT, w: i32, h: i32) -> (i32, i32) {
    let w = match w {
        0 => width(rc),
        w if w < 0 => w + width(area),
        w => w,
    };
    let h = match h {
        0 => height(rc),
        h if h < 0 => h + height(area),
        h => h,
    };
    (w, h)
}

/// 縦／横最大化をした後の状態。`prev` は覚えている状態（覚えていなければ `None`）。
/// 戻り値の `None` は元の位置に戻して忘れる（yamy の `funcWindowHVMaximize`）。
fn hv_next(prev: Option<Mode>, horizontal: bool) -> Option<Mode> {
    match (prev, horizontal) {
        (None, true) => Some(Mode::H),
        (None, false) => Some(Mode::V),
        (Some(Mode::Hv), true) => Some(Mode::V),
        (Some(Mode::Hv), false) => Some(Mode::H),
        (Some(Mode::V), true) | (Some(Mode::H), false) => Some(Mode::Hv),
        // 同じ向きをもう一度: 元に戻す
        (Some(Mode::H), true) | (Some(Mode::V), false) => None,
    }
}

/// 縦／横最大化の後の位置。`original` は最大化する前の位置。
fn hv_rect(original: RECT, area: &RECT, mode: Option<Mode>) -> RECT {
    let mut rc = original;
    if matches!(mode, Some(Mode::H) | Some(Mode::Hv)) {
        rc.left = area.left;
        rc.right = area.right;
    }
    if matches!(mode, Some(Mode::V) | Some(Mode::Hv)) {
        rc.top = area.top;
        rc.bottom = area.bottom;
    }
    rc
}

/// `&WindowSetAlpha` の 0〜100 を `SetLayeredWindowAttributes` の 0〜255 にする
/// （yamy の `funcWindowSetAlpha`。101 以上は 101 で割った余り）。
fn alpha_byte(alpha: i32) -> u8 {
    (255 * (alpha % 101) / 100) as u8
}

fn width(rc: &RECT) -> i32 {
    rc.right - rc.left
}
fn height(rc: &RECT) -> i32 {
    rc.bottom - rc.top
}

fn async_move(hwnd: HWND, x: i32, y: i32) {
    // SAFETY: 有効な窓ハンドル。非同期で位置だけ変える。
    let _ = unsafe {
        SetWindowPos(
            hwnd,
            None,
            x,
            y,
            0,
            0,
            SWP_ASYNCWINDOWPOS | SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_NOSIZE | SWP_NOZORDER,
        )
    };
}

fn set_pos_z(hwnd: HWND, after: HWND) {
    // SAFETY: 有効な窓ハンドル。Z 順だけ変える。
    let _ = unsafe {
        SetWindowPos(
            hwnd,
            Some(after),
            0,
            0,
            0,
            0,
            SWP_ASYNCWINDOWPOS | SWP_NOMOVE | SWP_NOACTIVATE | SWP_NOSIZE,
        )
    };
}

/// `&Recenter`: フォーカス窓へ命令を送る。スクロールはそのプロセスの中のフック DLL がする。
fn recenter(focus: HWND) {
    post_command(focus, COMMAND_RECENTER);
}

/// 窓へフック DLL への命令（`COMMAND_*`）を送る。そのプロセスの中のフック DLL が受ける。
pub(crate) fn post_command(hwnd: HWND, command: usize) {
    let name: Vec<u16> = COMMAND_MESSAGE_NAME
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: name は NUL 終端の UTF-16。
    let message = unsafe { RegisterWindowMessageW(PCWSTR(name.as_ptr())) };
    if message == 0 {
        return;
    }
    // SAFETY: 窓ハンドルを渡すだけ。無効なら失敗する。
    let _ = unsafe { PostMessageW(Some(hwnd), message, WPARAM(command), LPARAM(0)) };
}

/// `&ShellExecute`: yamy の Engine::shellExecute。yamy はキー処理から外して本体の窓で呼ぶので、
/// ここでは別スレッドで呼ぶ。失敗したらログに出す。
pub fn shell_execute(s: ShellExecute) {
    std::thread::spawn(move || {
        // ShellExecuteW はシェル拡張を COM で呼ぶことがあるので、このスレッドで COM を準備する
        // SAFETY: 引数は値だけ。
        let hr = unsafe {
            CoInitializeEx(
                None,
                COINIT(COINIT_APARTMENTTHREADED.0 | COINIT_DISABLE_OLE1DDE.0),
            )
        };
        let wide = |s: &str| -> Option<Vec<u16>> {
            (!s.is_empty()).then(|| s.encode_utf16().chain(std::iter::once(0)).collect())
        };
        let ptr = |w: &Option<Vec<u16>>| w.as_ref().map_or(PCWSTR::null(), |w| PCWSTR(w.as_ptr()));
        let operation = wide(&s.operation).or_else(|| wide("open"));
        let (file, parameters, directory) =
            (wide(&s.file), wide(&s.parameters), wide(&s.directory));
        // SAFETY: 文字列はどれも NUL 終端の UTF-16 か NULL で、呼び終わるまで持つ。
        let r = unsafe {
            ShellExecuteW(
                None,
                ptr(&operation),
                ptr(&file),
                ptr(&parameters),
                ptr(&directory),
                show_command(s.show),
            )
        };
        let code = r.0 as isize;
        if code <= 32 {
            crate::log!("&ShellExecute: {s:?}: {}", shell_execute_error(code));
        }
        if hr.is_ok() {
            // SAFETY: CoInitializeEx が成功したスレッド。
            unsafe { CoUninitialize() };
        }
    });
}

pub(crate) fn show_command(s: ShowCommand) -> SHOW_WINDOW_CMD {
    match s {
        ShowCommand::Hide => SW_HIDE,
        ShowCommand::Maximize => SW_MAXIMIZE,
        ShowCommand::Minimize => SW_MINIMIZE,
        ShowCommand::Restore => SW_RESTORE,
        ShowCommand::Show => SW_SHOW,
        ShowCommand::ShowDefault => SW_SHOWDEFAULT,
        ShowCommand::ShowMaximized => SW_SHOWMAXIMIZED,
        ShowCommand::ShowMinimized => SW_SHOWMINIMIZED,
        ShowCommand::ShowMinNoActive => SW_SHOWMINNOACTIVE,
        ShowCommand::ShowNA => SW_SHOWNA,
        ShowCommand::ShowNoActivate => SW_SHOWNOACTIVATE,
        ShowCommand::ShowNormal => SW_SHOWNORMAL,
    }
}

/// `ShellExecuteW` が 32 以下で返した失敗の理由（yamy の errorTable）。
fn shell_execute_error(code: isize) -> &'static str {
    match code {
        0 => "メモリかリソースが足りない",
        2 => "ファイルが見つからない",
        3 => "パスが見つからない",
        5 => "アクセスが拒否された",
        8 => "メモリが足りない",
        11 => "exe ファイルが不正",
        26 => "共有違反",
        27 => "ファイルの関連付けが不完全か不正",
        28 => "DDE がタイムアウトした",
        29 => "DDE が失敗した",
        30 => "ほかの DDE を処理中で完了できない",
        31 => "この拡張子に関連付けられたアプリが無い",
        32 => "DLL が見つからない",
        _ => "不明なエラー",
    }
}

fn sys_command(hwnd: HWND, cmd: u32) {
    // SAFETY: 有効な窓ハンドル。
    let _ = unsafe { PostMessageW(Some(hwnd), WM_SYSCOMMAND, WPARAM(cmd as usize), LPARAM(0)) };
}

fn redraw(hwnd: HWND) {
    // SAFETY: 有効な窓ハンドル。
    let _ = unsafe {
        RedrawWindow(
            Some(hwnd),
            None,
            None,
            RDW_ERASE | RDW_INVALIDATE | RDW_FRAME | RDW_ALLCHILDREN,
        )
    };
}

/// `CF_UNICODETEXT`。
const CF_UNICODETEXT: u32 = 13;

/// `&ClipboardUpcaseWord` / `&ClipboardDowncaseWord`: クリップボードの文字列の大文字小文字を変える
/// （yamy の funcClipboardChangeCase）。文字列が入っていなければ何もしない。
fn clipboard_change_case(upper: bool) {
    // SAFETY: 開いたら必ず CloseClipboard する。中の関数はクリップボードを開いている間だけ呼ぶ。
    unsafe {
        if OpenClipboard(None).is_err() {
            crate::log!("クリップボードを開けない");
            return;
        }
        if let Some(text) = read_clipboard_text() {
            let text = if upper {
                text.to_uppercase()
            } else {
                text.to_lowercase()
            };
            write_clipboard_text(&text);
        }
        let _ = CloseClipboard();
    }
}

/// クリップボードの文字列。開けないか文字列が無ければ `None`。
pub fn clipboard_text() -> Option<String> {
    // SAFETY: 開いたら必ず CloseClipboard する。
    unsafe {
        if OpenClipboard(None).is_err() {
            crate::log!("クリップボードを開けない");
            return None;
        }
        let text = read_clipboard_text();
        let _ = CloseClipboard();
        text
    }
}

/// クリップボードを空にする。
pub fn empty_clipboard() {
    // SAFETY: 開いたら必ず CloseClipboard する。
    unsafe {
        if OpenClipboard(None).is_err() {
            crate::log!("クリップボードを開けない");
            return;
        }
        let _ = EmptyClipboard();
        let _ = CloseClipboard();
    }
}

/// クリップボードを文字列で置き換える。
pub fn set_clipboard_text(text: &str) {
    // SAFETY: 開いたら必ず CloseClipboard する。
    unsafe {
        if OpenClipboard(None).is_err() {
            crate::log!("クリップボードを開けない");
            return;
        }
        write_clipboard_text(text);
        let _ = CloseClipboard();
    }
}

/// クリップボードを開いている間に呼ぶ。文字列が無ければ `None`。
unsafe fn read_clipboard_text() -> Option<String> {
    // SAFETY: 呼ぶ側がクリップボードを開いている。ハンドルは OS が持つ CF_UNICODETEXT の HGLOBAL。
    unsafe {
        let h = GetClipboardData(CF_UNICODETEXT).ok()?;
        let hg = HGLOBAL(h.0);
        let p = GlobalLock(hg) as *const u16;
        if p.is_null() {
            return None;
        }
        let mut len = 0;
        while *p.add(len) != 0 {
            len += 1;
        }
        let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
        let _ = GlobalUnlock(hg);
        Some(s)
    }
}

/// クリップボードを開いている間に呼ぶ。中身を置き換える。
unsafe fn write_clipboard_text(s: &str) {
    let w: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: 呼ぶ側がクリップボードを開いている。渡した HGLOBAL は SetClipboardData 後は OS のもの。
    unsafe {
        let Ok(hg) = GlobalAlloc(GMEM_MOVEABLE, w.len() * 2) else {
            return;
        };
        let p = GlobalLock(hg) as *mut u16;
        if p.is_null() {
            return;
        }
        std::ptr::copy_nonoverlapping(w.as_ptr(), p, w.len());
        let _ = GlobalUnlock(hg);
        if EmptyClipboard().is_ok() && SetClipboardData(CF_UNICODETEXT, Some(HANDLE(hg.0))).is_err()
        {
            crate::log!("クリップボードに書けない");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn rect(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
        RECT {
            left,
            top,
            right,
            bottom,
        }
    }

    /// 動かせる範囲（作業領域）。左上が (0, 0) でない画面。
    const AREA: RECT = rect(10, 20, 1010, 820);
    /// 幅 300・高さ 300 の窓。
    const WINDOW: RECT = rect(100, 200, 400, 500);

    #[test]
    fn move_adds_the_offset_to_the_position() {
        // &WindowMove(16, -8)
        assert_eq!(
            moved_to(&WINDOW, &AREA, Gravity::Center, 16, -8),
            (116, 192)
        );
    }

    #[test]
    fn move_to_north_measures_y_from_the_top_of_the_area() {
        // &WindowMoveTo(N, 3, 4)
        assert_eq!(moved_to(&WINDOW, &AREA, Gravity::North, 3, 4), (103, 24));
    }

    #[test]
    fn move_to_south_measures_y_from_the_bottom_of_the_area() {
        // &WindowMoveTo(S, 3, 4)
        assert_eq!(moved_to(&WINDOW, &AREA, Gravity::South, 3, 4), (103, 524));
    }

    #[test]
    fn move_to_east_measures_x_from_the_right_of_the_area() {
        // &WindowMoveTo(E, 3, 4)
        assert_eq!(moved_to(&WINDOW, &AREA, Gravity::East, 3, 4), (713, 204));
    }

    #[test]
    fn move_to_west_measures_x_from_the_left_of_the_area() {
        // &WindowMoveTo(W, 3, 4)
        assert_eq!(moved_to(&WINDOW, &AREA, Gravity::West, 3, 4), (13, 204));
    }

    #[test]
    fn move_to_north_east_measures_from_the_corner() {
        // &WindowMoveTo(NE, 3, 4)
        assert_eq!(
            moved_to(&WINDOW, &AREA, Gravity::NorthEast, 3, 4),
            (713, 24)
        );
    }

    #[test]
    fn move_visibly_keeps_a_window_inside_the_area() {
        // &WindowMoveVisibly
        assert_eq!(moved_visibly(&WINDOW, &AREA), (100, 200));
    }

    #[test]
    fn move_visibly_brings_back_the_left_edge() {
        assert_eq!(moved_visibly(&rect(-50, 200, 250, 500), &AREA), (10, 200));
    }

    #[test]
    fn move_visibly_brings_back_the_right_edge() {
        assert_eq!(moved_visibly(&rect(900, 200, 1200, 500), &AREA), (710, 200));
    }

    #[test]
    fn move_visibly_brings_back_the_top_edge() {
        assert_eq!(moved_visibly(&rect(100, -5, 400, 295), &AREA), (100, 20));
    }

    #[test]
    fn move_visibly_brings_back_the_bottom_edge() {
        assert_eq!(moved_visibly(&rect(100, 700, 400, 1000), &AREA), (100, 520));
    }

    #[test]
    fn resize_to_zero_keeps_the_size() {
        // &WindowResizeTo(0, 0)
        assert_eq!(resized_to(&WINDOW, &AREA, 0, 0), (300, 300));
    }

    #[test]
    fn resize_to_negative_is_measured_from_the_area_size() {
        // &WindowResizeTo(-100, -50)
        assert_eq!(resized_to(&WINDOW, &AREA, -100, -50), (900, 750));
    }

    #[test]
    fn resize_to_positive_is_the_size() {
        // &WindowResizeTo(640, 480)
        assert_eq!(resized_to(&WINDOW, &AREA, 640, 480), (640, 480));
    }

    #[test]
    fn first_h_maximize_is_horizontal() {
        // &WindowHMaximize
        assert_eq!(hv_next(None, true), Some(Mode::H));
    }

    #[test]
    fn first_v_maximize_is_vertical() {
        // &WindowVMaximize
        assert_eq!(hv_next(None, false), Some(Mode::V));
    }

    #[test]
    fn v_maximize_after_h_maximize_is_both() {
        assert_eq!(hv_next(Some(Mode::H), false), Some(Mode::Hv));
    }

    #[test]
    fn h_maximize_after_v_maximize_is_both() {
        assert_eq!(hv_next(Some(Mode::V), true), Some(Mode::Hv));
    }

    #[test]
    fn h_maximize_on_both_leaves_vertical() {
        assert_eq!(hv_next(Some(Mode::Hv), true), Some(Mode::V));
    }

    #[test]
    fn v_maximize_on_both_leaves_horizontal() {
        assert_eq!(hv_next(Some(Mode::Hv), false), Some(Mode::H));
    }

    #[test]
    fn h_maximize_twice_restores() {
        assert_eq!(hv_next(Some(Mode::H), true), None);
    }

    #[test]
    fn v_maximize_twice_restores() {
        assert_eq!(hv_next(Some(Mode::V), false), None);
    }

    #[test]
    fn horizontal_fills_the_width_of_the_area() {
        assert_eq!(
            hv_rect(WINDOW, &AREA, Some(Mode::H)),
            rect(10, 200, 1010, 500)
        );
    }

    #[test]
    fn vertical_fills_the_height_of_the_area() {
        assert_eq!(
            hv_rect(WINDOW, &AREA, Some(Mode::V)),
            rect(100, 20, 400, 820)
        );
    }

    #[test]
    fn both_fills_the_area() {
        assert_eq!(hv_rect(WINDOW, &AREA, Some(Mode::Hv)), AREA);
    }

    #[test]
    fn restored_rect_is_the_remembered_one() {
        assert_eq!(hv_rect(WINDOW, &AREA, None), WINDOW);
    }

    #[test]
    fn alpha_is_a_percentage_of_255() {
        // &WindowSetAlpha(70)
        assert_eq!(alpha_byte(70), 178);
    }

    #[test]
    fn alpha_over_100_wraps_around() {
        // &WindowSetAlpha(150)（yamy の i_alpha %= 101）
        assert_eq!(alpha_byte(150), 124);
    }
}

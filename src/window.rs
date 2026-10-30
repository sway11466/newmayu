//! 窓を操作する `&関数`（`&WindowMove` など）と `&MouseMove` の Win32 側。
//! yamy の function.cpp の funcWindow* と windowstool.cpp を写している。
//!
//! エンジンは OS を触らず、[`Output::Func`] に「フォーカス窓」と「何をするか」を乗せて返す。
//! ここがそれを受け、対象の窓（最上位の窓、または MDI 子窓）を探して動かす。

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow, RDW_ALLCHILDREN,
    RDW_ERASE, RDW_FRAME, RDW_INVALIDATE, RedrawWindow, ScreenToClient,
};
use windows::Win32::System::Diagnostics::Debug::MessageBeep;
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GWL_STYLE, GetClientRect, GetCursorPos, GetParent, GetWindowLongPtrW,
    GetWindowRect, HWND_BOTTOM, HWND_NOTOPMOST, HWND_TOP, HWND_TOPMOST, IsIconic, IsWindow,
    IsZoomed, LWA_ALPHA, MB_OK, PostMessageW, SC_CLOSE, SC_MAXIMIZE, SC_MINIMIZE, SC_RESTORE,
    SWP_ASYNCWINDOWPOS, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER,
    SetCursorPos, SetLayeredWindowAttributes, SetWindowLongPtrW, SetWindowPos, WM_SYSCOMMAND,
    WS_CHILD, WS_EX_LAYERED, WS_EX_MDICHILD, WS_EX_TOPMOST,
};

use crate::engine::Output;
use crate::model::{Func, Gravity, Target, ToWindow};

/// `&WindowVMaximize` / `&WindowHMaximize` で覚えた元の位置（yamy の `WindowPosition`）。
#[derive(Clone, Copy, PartialEq, Eq)]
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
                let (rc, rcd) = (w.rc, w.area);
                let mut x = rc.left;
                let mut y = rc.top;
                if rc.left < rcd.left {
                    x = rcd.left;
                } else if rcd.right < rc.right {
                    x = rcd.right - width(&rc);
                }
                if rc.top < rcd.top {
                    y = rcd.top;
                } else if rcd.bottom < rc.bottom {
                    y = rcd.bottom - height(&rc);
                }
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
            Func::WindowResizeTo {
                width: w,
                height: h,
                target,
            } => {
                let Some(t) = target_window(focus, target) else {
                    return;
                };
                let w = match w {
                    0 => width(&t.rc),
                    w if w < 0 => w + width(&t.area),
                    w => w,
                };
                let h = match h {
                    0 => height(&t.rc),
                    h if h < 0 => h + height(&t.area),
                    h => h,
                };
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
            Func::MouseMove { dx, dy } => {
                let mut pt = POINT::default();
                // SAFETY: pt は有効な POINT。
                if unsafe { GetCursorPos(&mut pt) }.is_ok() {
                    // SAFETY: 単純な API。
                    let _ = unsafe { SetCursorPos(pt.x + dx, pt.y + dy) };
                }
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
        let mut rc = w.rc;
        let rcd = w.area;
        let mode = match self.positions.iter().position(|p| p.hwnd == w.hwnd) {
            Some(i) => {
                let p = &mut self.positions[i];
                rc = p.rc;
                match (p.mode, horizontal) {
                    (Mode::Hv, true) => {
                        p.mode = Mode::V;
                        Some(Mode::V)
                    }
                    (Mode::Hv, false) => {
                        p.mode = Mode::H;
                        Some(Mode::H)
                    }
                    (Mode::V, true) | (Mode::H, false) => {
                        p.mode = Mode::Hv;
                        Some(Mode::Hv)
                    }
                    _ => {
                        // 同じ向きをもう一度: 元に戻す
                        self.positions.remove(i);
                        None
                    }
                }
            }
            None => {
                let mode = if horizontal { Mode::H } else { Mode::V };
                self.positions.insert(
                    0,
                    Remembered {
                        hwnd: w.hwnd,
                        rc,
                        mode,
                    },
                );
                Some(mode)
            }
        };
        if matches!(mode, Some(Mode::H) | Some(Mode::Hv)) {
            rc.left = rcd.left;
            rc.right = rcd.right;
        }
        if matches!(mode, Some(Mode::V) | Some(Mode::Hv)) {
            rc.top = rcd.top;
            rc.bottom = rcd.bottom;
        }
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
            let alpha = alpha % 101;
            // SAFETY: 有効な窓ハンドル。
            if unsafe {
                SetLayeredWindowAttributes(hwnd, COLORREF(0), (255 * alpha / 100) as u8, LWA_ALPHA)
            }
            .is_err()
            {
                eprintln!(
                    "&WindowSetAlpha({alpha}): 失敗 (hwnd={:#x})",
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
fn toplevel(mut hwnd: HWND, want_mdi: bool) -> (Option<HWND>, bool) {
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
    let (rc, rcd) = (w.rc, w.area);
    let mut x = rc.left + dx;
    let mut y = rc.top + dy;
    if gravity.north() {
        y = dy + rcd.top;
    }
    if gravity.east() {
        x = dx + rcd.right - width(&rc);
    }
    if gravity.west() {
        x = dx + rcd.left;
    }
    if gravity.south() {
        y = dy + rcd.bottom - height(&rc);
    }
    async_move(w.hwnd, x, y);
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

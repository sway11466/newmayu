//! 案内の窓（`&HelpMessage` / `&HelpVariable`）。yamy と異なる（doc/spec.md 6 章）。
//!
//! 窓は専用のスレッドで作り、そこでメッセージループを回す。フックを処理するメインスレッドを
//! 描画・タイマー・ドラッグで止めないため。エンジンスレッドは [`Help::run`] で頼むだけ。
//!
//! - 板: フォーカス窓がある画面の下の中央に出す。クリックは下の窓へ素通りする
//!   （`WS_EX_LAYERED | WS_EX_TRANSPARENT`）。5 秒か、消す指示で消える
//! - ピン: 板の右上に重ねる別の小窓。ここだけクリックを受ける
//!   （1 枚の窓で `WM_NCHITTEST` に `HTTRANSPARENT` を返しても、別スレッドの窓へは素通りしない）
//! - ピン留めした板: 素通りをやめ、どこを掴んでもドラッグできる。ピンは板の中に描き、
//!   押すと消える。時間でも消す指示でも消えない。何枚でも残せる
//!
//! どの窓もタスクバー・Alt+Tab に出さず（`WS_EX_TOOLWINDOW`）、フォーカスを奪わない
//! （`WS_EX_NOACTIVATE`、`WM_MOUSEACTIVATE` に `MA_NOACTIVATE`）。
//! スレッドは Per-Monitor v2 の DPI 対応にし、画面ごとの DPI で大きさを決める。

use std::cell::RefCell;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreatePen, CreateRoundRectRgn, CreateSolidBrush, DEFAULT_CHARSET,
    DT_CALCRECT, DT_CENTER, DT_EXPANDTABS, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, DT_WORDBREAK,
    DeleteObject, DrawTextW, EndPaint, FillRect, GetDC, GetMonitorInfoW, GetStockObject, HDC,
    HFONT, HGDIOBJ, HMONITOR, InvalidateRect, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY,
    MONITORINFO, MonitorFromPoint, MonitorFromWindow, NULL_BRUSH, PAINTSTRUCT, PS_SOLID, ReleaseDC,
    RoundRect, SelectObject, SetBkMode, SetTextColor, SetWindowRgn, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForMonitor, MDT_EFFECTIVE_DPI,
    SetThreadDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GW_OWNER, GWL_EXSTYLE,
    GWLP_USERDATA, GetMessageW, GetWindow, GetWindowLongPtrW, GetWindowRect, HTCAPTION, HTCLIENT,
    HTTRANSPARENT, HWND_MESSAGE, HWND_TOPMOST, IDC_ARROW, KillTimer, LWA_ALPHA, LoadCursorW,
    MA_NOACTIVATE, MSG, PostMessageW, RegisterClassW, SW_SHOWNOACTIVATE, SWP_NOACTIVATE,
    SetLayeredWindowAttributes, SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow,
    WINDOW_EX_STYLE, WM_APP, WM_DPICHANGED, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE,
    WM_NCDESTROY, WM_NCHITTEST, WM_PAINT, WM_TIMER, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::{PCWSTR, w};

use crate::engine::Output;
use crate::log;
use crate::model::HelpText;

/// ピン留めしていない板が消えるまで。
const LIFETIME_MS: u32 = 5000;
const TIMER_ID: usize = 1;
/// `TrackMouseEvent` の離れた通知（windows クレートでは Win32_UI_Controls にある）。
const WM_MOUSELEAVE: u32 = 0x02A3;
/// エンジンスレッドから「頼みごとが届いた」と知らせるメッセージ（制御用の窓へ）。
const WM_HELP_WAKE: u32 = WM_APP + 1;

const CLASS_CTL: PCWSTR = w!("newmayu_help_ctl");
const CLASS_BOARD: PCWSTR = w!("newmayu_help_board");
const CLASS_PIN: PCWSTR = w!("newmayu_help_pin");

/// 寸法（96 DPI のときのピクセル）。
const PAD_X: i32 = 16;
const PAD_Y: i32 = 12;
const GAP: i32 = 4;
const PIN: i32 = 28;
const PIN_INSET: i32 = 6;
const RADIUS: i32 = 8;
const MARGIN_BOTTOM: i32 = 24;
const MIN_WIDTH: i32 = 200;
const MAX_WIDTH: i32 = 640;
const FONT_PX: i32 = 14;
const ICON_PX: i32 = 16;

/// Segoe Fluent Icons の Pin / PinnedFill。
const GLYPH_PIN: &str = "\u{E718}";
const GLYPH_PINNED: &str = "\u{E842}";

/// エンジンスレッドからの頼みごと。`hwnd` はそのときのフォーカス窓。
struct Request {
    hwnd: u64,
    text: Option<HelpText>,
}

/// 案内の窓のスレッドへの窓口。エンジンスレッドが持つ。
pub struct Help {
    tx: Sender<Request>,
    /// 制御用の窓（メッセージ専用窓）。`HWND` は `Send` でないので数で持つ。
    ctl: isize,
}

impl Help {
    /// 案内の窓のスレッドを立てる。
    pub fn start() -> Result<Help, String> {
        let (tx, rx) = channel();
        let (ready_tx, ready_rx) = channel();
        thread::spawn(move || ui_thread(rx, ready_tx));
        let ctl = ready_rx
            .recv()
            .map_err(|_| "案内の窓のスレッドが立ち上がらない".to_string())??;
        Ok(Help { tx, ctl })
    }

    /// エンジンの出力のうち `Output::Help` だけを案内の窓のスレッドへ渡す。
    pub fn run(&self, outputs: &[Output]) {
        let mut sent = false;
        for o in outputs {
            if let Output::Help { hwnd, text } = o {
                let _ = self.tx.send(Request {
                    hwnd: *hwnd,
                    text: text.clone(),
                });
                sent = true;
            }
        }
        if sent {
            // SAFETY: ctl は案内の窓のスレッドが作った窓。窓が無くなっていれば失敗するだけ。
            let _ = unsafe {
                PostMessageW(
                    Some(HWND(self.ctl as *mut _)),
                    WM_HELP_WAKE,
                    WPARAM(0),
                    LPARAM(0),
                )
            };
        }
    }
}

/// 案内の窓のスレッドの状態。
struct Ui {
    rx: Receiver<Request>,
    /// ピン留めしていない板（出ていれば 1 枚）。
    current: Option<HWND>,
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
}

fn ui_thread(rx: Receiver<Request>, ready: Sender<Result<isize, String>>) {
    // SAFETY: このスレッドで作る窓だけに効く。
    unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    let ctl = match create_classes_and_ctl() {
        Ok(h) => h,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    UI.with(|u| *u.borrow_mut() = Some(Ui { rx, current: None }));
    let _ = ready.send(Ok(ctl.0 as isize));
    let mut msg = MSG::default();
    // SAFETY: msg は有効な MSG。
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
        // SAFETY: GetMessageW が埋めた MSG。
        unsafe { DispatchMessageW(&msg) };
    }
}

fn create_classes_and_ctl() -> Result<HWND, String> {
    // SAFETY: 引数なし（自分のモジュール）。
    let hinstance = unsafe { GetModuleHandleW(None) }.map_err(|e| e.to_string())?;
    // SAFETY: 標準の矢印カーソル。
    let cursor = unsafe { LoadCursorW(None, IDC_ARROW) }.map_err(|e| e.to_string())?;
    for (name, proc) in [
        (CLASS_CTL, ctl_proc as WndProc),
        (CLASS_BOARD, board_proc),
        (CLASS_PIN, pin_proc),
    ] {
        let class = WNDCLASSW {
            lpfnWndProc: Some(proc),
            hInstance: hinstance.into(),
            hCursor: cursor,
            lpszClassName: name,
            ..Default::default()
        };
        // SAFETY: class は有効な WNDCLASSW。
        unsafe { RegisterClassW(&class) };
    }
    // SAFETY: 登録済みのクラス名。メッセージ専用窓。
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS_CTL,
            w!(""),
            Default::default(),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(hinstance.into()),
            None,
        )
    }
    .map_err(|e| format!("案内の窓の制御用の窓を作れない: {e}"))
}

type WndProc = unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT;

unsafe extern "system" fn ctl_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_HELP_WAKE {
        // 借用を持ったまま窓を作る・壊すと、窓プロシージャから UI を借り直して落ちる。
        // 1 件ずつ取り出してから処理する
        while let Some(req) = UI.with(|u| u.borrow().as_ref().and_then(|ui| ui.rx.try_recv().ok()))
        {
            match req.text {
                Some(text) => show(req.hwnd, text),
                None => hide(),
            }
        }
        return LRESULT(0);
    }
    // SAFETY: 既定の処理に渡す。
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

fn current() -> Option<HWND> {
    UI.with(|u| u.borrow().as_ref().and_then(|ui| ui.current))
}

fn set_current(h: Option<HWND>) {
    UI.with(|u| {
        if let Some(ui) = u.borrow_mut().as_mut() {
            ui.current = h;
        }
    });
}

fn hide() {
    if let Some(h) = current() {
        set_current(None);
        // SAFETY: このスレッドで作った板。
        let _ = unsafe { DestroyWindow(h) };
    }
}

// ---- 色と寸法 ----

#[derive(Clone, Copy)]
struct Theme {
    bg: COLORREF,
    border: COLORREF,
    text: COLORREF,
    hot: COLORREF,
}

const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}

/// Windows の「モード」（タスクバーや音量表示が従う方）を読む。
fn theme() -> Theme {
    let mut v: u32 = 1;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: v と size は有効な出力先。
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("SystemUsesLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut v as *mut u32 as *mut _),
            Some(&mut size),
        )
    };
    // 値が無いのは一度もモードを変えていない Windows で、そのときはライト
    if r.is_err() || v != 0 {
        Theme {
            bg: rgb(249, 249, 249),
            border: rgb(229, 229, 229),
            text: rgb(26, 26, 26),
            hot: rgb(234, 234, 234),
        }
    } else {
        Theme {
            bg: rgb(44, 44, 44),
            border: rgb(64, 64, 64),
            text: rgb(255, 255, 255),
            hot: rgb(61, 61, 61),
        }
    }
}

fn scale(v: i32, dpi: u32) -> i32 {
    v * dpi as i32 / 96
}

fn monitor_dpi(m: HMONITOR) -> u32 {
    let (mut x, mut y) = (96, 96);
    // SAFETY: x, y は有効な出力先。
    if unsafe { GetDpiForMonitor(m, MDT_EFFECTIVE_DPI, &mut x, &mut y) }.is_err() {
        return 96;
    }
    x
}

struct Fonts {
    title: HFONT,
    body: HFONT,
    icon: HFONT,
}

impl Fonts {
    fn new(dpi: u32) -> Fonts {
        let make = |px: i32, weight: i32, face: PCWSTR| {
            // SAFETY: face は NUL 終端の定数。
            unsafe {
                CreateFontW(
                    -scale(px, dpi),
                    0,
                    0,
                    0,
                    weight,
                    0,
                    0,
                    0,
                    DEFAULT_CHARSET,
                    Default::default(),
                    Default::default(),
                    Default::default(),
                    0,
                    face,
                )
            }
        };
        Fonts {
            title: make(FONT_PX, 600, w!("Segoe UI")),
            body: make(FONT_PX, 400, w!("Segoe UI")),
            icon: make(ICON_PX, 400, w!("Segoe Fluent Icons")),
        }
    }
}

impl Drop for Fonts {
    fn drop(&mut self) {
        for f in [self.title, self.body, self.icon] {
            // SAFETY: CreateFontW が作ったもの。
            let _ = unsafe { DeleteObject(HGDIOBJ(f.0)) };
        }
    }
}

/// 板の中の配置（クライアント座標）。
struct Layout {
    width: i32,
    height: i32,
    title: RECT,
    body: RECT,
    pin: RECT,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

const TEXT_FLAGS: windows::Win32::Graphics::Gdi::DRAW_TEXT_FORMAT =
    windows::Win32::Graphics::Gdi::DRAW_TEXT_FORMAT(
        DT_WORDBREAK.0 | DT_EXPANDTABS.0 | DT_NOPREFIX.0,
    );

/// `max_text` の幅に収めたときの文字列の大きさ。
fn measure(dc: HDC, font: HFONT, s: &str, max_text: i32) -> (i32, i32) {
    if s.is_empty() {
        return (0, 0);
    }
    let mut rc = RECT {
        right: max_text,
        ..Default::default()
    };
    // SAFETY: dc は有効な DC、font は有効なフォント。
    unsafe {
        let old = SelectObject(dc, HGDIOBJ(font.0));
        DrawTextW(dc, &mut wide(s), &mut rc, TEXT_FLAGS | DT_CALCRECT);
        SelectObject(dc, old);
    }
    (rc.right, rc.bottom)
}

fn layout(text: &HelpText, fonts: &Fonts, dpi: u32, work_width: i32) -> Layout {
    let pad_x = scale(PAD_X, dpi);
    let pad_y = scale(PAD_Y, dpi);
    let pin = scale(PIN, dpi);
    let inset = scale(PIN_INSET, dpi);
    let max_width = scale(MAX_WIDTH, dpi).min(work_width * 6 / 10);
    // SAFETY: 画面の DC。下で返す。
    let dc = unsafe { GetDC(None) };
    // タイトルはピンの分だけ狭い
    let title_max = max_width - pad_x - pin - inset;
    let body_max = max_width - pad_x * 2;
    let (tw, th) = measure(dc, fonts.title, &text.title, title_max);
    let (bw, bh) = measure(dc, fonts.body, &text.body, body_max);
    // SAFETY: GetDC で得た DC。
    unsafe { ReleaseDC(None, dc) };
    let width = (pad_x + tw + pin + inset)
        .max(pad_x * 2 + bw)
        .max(scale(MIN_WIDTH, dpi));
    let title_h = th.max(pin - (pad_y - inset));
    let title = RECT {
        left: pad_x,
        top: pad_y,
        right: width - pin - inset,
        bottom: pad_y + th,
    };
    let body_top = pad_y + title_h + if bh > 0 { scale(GAP, dpi) } else { 0 };
    let body = RECT {
        left: pad_x,
        top: body_top,
        right: width - pad_x,
        bottom: body_top + bh,
    };
    let pin = RECT {
        left: width - inset - pin,
        top: inset,
        right: width - inset,
        bottom: inset + pin,
    };
    Layout {
        width,
        height: body.bottom + pad_y,
        title,
        body,
        pin,
    }
}

// ---- 板 ----

/// 板に結び付ける状態。`GWLP_USERDATA` に Box の生ポインタで置く。
struct Board {
    text: HelpText,
    pinned: bool,
    /// ピン留め前の、上に重ねるピンの窓。
    pin_wnd: Option<HWND>,
    /// ピン留め後、板の中のピンにマウスが乗っているか。
    hot: bool,
    dpi: u32,
    theme: Theme,
    fonts: Fonts,
    layout: Layout,
}

/// # Safety
/// `hwnd` はこのスレッドで作った板で、まだ `WM_NCDESTROY` を過ぎていないこと。
unsafe fn board<'a>(hwnd: HWND) -> Option<&'a mut Board> {
    // SAFETY: GWLP_USERDATA には Box<Board> の生ポインタか 0 が入っている。
    unsafe { (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Board).as_mut() }
}

/// フォーカス窓がある画面の作業領域と DPI。フォーカス窓が無ければメインの画面。
fn work_area(focus: u64) -> (RECT, u32) {
    // SAFETY: どの窓ハンドルでも（無効でも）画面を返す。
    let m = unsafe {
        if focus == 0 {
            MonitorFromPoint(POINT::default(), MONITOR_DEFAULTTOPRIMARY)
        } else {
            MonitorFromWindow(HWND(focus as usize as *mut _), MONITOR_DEFAULTTONEAREST)
        }
    };
    let mut mi = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: mi は cbSize を埋めた MONITORINFO。
    let _ = unsafe { GetMonitorInfoW(m, &mut mi) };
    (mi.rcWork, monitor_dpi(m))
}

fn show(focus: u64, text: HelpText) {
    let (work, dpi) = work_area(focus);
    let theme = theme();
    let fonts = Fonts::new(dpi);
    let lay = layout(&text, &fonts, dpi, work.right - work.left);
    let x = work.left + (work.right - work.left - lay.width) / 2;
    let y = work.bottom - lay.height - scale(MARGIN_BOTTOM, dpi);
    let (w, h, pin_rc) = (lay.width, lay.height, lay.pin);

    let hwnd = match current() {
        Some(h) => {
            // SAFETY: current はこのスレッドの生きている板。
            if let Some(b) = unsafe { board(h) } {
                b.text = text;
                b.dpi = dpi;
                b.theme = theme;
                b.fonts = fonts;
                b.layout = lay;
            }
            h
        }
        None => match create_board(text, dpi, theme, fonts, lay) {
            Some(h) => {
                set_current(Some(h));
                h
            }
            None => return,
        },
    };
    place_board(hwnd, x, y, w, h, dpi);
    // SAFETY: このスレッドの板。
    if let Some(p) = unsafe { board(hwnd) }.and_then(|b| b.pin_wnd) {
        place_pin(p, x, y, pin_rc);
    }
    // SAFETY: このスレッドの板。張り直すと 5 秒を数え直す。
    unsafe { SetTimer(Some(hwnd), TIMER_ID, LIFETIME_MS, None) };
}

fn create_board(text: HelpText, dpi: u32, theme: Theme, fonts: Fonts, lay: Layout) -> Option<HWND> {
    // SAFETY: 引数なし（自分のモジュール）。
    let hinstance = unsafe { GetModuleHandleW(None) }.ok()?;
    let ex =
        WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_LAYERED | WS_EX_TRANSPARENT;
    // SAFETY: 登録済みのクラス名。位置と大きさは place_board で決める。
    let hwnd = match unsafe {
        CreateWindowExW(
            ex,
            CLASS_BOARD,
            w!("newmayu"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinstance.into()),
            None,
        )
    } {
        Ok(h) => h,
        Err(e) => {
            log!("案内の窓を作れない: {e}");
            return None;
        }
    };
    // SAFETY: 今作った窓。不透明のまま（素通りのために層付きにしているだけ）。
    let _ = unsafe { SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA) };
    // SAFETY: 板をオーナーにした、上に重なるピンの窓。
    let pin_wnd = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            CLASS_PIN,
            w!(""),
            WS_POPUP,
            0,
            0,
            0,
            0,
            Some(hwnd),
            None,
            Some(hinstance.into()),
            None,
        )
    }
    .inspect_err(|e| log!("案内の窓のピンを作れない: {e}"))
    .ok();
    let b = Box::new(Board {
        text,
        pinned: false,
        pin_wnd,
        hot: false,
        dpi,
        theme,
        fonts,
        layout: lay,
    });
    // SAFETY: Box は WM_NCDESTROY で戻して落とす。
    unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(b) as isize) };
    // SAFETY: 今作った窓。
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        if let Some(p) = pin_wnd {
            let _ = ShowWindow(p, SW_SHOWNOACTIVATE);
        }
    }
    Some(hwnd)
}

fn place_board(hwnd: HWND, x: i32, y: i32, w: i32, h: i32, dpi: u32) {
    let r = scale(RADIUS, dpi) * 2;
    // SAFETY: このスレッドの板。領域は窓が持つ（SetWindowRgn に渡したら消さない）。
    unsafe {
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_NOACTIVATE);
        SetWindowRgn(
            hwnd,
            Some(CreateRoundRectRgn(0, 0, w + 1, h + 1, r, r)),
            true,
        );
        let _ = InvalidateRect(Some(hwnd), None, true);
    }
}

fn place_pin(pin: HWND, board_x: i32, board_y: i32, rc: RECT) {
    // SAFETY: このスレッドのピンの窓。オーナー（板）より上に保たれる。
    unsafe {
        let _ = SetWindowPos(
            pin,
            Some(HWND_TOPMOST),
            board_x + rc.left,
            board_y + rc.top,
            rc.right - rc.left,
            rc.bottom - rc.top,
            SWP_NOACTIVATE,
        );
        let _ = InvalidateRect(Some(pin), None, true);
    }
}

/// ピン留めする。上に重ねていたピンの窓を消し、板が自分でクリックを受ける。
fn pin_board(hwnd: HWND) {
    if current() == Some(hwnd) {
        set_current(None);
    }
    // SAFETY: このスレッドの板。
    let Some(b) = (unsafe { board(hwnd) }) else {
        return;
    };
    b.pinned = true;
    let pin_wnd = b.pin_wnd.take();
    // SAFETY: このスレッドの窓。
    unsafe {
        let _ = KillTimer(Some(hwnd), TIMER_ID);
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) & !(WS_EX_TRANSPARENT.0 as isize);
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex);
        if let Some(p) = pin_wnd {
            let _ = DestroyWindow(p);
        }
        let _ = InvalidateRect(Some(hwnd), None, true);
    }
}

fn track_leave(hwnd: HWND) {
    let mut t = TRACKMOUSEEVENT {
        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
        dwFlags: TME_LEAVE,
        hwndTrack: hwnd,
        dwHoverTime: 0,
    };
    // SAFETY: t は cbSize を埋めた TRACKMOUSEEVENT。
    let _ = unsafe { TrackMouseEvent(&mut t) };
}

fn point_of(lp: LPARAM) -> POINT {
    POINT {
        x: (lp.0 & 0xffff) as i16 as i32,
        y: ((lp.0 >> 16) & 0xffff) as i16 as i32,
    }
}

fn inside(rc: &RECT, p: POINT) -> bool {
    p.x >= rc.left && p.x < rc.right && p.y >= rc.top && p.y < rc.bottom
}

/// ピンを描く。`rc` は描く先の DC の座標。
fn paint_pin(dc: HDC, rc: RECT, b: &Board, pinned: bool, hot: bool) {
    let r = scale(4, b.dpi) * 2;
    // SAFETY: dc は有効な DC。作ったブラシ・ペンは戻して消す。
    unsafe {
        if hot {
            let brush = CreateSolidBrush(b.theme.hot);
            let pen = CreatePen(PS_SOLID, 1, b.theme.hot);
            let ob = SelectObject(dc, HGDIOBJ(brush.0));
            let op = SelectObject(dc, HGDIOBJ(pen.0));
            let _ = RoundRect(dc, rc.left, rc.top, rc.right, rc.bottom, r, r);
            SelectObject(dc, ob);
            SelectObject(dc, op);
            let _ = DeleteObject(HGDIOBJ(brush.0));
            let _ = DeleteObject(HGDIOBJ(pen.0));
        }
        let old = SelectObject(dc, HGDIOBJ(b.fonts.icon.0));
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, b.theme.text);
        let mut rc = rc;
        let glyph = if pinned { GLYPH_PINNED } else { GLYPH_PIN };
        DrawTextW(
            dc,
            &mut wide(glyph),
            &mut rc,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );
        SelectObject(dc, old);
    }
}

fn paint_board(hwnd: HWND, b: &Board) {
    let mut ps = PAINTSTRUCT::default();
    // SAFETY: WM_PAINT の中。BeginPaint と EndPaint を対にする。
    unsafe {
        let dc = BeginPaint(hwnd, &mut ps);
        let lay = &b.layout;
        let all = RECT {
            right: lay.width,
            bottom: lay.height,
            ..Default::default()
        };
        let bg = CreateSolidBrush(b.theme.bg);
        FillRect(dc, &all, bg);
        let _ = DeleteObject(HGDIOBJ(bg.0));
        // 縁取り（角は SetWindowRgn の丸めに合わせる）
        let pen = CreatePen(PS_SOLID, 1, b.theme.border);
        let op = SelectObject(dc, HGDIOBJ(pen.0));
        let ob = SelectObject(dc, GetStockObject(NULL_BRUSH));
        let r = scale(RADIUS, b.dpi) * 2;
        let _ = RoundRect(dc, 0, 0, lay.width, lay.height, r, r);
        SelectObject(dc, op);
        SelectObject(dc, ob);
        let _ = DeleteObject(HGDIOBJ(pen.0));

        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, b.theme.text);
        for (font, s, rc) in [
            (b.fonts.title, &b.text.title, lay.title),
            (b.fonts.body, &b.text.body, lay.body),
        ] {
            if s.is_empty() {
                continue;
            }
            let old = SelectObject(dc, HGDIOBJ(font.0));
            let mut rc = rc;
            DrawTextW(dc, &mut wide(s), &mut rc, TEXT_FLAGS);
            SelectObject(dc, old);
        }
        if b.pinned {
            paint_pin(dc, lay.pin, b, true, b.hot);
        }
        let _ = EndPaint(hwnd, &ps);
    }
}

unsafe extern "system" fn board_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        WM_NCHITTEST => {
            // SAFETY: このスレッドの板。
            if let Some(b) = unsafe { board(hwnd) } {
                if !b.pinned {
                    return LRESULT(HTTRANSPARENT as isize);
                }
                let mut rc = RECT::default();
                // SAFETY: このスレッドの板。
                let _ = unsafe { GetWindowRect(hwnd, &mut rc) };
                let p = point_of(lp);
                let local = POINT {
                    x: p.x - rc.left,
                    y: p.y - rc.top,
                };
                // ピンだけはクリック、ほかはどこを掴んでも動かせる
                let hit = if inside(&b.layout.pin, local) {
                    HTCLIENT
                } else {
                    HTCAPTION
                };
                return LRESULT(hit as isize);
            }
        }
        WM_MOUSEMOVE => {
            // SAFETY: このスレッドの板。
            if let Some(b) = unsafe { board(hwnd) } {
                let hot = inside(&b.layout.pin, point_of(lp));
                if hot != b.hot {
                    b.hot = hot;
                    track_leave(hwnd);
                    // SAFETY: このスレッドの板。
                    let _ = unsafe { InvalidateRect(Some(hwnd), Some(&b.layout.pin), true) };
                }
            }
            return LRESULT(0);
        }
        WM_MOUSELEAVE => {
            // SAFETY: このスレッドの板。
            if let Some(b) = unsafe { board(hwnd) } {
                b.hot = false;
                // SAFETY: このスレッドの板。
                let _ = unsafe { InvalidateRect(Some(hwnd), Some(&b.layout.pin), true) };
            }
            return LRESULT(0);
        }
        WM_LBUTTONUP => {
            // ピン留めした板のピン（クライアント領域はそこだけ）を押したら消す
            // SAFETY: このスレッドの板。
            if unsafe { board(hwnd) }
                .is_some_and(|b| b.pinned && inside(&b.layout.pin, point_of(lp)))
            {
                // SAFETY: このスレッドの板。
                let _ = unsafe { DestroyWindow(hwnd) };
            }
            return LRESULT(0);
        }
        WM_TIMER => {
            if wp.0 == TIMER_ID && current() == Some(hwnd) {
                hide();
            }
            return LRESULT(0);
        }
        WM_DPICHANGED => {
            // ピン留めした板を DPI の違う画面へ動かしたとき。左上はそのままで作り直す
            // SAFETY: このスレッドの板。lp は WM_DPICHANGED が渡す新しい矩形。
            if let Some(b) = unsafe { board(hwnd) } {
                let dpi = (wp.0 & 0xffff) as u32;
                // SAFETY: WM_DPICHANGED の lParam は RECT へのポインタ。
                let suggested = unsafe { *(lp.0 as *const RECT) };
                let (work, _) = work_area(hwnd.0 as usize as u64);
                b.dpi = dpi;
                b.fonts = Fonts::new(dpi);
                b.layout = layout(&b.text, &b.fonts, dpi, work.right - work.left);
                place_board(
                    hwnd,
                    suggested.left,
                    suggested.top,
                    b.layout.width,
                    b.layout.height,
                    dpi,
                );
            }
            return LRESULT(0);
        }
        WM_PAINT => {
            // SAFETY: このスレッドの板。
            if let Some(b) = unsafe { board(hwnd) } {
                paint_board(hwnd, b);
                return LRESULT(0);
            }
        }
        WM_NCDESTROY => {
            if current() == Some(hwnd) {
                set_current(None);
            }
            // SAFETY: create_board で置いた Box を取り戻して落とす。以後は 0。
            unsafe {
                let p = SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) as *mut Board;
                if !p.is_null() {
                    drop(Box::from_raw(p));
                }
            }
        }
        _ => {}
    }
    // SAFETY: 既定の処理に渡す。
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

// ---- ピン留め前のピンの窓 ----

/// ピンの窓のオーナー（板）。
fn owner(pin: HWND) -> HWND {
    // SAFETY: このスレッドのピンの窓。
    unsafe { GetWindow(pin, GW_OWNER) }.unwrap_or_default()
}

thread_local! {
    /// マウスが乗っているピンの窓。
    static HOT_PIN: RefCell<Option<HWND>> = const { RefCell::new(None) };
}

unsafe extern "system" fn pin_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        WM_MOUSEMOVE => {
            if HOT_PIN.with(|h| h.replace(Some(hwnd))) != Some(hwnd) {
                track_leave(hwnd);
                // SAFETY: このスレッドのピンの窓。
                let _ = unsafe { InvalidateRect(Some(hwnd), None, true) };
            }
            return LRESULT(0);
        }
        WM_MOUSELEAVE => {
            HOT_PIN.with(|h| h.replace(None));
            // SAFETY: このスレッドのピンの窓。
            let _ = unsafe { InvalidateRect(Some(hwnd), None, true) };
            return LRESULT(0);
        }
        WM_LBUTTONUP => {
            HOT_PIN.with(|h| h.replace(None));
            pin_board(owner(hwnd));
            return LRESULT(0);
        }
        WM_PAINT => {
            let board_hwnd = owner(hwnd);
            // SAFETY: オーナーはこのスレッドの板。
            if let Some(b) = unsafe { board(board_hwnd) } {
                let mut ps = PAINTSTRUCT::default();
                let pin = &b.layout.pin;
                let rc = RECT {
                    right: pin.right - pin.left,
                    bottom: pin.bottom - pin.top,
                    ..Default::default()
                };
                let hot = HOT_PIN.with(|h| *h.borrow() == Some(hwnd));
                // SAFETY: WM_PAINT の中。BeginPaint と EndPaint を対にする。
                unsafe {
                    let dc = BeginPaint(hwnd, &mut ps);
                    let bg = CreateSolidBrush(b.theme.bg);
                    FillRect(dc, &rc, bg);
                    let _ = DeleteObject(HGDIOBJ(bg.0));
                    paint_pin(dc, rc, b, false, hot);
                    let _ = EndPaint(hwnd, &ps);
                }
                return LRESULT(0);
            }
        }
        WM_NCDESTROY => {
            HOT_PIN.with(|h| {
                if *h.borrow() == Some(hwnd) {
                    h.replace(None);
                }
            });
        }
        _ => {}
    }
    // SAFETY: 既定の処理に渡す。
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

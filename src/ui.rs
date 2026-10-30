//! タスクトレイ。
//!
//! 見えない窓を 1 つ作り、`Shell_NotifyIconW` でアイコンを置く。アイコンを
//! クリックするとポップアップメニューを出し、選ばれた項目を [`Command`] として
//! 作成時に渡された関数に渡す。
//!
//! 窓はメッセージループを回しているスレッドで作ること（`Tray::new` を呼んだ
//! スレッドにメッセージが届く）。

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CW_USEDEFAULT, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
    DestroyWindow, GWLP_USERDATA, GetCursorPos, GetWindowLongPtrW, IDI_APPLICATION, LoadIconW,
    MF_STRING, PostMessageW, RegisterClassW, RegisterWindowMessageW, SetForegroundWindow,
    SetWindowLongPtrW, TPM_BOTTOMALIGN, TPM_NONOTIFY, TPM_RETURNCMD, TrackPopupMenu,
    WINDOW_EX_STYLE, WM_APP, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP, WNDCLASSW, WS_OVERLAPPED,
};
use windows::core::{BOOL, w};

/// メニューで選ばれたもの。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Exit,
}

/// トレイアイコンの通知が届くメッセージ。
const WM_TRAY: u32 = WM_APP + 1;
const ICON_ID: u32 = 1;
const ID_EXIT: usize = 3;

type Handler = Box<dyn FnMut(Command)>;

/// 窓に結び付ける状態。`GWLP_USERDATA` に Box の生ポインタで置く。
struct State {
    handler: Handler,
    tip: String,
    /// Explorer が立ち上がり直したときに届く `TaskbarCreated`。
    taskbar_created: u32,
}

/// タスクトレイのアイコン。落とすと消える。
pub struct Tray {
    hwnd: HWND,
    /// 窓から参照される。窓より長く生きる必要があるのでここで持つ。
    _state: Box<State>,
}

impl Tray {
    /// アイコンを置く。`tip` はマウスを乗せたときの文字。
    pub fn new(tip: &str, handler: Handler) -> Result<Tray, String> {
        // SAFETY: 引数なし（自分のモジュール）。
        let hinstance = unsafe { GetModuleHandleW(None) }.map_err(|e| e.to_string())?;
        let class = WNDCLASSW {
            lpfnWndProc: Some(wnd_proc),
            hInstance: hinstance.into(),
            lpszClassName: w!("newmayu_tray"),
            ..Default::default()
        };
        // SAFETY: class は有効な WNDCLASSW。同じ名前を二度登録すると 0 が返るが、
        // その場合も既存のクラスで窓は作れる。
        unsafe { RegisterClassW(&class) };
        // SAFETY: 文字列は NUL 終端の定数。
        let taskbar_created = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
        let mut state = Box::new(State {
            handler,
            tip: tip.to_string(),
            taskbar_created,
        });
        // SAFETY: 登録済みのクラス名。見せない窓なので位置・大きさは任せる。
        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("newmayu_tray"),
                w!("newmayu"),
                WS_OVERLAPPED,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                0,
                0,
                None,
                None,
                Some(hinstance.into()),
                None,
            )
        }
        .map_err(|e| format!("トレイの窓を作れない: {e}"))?;
        // SAFETY: hwnd は今作った窓。state は Tray が持ち続けるので窓より長く生きる。
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, &mut *state as *mut State as isize) };
        if !add_icon(hwnd, tip) {
            // SAFETY: 今作った窓。
            let _ = unsafe { DestroyWindow(hwnd) };
            return Err("トレイにアイコンを置けない".to_string());
        }
        Ok(Tray {
            hwnd,
            _state: state,
        })
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        let data = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: ICON_ID,
            ..Default::default()
        };
        // SAFETY: data は有効な NOTIFYICONDATAW。
        let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &data) };
        // SAFETY: new で作った窓。USERDATA を外してから壊す。
        unsafe {
            SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

/// アイコンを置く。すでにあるときも同じ。
fn add_icon(hwnd: HWND, tip: &str) -> bool {
    // SAFETY: 標準アイコン。hinstance なし。
    let icon = unsafe { LoadIconW(None, IDI_APPLICATION) }.unwrap_or_default();
    let mut data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: ICON_ID,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
        uCallbackMessage: WM_TRAY,
        hIcon: icon,
        ..Default::default()
    };
    for (dst, src) in data.szTip.iter_mut().zip(tip.encode_utf16().take(127)) {
        *dst = src;
    }
    // SAFETY: data は有効な NOTIFYICONDATAW。
    unsafe { Shell_NotifyIconW(NIM_ADD, &data) }.as_bool()
}

/// メニューを出し、選ばれた項目を返す。何も選ばれなければ `None`。
fn popup(hwnd: HWND) -> Option<Command> {
    let mut pt = Default::default();
    // SAFETY: pt は有効な POINT。
    unsafe { GetCursorPos(&mut pt) }.ok()?;
    // SAFETY: 引数なし。
    let menu = unsafe { CreatePopupMenu() }.ok()?;
    // SAFETY: menu は今作ったメニュー。文字列は NUL 終端の定数。
    let built = unsafe { AppendMenuW(menu, MF_STRING, ID_EXIT, w!("終了")) }.is_ok();
    let mut chosen = BOOL(0);
    if built {
        // SAFETY: hwnd は自分の窓。メニューを閉じるためにいったん前面に出す（MS の指示）。
        unsafe {
            let _ = SetForegroundWindow(hwnd);
            chosen = TrackPopupMenu(
                menu,
                TPM_BOTTOMALIGN | TPM_RETURNCMD | TPM_NONOTIFY,
                pt.x,
                pt.y,
                None,
                hwnd,
                None,
            );
            let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        }
    }
    // SAFETY: 今作ったメニュー。
    let _ = unsafe { DestroyMenu(menu) };
    match chosen.0 as usize {
        ID_EXIT => Some(Command::Exit),
        _ => None,
    }
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: USERDATA には Tray::new が置いた State の生ポインタか 0 が入っている。
    // State は Tray が持ち、Drop で窓を壊す前に 0 に戻すので、0 でなければ有効。
    let state = unsafe { (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State).as_mut() };
    if let Some(state) = state {
        if msg == WM_TRAY {
            let event = (lparam.0 & 0xffff) as u32;
            if (event == WM_RBUTTONUP || event == WM_LBUTTONUP)
                && let Some(cmd) = popup(hwnd)
            {
                (state.handler)(cmd);
            }
            return LRESULT(0);
        }
        if msg == state.taskbar_created {
            add_icon(hwnd, &state.tip);
            return LRESULT(0);
        }
    }
    // SAFETY: 引数はそのまま既定の処理へ。
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

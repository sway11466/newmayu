//! フック DLL。yamy の hook.cpp に当たる。
//!
//! 本体が `newmayu_hook_install` を呼ぶと、`WH_GETMESSAGE` と `WH_CALLWNDPROC` を全スレッド対象で
//! 掛ける。OS がこの DLL を各 GUI プロセスに読み込み、そのプロセスの窓メッセージがここを通る。
//! ここで IME の状態（オン・変換中）の変化を見つけ、本体のメールスロットに通知を書く。
//!
//! 他プロセスの中で動くので、やることを最小にする:
//! - 状態は atomic の static だけ。ヒープも std のスレッドローカルも使わない
//! - panic は FFI の境界を越えさせない（境界の関数は値を返すだけ）
//! - メールスロットは最初に書くときに開き、失敗したら次の機会にまた開く

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};

use newmayu_ipc::{IME_COMPOSING, IME_OPEN, Kind, MAILSLOT_NAME, Notify};
use windows::Win32::Foundation::{
    GENERIC_WRITE, HANDLE, HINSTANCE, HWND, INVALID_HANDLE_VALUE, LPARAM, LRESULT, WPARAM,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, OPEN_EXISTING, WriteFile,
};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::Ime::{
    IMN_SETOPENSTATUS, ImmGetContext, ImmGetOpenStatus, ImmReleaseContext,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CWPSTRUCT, CallNextHookEx, HHOOK, MSG, PM_REMOVE, SetWindowsHookExW, UnhookWindowsHookEx,
    WH_CALLWNDPROC, WH_GETMESSAGE, WM_IME_ENDCOMPOSITION, WM_IME_NOTIFY, WM_IME_STARTCOMPOSITION,
    WM_KEYDOWN, WM_KEYUP, WM_SETFOCUS, WM_SYSKEYDOWN, WM_SYSKEYUP,
};
use windows::core::PCWSTR;

/// この DLL のモジュールハンドル。`SetWindowsHookExW` に渡す。
static MODULE: AtomicIsize = AtomicIsize::new(0);
/// 掛けたフック（本体プロセスの中だけで意味がある）。
static HOOK_GET_MESSAGE: AtomicIsize = AtomicIsize::new(0);
static HOOK_CALL_WND_PROC: AtomicIsize = AtomicIsize::new(0);

/// メールスロットの書き込みハンドル（プロセスごと）。0 = まだ開いていない。
static MAILSLOT: AtomicIsize = AtomicIsize::new(0);
/// このプロセスで最後に通知した IME の状態。
static IME_OPEN_STATE: AtomicBool = AtomicBool::new(false);
static IME_COMPOSING_STATE: AtomicBool = AtomicBool::new(false);
static IME_KNOWN: AtomicBool = AtomicBool::new(false);
/// デバッグ用: 送れなかった回数。
static SEND_FAILURES: AtomicU32 = AtomicU32::new(0);

/// OS が DLL を読み込むときに呼ぶ。モジュールハンドルを覚えるだけ。
#[unsafe(no_mangle)]
pub extern "system" fn DllMain(
    hinst: HINSTANCE,
    reason: u32,
    _reserved: *mut core::ffi::c_void,
) -> i32 {
    const DLL_PROCESS_ATTACH: u32 = 1;
    if reason == DLL_PROCESS_ATTACH {
        MODULE.store(hinst.0 as isize, Ordering::Relaxed);
    }
    1
}

/// 本体から呼ぶ: 2 本のフックを掛ける。成功で 0。
#[unsafe(no_mangle)]
pub extern "system" fn newmayu_hook_install() -> i32 {
    let module = HINSTANCE(MODULE.load(Ordering::Relaxed) as *mut _);
    // SAFETY: 関数は各フックの規約どおり。module はこの DLL のハンドル。
    let gm = unsafe { SetWindowsHookExW(WH_GETMESSAGE, Some(get_message_proc), Some(module), 0) };
    let Ok(gm) = gm else {
        return 1;
    };
    // SAFETY: 同上。
    let cw = unsafe { SetWindowsHookExW(WH_CALLWNDPROC, Some(call_wnd_proc), Some(module), 0) };
    let Ok(cw) = cw else {
        // SAFETY: 直前に掛けた有効なハンドル。
        let _ = unsafe { UnhookWindowsHookEx(gm) };
        return 2;
    };
    HOOK_GET_MESSAGE.store(gm.0 as isize, Ordering::Relaxed);
    HOOK_CALL_WND_PROC.store(cw.0 as isize, Ordering::Relaxed);
    0
}

/// 本体から呼ぶ: フックを外す。既に入っている他プロセスの DLL は、次のメッセージで外れる。
#[unsafe(no_mangle)]
pub extern "system" fn newmayu_hook_uninstall() -> i32 {
    for h in [&HOOK_GET_MESSAGE, &HOOK_CALL_WND_PROC] {
        let v = h.swap(0, Ordering::Relaxed);
        if v != 0 {
            // SAFETY: install で得た有効なハンドル。
            let _ = unsafe { UnhookWindowsHookEx(HHOOK(v as *mut _)) };
        }
    }
    0
}

/// `WH_GETMESSAGE`: スレッドがキューから取り出したメッセージ。
unsafe extern "system" fn get_message_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && wparam.0 as u32 == PM_REMOVE.0 && lparam.0 != 0 {
        // SAFETY: WH_GETMESSAGE では lparam は MSG を指す。
        let msg = unsafe { &*(lparam.0 as *const MSG) };
        on_message(msg.hwnd, msg.message, msg.wParam, msg.lParam, 0);
    }
    // SAFETY: 引数はそのまま次のフックへ渡す。
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// `WH_CALLWNDPROC`: 窓プロシージャに送られたメッセージ。
unsafe extern "system" fn call_wnd_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && lparam.0 != 0 {
        // SAFETY: WH_CALLWNDPROC では lparam は CWPSTRUCT を指す。
        let c = unsafe { &*(lparam.0 as *const CWPSTRUCT) };
        on_message(c.hwnd, c.message, c.wParam, c.lParam, 10);
    }
    // SAFETY: 引数はそのまま次のフックへ渡す。
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// 両フック共通。`cause_base` は通知のデバッグ番号の下駄（0 = GetMessage、10 = CallWndProc）。
fn on_message(hwnd: HWND, message: u32, wparam: WPARAM, _lparam: LPARAM, cause_base: u32) {
    match message {
        WM_IME_STARTCOMPOSITION => {
            IME_COMPOSING_STATE.store(true, Ordering::Relaxed);
            notify_ime(hwnd, cause_base + 1);
        }
        WM_IME_ENDCOMPOSITION => {
            IME_COMPOSING_STATE.store(false, Ordering::Relaxed);
            notify_ime(hwnd, cause_base + 2);
        }
        WM_IME_NOTIFY if wparam.0 as u32 == IMN_SETOPENSTATUS => {
            refresh_open_status(hwnd, cause_base + 3, true);
        }
        WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP => {
            refresh_open_status(hwnd, cause_base + 4, false);
        }
        WM_SETFOCUS => {
            refresh_open_status(hwnd, cause_base + 5, true);
        }
        _ => {}
    }
}

/// IME がオンかを問い直し、変わっていれば（`force` なら必ず）通知する。
fn refresh_open_status(hwnd: HWND, cause: u32, force: bool) {
    // SAFETY: hwnd はメッセージの宛先。コンテキストは必ず返す。
    let himc = unsafe { ImmGetContext(hwnd) };
    if himc.0.is_null() {
        return;
    }
    // SAFETY: 直前に得た有効なコンテキスト。
    let open = unsafe { ImmGetOpenStatus(himc) }.as_bool();
    // SAFETY: 同上。
    let _ = unsafe { ImmReleaseContext(hwnd, himc) };
    let prev = IME_OPEN_STATE.swap(open, Ordering::Relaxed);
    let known = IME_KNOWN.swap(true, Ordering::Relaxed);
    if force || !known || prev != open {
        notify_ime(hwnd, cause);
    }
}

fn notify_ime(hwnd: HWND, cause: u32) {
    let mut flags = 0;
    if IME_OPEN_STATE.load(Ordering::Relaxed) {
        flags |= IME_OPEN;
    }
    if IME_COMPOSING_STATE.load(Ordering::Relaxed) {
        flags |= IME_COMPOSING;
    }
    let n = Notify {
        kind: Kind::Ime,
        cause,
        // SAFETY: 引数なしの単純な API。
        thread_id: unsafe { GetCurrentThreadId() },
        hwnd: hwnd.0 as usize as u64,
        flags,
    };
    send(&n.to_bytes());
}

/// メールスロットに 1 通知を書く。開いていなければ開く。失敗は数えるだけ。
fn send(bytes: &[u8]) {
    let mut h = MAILSLOT.load(Ordering::Relaxed);
    if h == 0 {
        h = open_mailslot();
        if h == 0 {
            SEND_FAILURES.fetch_add(1, Ordering::Relaxed);
            return;
        }
        MAILSLOT.store(h, Ordering::Relaxed);
    }
    // SAFETY: h は CreateFileW が返した有効なハンドル。
    if unsafe { WriteFile(HANDLE(h as *mut _), Some(bytes), None, None) }.is_err() {
        // 本体が居なくなった等。閉じて次に開き直す
        MAILSLOT.store(0, Ordering::Relaxed);
        // SAFETY: 同上。
        let _ = unsafe { windows::Win32::Foundation::CloseHandle(HANDLE(h as *mut _)) };
        SEND_FAILURES.fetch_add(1, Ordering::Relaxed);
    }
}

/// 開けなければ 0。
fn open_mailslot() -> isize {
    let mut name: [u16; 64] = [0; 64];
    for (i, u) in MAILSLOT_NAME.encode_utf16().enumerate() {
        name[i] = u;
    }
    // SAFETY: name は NUL 終端の UTF-16。
    let h = unsafe {
        CreateFileW(
            PCWSTR(name.as_ptr()),
            GENERIC_WRITE.0,
            FILE_SHARE_READ,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    };
    match h {
        Ok(h) if h != INVALID_HANDLE_VALUE => h.0 as isize,
        _ => 0,
    }
}

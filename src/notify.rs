//! フック DLL（`newmayu-hook`）との結線。
//!
//! - `HookDll`: 本体の隣にある `newmayu_hook.dll` を読み込み、フックを掛ける・外す
//! - `Mailslot`: DLL が通知を書き込むメールスロット。本体が作り、読み取りスレッドで読む
//!
//! 通知の形は `newmayu_ipc::Notify`。

use std::path::PathBuf;

use newmayu_ipc::{MAILSLOT_NAME, MAX_SIZE, Notify};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HMODULE};
use windows::Win32::Storage::FileSystem::ReadFile;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Mailslots::CreateMailslotW;
use windows::core::{HSTRING, PCSTR};

const DLL_FILE: &str = "newmayu_hook.dll";

type HookFn = unsafe extern "system" fn() -> i32;

/// 読み込んだフック DLL。落とすときにフックを外す。
pub struct HookDll {
    _module: HMODULE,
    uninstall: HookFn,
}

impl HookDll {
    /// 実行ファイルと同じフォルダの DLL を読み込み、フックを掛ける。
    pub fn install() -> Result<HookDll, String> {
        let path = dll_path()?;
        // SAFETY: path は NUL 終端の UTF-16（HSTRING）。
        let module = unsafe { LoadLibraryW(&HSTRING::from(path.as_os_str())) }
            .map_err(|e| format!("{} を読み込めない: {e}", path.display()))?;
        let install = proc(module, c"newmayu_hook_install")?;
        let uninstall = proc(module, c"newmayu_hook_uninstall")?;
        // SAFETY: DLL が公開している引数なしの関数。
        let r = unsafe { install() };
        if r != 0 {
            return Err(format!("フックを掛けられない（{r}）"));
        }
        Ok(HookDll {
            _module: module,
            uninstall,
        })
    }
}

impl Drop for HookDll {
    fn drop(&mut self) {
        // SAFETY: install と同じ DLL の関数。
        let _ = unsafe { (self.uninstall)() };
    }
}

fn dll_path() -> Result<PathBuf, String> {
    let exe =
        std::env::current_exe().map_err(|e| format!("実行ファイルの場所が分からない: {e}"))?;
    let dir = exe
        .parent()
        .ok_or_else(|| "実行ファイルの場所が分からない".to_string())?;
    Ok(dir.join(DLL_FILE))
}

fn proc(module: HMODULE, name: &std::ffi::CStr) -> Result<HookFn, String> {
    // SAFETY: name は NUL 終端。
    let p = unsafe { GetProcAddress(module, PCSTR(name.as_ptr() as *const u8)) }
        .ok_or_else(|| format!("{DLL_FILE} に {} が無い", name.to_string_lossy()))?;
    // SAFETY: DLL 側の宣言は extern "system" fn() -> i32。
    Ok(unsafe { std::mem::transmute::<unsafe extern "system" fn() -> isize, HookFn>(p) })
}

/// 通知を受けるメールスロット。
pub struct Mailslot {
    handle: HANDLE,
}

impl Mailslot {
    pub fn create() -> Result<Mailslot, String> {
        // SAFETY: 名前は NUL 終端の UTF-16。長さの上限なし（0）、読み取りは待ち続ける（MAILSLOT_WAIT_FOREVER）。
        let handle = unsafe { CreateMailslotW(&HSTRING::from(MAILSLOT_NAME), 0, u32::MAX, None) }
            .map_err(|e| format!("メールスロットを作れない: {e}"))?;
        Ok(Mailslot { handle })
    }

    /// 1 通知を待って読む。形が違うものは読み飛ばす。読めなくなったら `None`。
    pub fn read(&self) -> Option<Notify> {
        let mut buf = [0u8; MAX_SIZE];
        loop {
            let mut len = 0u32;
            // SAFETY: handle は有効なメールスロット。buf は十分な長さ。
            let r = unsafe { ReadFile(self.handle, Some(&mut buf), Some(&mut len), None) };
            if r.is_err() {
                return None;
            }
            if let Some(n) = Notify::from_bytes(&buf[..len as usize]) {
                return Some(n);
            }
        }
    }
}

impl Drop for Mailslot {
    fn drop(&mut self) {
        // SAFETY: create で得た有効なハンドル。
        let _ = unsafe { CloseHandle(self.handle) };
    }
}

// HANDLE を別スレッドで読むため。ハンドルはプロセス内で共有できる。
unsafe impl Send for Mailslot {}

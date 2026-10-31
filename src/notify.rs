//! フック DLL（`newmayu-hook`）との結線。
//!
//! - `HookDll`: 本体の隣にある `newmayu_hook.dll` を読み込み、フックを掛ける・外す
//! - `Mailslot`: DLL が通知を書き込むメールスロット。本体が作り、読み取りスレッドで読む
//! - `SyncShare`: `&Sync` の同期キー（`def sync`）を DLL に知らせる共有メモリ
//!
//! 通知の形は `newmayu_ipc::Notify`。

use std::path::PathBuf;

use newmayu_ipc::{
    MAILSLOT_NAME, MAX_SIZE, Notify, SYNC_SHARE_NAME, SYNC_SHARE_SIZE, write_sync_share,
};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HMODULE, INVALID_HANDLE_VALUE};
use windows::Win32::Storage::FileSystem::ReadFile;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Mailslots::CreateMailslotW;
use windows::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_WRITE, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile, PAGE_READWRITE,
    UnmapViewOfFile,
};

use crate::model::{Key, ScanPrefix};
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

/// `&Sync` の同期キーを DLL に知らせる共有メモリ。本体が作り、`def sync` の値を書く。
pub struct SyncShare {
    handle: HANDLE,
    view: MEMORY_MAPPED_VIEW_ADDRESS,
}

impl SyncShare {
    pub fn create(key: Option<Key>) -> Result<SyncShare, String> {
        // SAFETY: 名前は NUL 終端の UTF-16。ページングファイルに取る小さな領域。
        let handle = unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                None,
                PAGE_READWRITE,
                0,
                SYNC_SHARE_SIZE as u32,
                &HSTRING::from(SYNC_SHARE_NAME),
            )
        }
        .map_err(|e| format!("同期キーの共有メモリを作れない: {e}"))?;
        // SAFETY: handle は有効なファイルマッピング。
        let view = unsafe { MapViewOfFile(handle, FILE_MAP_WRITE, 0, 0, SYNC_SHARE_SIZE) };
        if view.Value.is_null() {
            // SAFETY: 直前に作った有効なハンドル。
            let _ = unsafe { CloseHandle(handle) };
            return Err("同期キーの共有メモリを写せない".to_string());
        }
        let s = SyncShare { handle, view };
        s.set(key);
        Ok(s)
    }

    /// 同期キーを書く（設定の再読込で変わったとき用）。`None` なら「無し」。
    pub fn set(&self, key: Option<Key>) {
        let mut buf = [0u8; SYNC_SHARE_SIZE];
        if let Some(k) = key {
            write_sync_share(&mut buf, u32::from(k.code), k.prefix != ScanPrefix::None);
        }
        // SAFETY: view は SYNC_SHARE_SIZE バイトの書ける領域。
        unsafe { (self.view.Value as *mut [u8; SYNC_SHARE_SIZE]).write_volatile(buf) };
    }
}

impl Drop for SyncShare {
    fn drop(&mut self) {
        // SAFETY: create で得た有効な写しとハンドル。
        unsafe {
            let _ = UnmapViewOfFile(self.view);
            let _ = CloseHandle(self.handle);
        }
    }
}

// エンジンスレッドに渡すため。ハンドルと写しはプロセス内で共有できる。
unsafe impl Send for SyncShare {}

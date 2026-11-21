//! ログダイアログ（yamy の `dlglog.cpp`）。
//!
//! 起動時に隠したまま作り、ログ（[`crate::log`]）を溜め続ける。トレイの「ログ」と
//! `&MayuDialog(Log, …)` で出し、「閉じる」で隠す。形は yamy の `mayu.rc` の `IDD_DIALOG_log` と同じ。
//! - 「ログのクリア」（`&LogClear` も）: 中身を消し、見出し（[`banner`]）を出し直す
//! - 「フォントの変更」: 選んだフォントにし、`yamy.ini` の `logFont` に書く
//! - 「詳細」: 詳細のログ（キー変換の過程・フォーカスの変化など）も出す
//!
//! yamy は「フォントの変更」で選んだフォントをログの欄ではなくダイアログに付けるので、
//! 次に起動するまでログの文字が変わらない。バグと想定し、ログの欄に付けるようにした。

use std::sync::OnceLock;
use std::sync::atomic::{AtomicIsize, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Globalization::{
    DATE_LONGDATE, GetDateFormatEx, GetTimeFormatEx, TIME_FORMAT_FLAGS,
};
use windows::Win32::Graphics::Gdi::{CreateFontIndirectW, DeleteObject, HFONT, LOGFONTW};
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::Controls::Dialogs::{
    CF_INITTOLOGFONTSTRUCT, CF_SCREENFONTS, CHOOSEFONTW, ChooseFontW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BM_GETCHECK, DestroyWindow, GetWindowRect, GetWindowTextLengthW, ICON_BIG, ICON_SMALL,
    IDI_APPLICATION, LoadIconW, MINMAXINFO, MoveWindow, PostMessageW, SW_HIDE, SW_RESTORE, SW_SHOW,
    SW_SHOWDEFAULT, SW_SHOWMAXIMIZED, SW_SHOWNORMAL, SendMessageW, SetForegroundWindow, ShowWindow,
    WM_APP, WM_CLOSE, WM_COMMAND, WM_GETMINMAXINFO, WM_INITDIALOG, WM_NCDESTROY, WM_SETFONT,
    WM_SETICON, WM_SIZE,
};
use windows::core::PCWSTR;

use crate::dialog::*;
use crate::log;
use crate::model::ShowCommand;
use crate::setting::Ini;

const IDC_EDIT_LOG: i32 = 1201;
const IDC_BUTTON_CLEAR_LOG: i32 = 1202;
const IDC_BUTTON_CHANGE_FONT: i32 = 1203;
const IDC_CHECK_DETAIL: i32 = 1204;

/// 溜まったログを受け取る。
const WM_LOG_APPEND: u32 = WM_APP + 10;
/// `&MayuDialog(Log, …)`。wparam は `SW_*`。
const WM_LOG_SHOW: u32 = WM_APP + 11;

// エディットのメッセージ
const EM_SETSEL: u32 = 0xB1;
const EM_LINEINDEX: u32 = 0xBB;
const EM_LINELENGTH: u32 = 0xC1;
const EM_REPLACESEL: u32 = 0xC2;
const EM_SETLIMITTEXT: u32 = 0xC5;
const BST_CHECKED: isize = 1;

/// 中身がこれを超えたら古い 3 分の 2 を消す（yamy と同じ 65000 文字）。
const THRESHOLD: usize = 65000;
/// `logFont` が無いときのフォント（yamy の `IDS_logFont`）。
const DEFAULT_FONT: &str = "-13,0,0,0,400,0,0,0,128,1,2,1,1,Terminal";
const RULE: &str = "------------------------------------------------------------";

/// ダイアログの窓（無ければ 0）。
static HWND_LOG: AtomicIsize = AtomicIsize::new(0);
/// 起動した時刻（見出しの `started at`）。
static STARTED: OnceLock<String> = OnceLock::new();

fn dlg() -> Option<HWND> {
    match HWND_LOG.load(Ordering::SeqCst) {
        0 => None,
        h => Some(HWND(h as *mut _)),
    }
}

/// ダイアログを隠したまま作り、ログを受け取り始める。メッセージループを回すスレッドで呼ぶ。
pub fn create(ini: Ini) -> Result<(), String> {
    let mut t = Template::new(
        DS_CENTERMOUSE | WS_MINIMIZEBOX | WS_POPUP | WS_CAPTION | WS_SYSMENU | WS_THICKFRAME,
        (0, 0, 250, 62),
        "ログ - newmayu",
    );
    t.item(
        Class::Atom(BUTTON),
        "閉じる(&C)",
        IDOK,
        (192, 44, 50, 15),
        BS_DEFPUSHBUTTON | WS_TABSTOP,
    )
    .item(
        Class::Atom(EDIT),
        "",
        IDC_EDIT_LOG,
        (0, 0, 250, 40),
        ES_MULTILINE
            | ES_AUTOVSCROLL
            | ES_AUTOHSCROLL
            | ES_NOHIDESEL
            | ES_READONLY
            | ES_WANTRETURN
            | WS_VSCROLL
            | WS_HSCROLL
            | WS_BORDER
            | WS_TABSTOP,
    )
    .item_ex(
        Class::Atom(BUTTON),
        "ログのクリア(&L)",
        IDC_BUTTON_CLEAR_LOG,
        (4, 44, 69, 15),
        BS_PUSHBUTTON | WS_TABSTOP,
        WS_EX_CLIENTEDGE,
    )
    .item_ex(
        Class::Atom(BUTTON),
        "フォントの変更(&F)...",
        IDC_BUTTON_CHANGE_FONT,
        (76, 44, 69, 15),
        BS_PUSHBUTTON | WS_TABSTOP,
        WS_EX_CLIENTEDGE,
    )
    .item(
        Class::Atom(BUTTON),
        "詳細(&D)",
        IDC_CHECK_DETAIL,
        (149, 47, 39, 10),
        BS_AUTOCHECKBOX | WS_TABSTOP,
    );
    let ini = Box::new(ini);
    let hwnd = t
        .create(log_proc, LPARAM(Box::into_raw(ini) as isize))
        .ok_or("ログダイアログを作れない")?;
    HWND_LOG.store(hwnd.0 as isize, Ordering::SeqCst);
    if let Some(edit) = item(hwnd, IDC_EDIT_LOG) {
        send(edit, EM_SETLIMITTEXT, 0, 0);
    }
    let h = hwnd.0 as isize;
    log::attach_view(Box::new(move || {
        // SAFETY: ダイアログの窓へ知らせるだけ。窓が無くなっていれば失敗して終わる。
        let _ =
            unsafe { PostMessageW(Some(HWND(h as *mut _)), WM_LOG_APPEND, WPARAM(0), LPARAM(0)) };
    }));
    Ok(())
}

/// ダイアログを壊す。ログはもう受け取らない。
pub fn destroy() {
    log::detach_view();
    if let Some(h) = dlg() {
        HWND_LOG.store(0, Ordering::SeqCst);
        // SAFETY: create で作った窓。作ったスレッドから呼ぶ。
        let _ = unsafe { DestroyWindow(h) };
    }
}

/// メッセージループの `IsDialogMessage` に渡す窓。
pub fn hwnd() -> Option<HWND> {
    dlg()
}

/// トレイの「ログ」・読み込みの失敗: 出して前に持ってくる（yamy と同じ）。
pub fn show() {
    if let Some(h) = dlg() {
        // SAFETY: ダイアログの窓。
        unsafe {
            let _ = ShowWindow(h, SW_SHOW);
            let _ = SetForegroundWindow(h);
        }
    }
}

/// `&MayuDialog(Log, 出し方)`。どのスレッドからでも呼べる。
pub fn post_show(s: ShowCommand) {
    if let Some(h) = dlg() {
        let sw = crate::window::show_command(s);
        // SAFETY: ダイアログの窓へ知らせるだけ。
        let _ = unsafe { PostMessageW(Some(h), WM_LOG_SHOW, WPARAM(sw.0 as usize), LPARAM(0)) };
    }
}

/// `&LogClear`: 「ログのクリア」を押したのと同じ。どのスレッドからでも呼べる。
pub fn post_clear() {
    if let Some(h) = dlg() {
        // SAFETY: ダイアログの窓へ知らせるだけ。
        let _ = unsafe {
            PostMessageW(
                Some(h),
                WM_COMMAND,
                WPARAM(IDC_BUTTON_CLEAR_LOG as usize),
                LPARAM(0),
            )
        };
    }
}

/// ログの見出し（yamy の `showBanner`）。起動時は `cleared = false`、クリアの後は `true`。
pub fn banner(cleared: bool) {
    let now = now_text();
    let started = STARTED.get_or_init(|| now.clone());
    let debug = if cfg!(debug_assertions) {
        " (DEBUG)"
    } else {
        ""
    };
    crate::log!("{RULE}");
    crate::log!("newmayu {}{debug} (UNICODE)", env!("CARGO_PKG_VERSION"));
    crate::log!("  built by {}", env!("NEWMAYU_BUILT_BY"));
    crate::log!("started at {started}");
    crate::log!("{}", module_path());
    crate::log!("{RULE}");
    if cleared {
        crate::log!("log was cleared at {now}");
    } else {
        crate::log!("log begins at {now}");
    }
}

/// 今の日時（yamy の `%#c`。地域の長い日付と時刻）。
fn now_text() -> String {
    // SAFETY: 引数なし。
    let t = unsafe { GetLocalTime() };
    let mut date = [0u16; 128];
    let mut time = [0u16; 128];
    // SAFETY: 書き込み先は配列。地域は利用者の既定（NULL）。
    let (d, tm) = unsafe {
        (
            GetDateFormatEx(
                PCWSTR::null(),
                DATE_LONGDATE,
                Some(&t),
                PCWSTR::null(),
                Some(&mut date),
                PCWSTR::null(),
            ),
            GetTimeFormatEx(
                PCWSTR::null(),
                TIME_FORMAT_FLAGS(0),
                Some(&t),
                PCWSTR::null(),
                Some(&mut time),
            ),
        )
    };
    let s = |b: &[u16], n: i32| String::from_utf16_lossy(&b[..(n.max(1) - 1) as usize]);
    format!("{} {}", s(&date, d), s(&time, tm))
}

/// 自分の exe のパス。
fn module_path() -> String {
    let mut buf = [0u16; 260 * 4];
    // SAFETY: 書き込み先は配列。自分のモジュール。
    let n = unsafe { GetModuleFileNameW(None, &mut buf) } as usize;
    String::from_utf16_lossy(&buf[..n])
}

/// `LOGFONT` を `,` で並べた形から読む（yamy の `string2logfont`）。
fn parse_font(s: &str) -> Option<LOGFONTW> {
    let mut it = s.splitn(14, ',');
    let mut n = [0i32; 13];
    for v in &mut n {
        *v = it.next()?.parse().ok()?;
    }
    let face = it.next().filter(|f| !f.is_empty())?;
    let mut lf = LOGFONTW {
        lfHeight: n[0],
        lfWidth: n[1],
        lfEscapement: n[2],
        lfOrientation: n[3],
        lfWeight: n[4],
        lfItalic: n[5] as u8,
        lfUnderline: n[6] as u8,
        lfStrikeOut: n[7] as u8,
        ..Default::default()
    };
    lf.lfCharSet.0 = n[8] as u8;
    lf.lfOutPrecision.0 = n[9] as u8;
    lf.lfClipPrecision.0 = n[10] as u8;
    lf.lfQuality.0 = n[11] as u8;
    lf.lfPitchAndFamily = n[12] as u8;
    // 入りきらなければ切る（yamy の tcslcpy）
    for (d, c) in lf.lfFaceName[..31].iter_mut().zip(face.encode_utf16()) {
        *d = c;
    }
    Some(lf)
}

/// `LOGFONT` を `,` で並べた形にする（yamy の Registry::write の LOGFONT）。
fn format_font(lf: &LOGFONTW) -> String {
    let n = lf.lfFaceName.iter().position(|&c| c == 0).unwrap_or(32);
    format!(
        "{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
        lf.lfHeight,
        lf.lfWidth,
        lf.lfEscapement,
        lf.lfOrientation,
        lf.lfWeight,
        lf.lfItalic,
        lf.lfUnderline,
        lf.lfStrikeOut,
        lf.lfCharSet.0,
        lf.lfOutPrecision.0,
        lf.lfClipPrecision.0,
        lf.lfQuality.0,
        lf.lfPitchAndFamily,
        String::from_utf16_lossy(&lf.lfFaceName[..n])
    )
}

fn send(hwnd: HWND, msg: u32, w: usize, l: isize) -> isize {
    // SAFETY: hwnd はダイアログかその子。
    unsafe { SendMessageW(hwnd, msg, Some(WPARAM(w)), Some(LPARAM(l))) }.0
}

/// 末尾に足す。改行は `\r\n` にする。長くなったら古い 3 分の 2 を消す（yamy の `editInsertTextAtLast`）。
fn append(edit: HWND, text: &str) {
    if text.is_empty() {
        return;
    }
    // SAFETY: edit はダイアログのエディット。
    let mut len = unsafe { GetWindowTextLengthW(edit) }.max(0) as usize;
    if THRESHOLD < len {
        send(edit, EM_SETSEL, 0, (len / 3 * 2) as isize);
        replace_sel(edit, "");
        // 途中から始まる先頭の行を消す（yamy の editDeleteLine(0)）
        let line = send(edit, EM_LINELENGTH, 0, 0) + 2;
        let index = send(edit, EM_LINEINDEX, 0, 0);
        send(edit, EM_SETSEL, index as usize, index + line);
        replace_sel(edit, "");
        // SAFETY: 同上。
        len = unsafe { GetWindowTextLengthW(edit) }.max(0) as usize;
    }
    send(edit, EM_SETSEL, len, len as isize);
    replace_sel(edit, &crlf(text));
}

fn crlf(text: &str) -> String {
    text.replace('\n', "\r\n")
}

fn replace_sel(edit: HWND, s: &str) {
    let buf: Vec<u16> = s.encode_utf16().chain([0]).collect();
    send(edit, EM_REPLACESEL, 0, buf.as_ptr() as isize);
}

/// ダイアログの状態。
struct LogDlg {
    ini: Box<Ini>,
    edit: HWND,
    font: LOGFONTW,
    hfont: Option<HFONT>,
    layout: Layout,
}

impl LogDlg {
    fn set_font(&mut self) {
        // SAFETY: font は有効な LOGFONTW。
        let new = unsafe { CreateFontIndirectW(&self.font) };
        send(self.edit, WM_SETFONT, new.0 as usize, 1);
        if let Some(old) = self.hfont.replace(new) {
            // SAFETY: 自分で作り、もう付けていないフォント。
            let _ = unsafe { DeleteObject(old.into()) };
        }
    }

    fn command(&mut self, dlg: HWND, id: i32) -> bool {
        match id {
            IDOK => {
                // SAFETY: ダイアログの窓。
                let _ = unsafe { ShowWindow(dlg, SW_HIDE) };
            }
            IDC_BUTTON_CLEAR_LOG => {
                send(self.edit, EM_SETSEL, 0, -1);
                replace_sel(self.edit, "");
                banner(true);
            }
            IDC_BUTTON_CHANGE_FONT => {
                let mut lf = self.font;
                let mut cf = CHOOSEFONTW {
                    lStructSize: std::mem::size_of::<CHOOSEFONTW>() as u32,
                    hwndOwner: dlg,
                    lpLogFont: &mut lf,
                    Flags: CF_INITTOLOGFONTSTRUCT | CF_SCREENFONTS,
                    ..Default::default()
                };
                // SAFETY: cf と lf は呼んでいる間生きている。
                if unsafe { ChooseFontW(&mut cf) }.as_bool() {
                    self.font = lf;
                    self.set_font();
                    self.ini.set_log_font(&format_font(&self.font));
                }
            }
            IDC_CHECK_DETAIL => {
                let checked = item(dlg, IDC_CHECK_DETAIL)
                    .is_some_and(|h| send(h, BM_GETCHECK, 0, 0) == BST_CHECKED);
                log::set_detail(checked);
            }
            _ => return false,
        }
        true
    }
}

unsafe extern "system" fn log_proc(dlg: HWND, msg: u32, w: WPARAM, l: LPARAM) -> isize {
    match msg {
        WM_INITDIALOG => {
            // SAFETY: create が渡した Box<Ini> の生ポインタ。ここで引き取る。
            let ini = unsafe { Box::from_raw(l.0 as *mut Ini) };
            let Some(edit) = item(dlg, IDC_EDIT_LOG) else {
                return 1;
            };
            // SAFETY: 標準アイコン（トレイと同じ）。
            if let Ok(icon) = unsafe { LoadIconW(None, IDI_APPLICATION) } {
                send(dlg, WM_SETICON, ICON_SMALL as usize, icon.0 as isize);
                send(dlg, WM_SETICON, ICON_BIG as usize, icon.0 as isize);
            }
            let font = ini
                .log_font()
                .and_then(|s| parse_font(&s))
                .or_else(|| parse_font(DEFAULT_FONT))
                .unwrap_or_default();
            let mut s = Box::new(LogDlg {
                ini,
                edit,
                font,
                hfont: None,
                layout: Layout::new(dlg),
            });
            s.set_font();
            log::set_detail(false);
            use Origin::*;
            s.layout.add(dlg, IDOK, Near, Far, Near, Far);
            s.layout.add(dlg, IDC_EDIT_LOG, Near, Near, Far, Far);
            for id in [
                IDC_BUTTON_CLEAR_LOG,
                IDC_BUTTON_CHANGE_FONT,
                IDC_CHECK_DETAIL,
            ] {
                s.layout.add(dlg, id, Near, Far, Near, Far);
            }
            set_state(dlg, s);
            // 縦を 4 倍にする（yamy と同じ）
            let mut rc = RECT::default();
            // SAFETY: ダイアログの窓。
            unsafe {
                let _ = GetWindowRect(dlg, &mut rc);
                rc.bottom += (rc.bottom - rc.top) * 3;
                let _ = MoveWindow(
                    dlg,
                    rc.left,
                    rc.top,
                    rc.right - rc.left,
                    rc.bottom - rc.top,
                    true,
                );
            }
            1
        }
        WM_LOG_APPEND => {
            if let Some(s) = state::<LogDlg>(dlg) {
                append(s.edit, &log::take_view());
            }
            1
        }
        WM_LOG_SHOW => {
            let sw = windows::Win32::UI::WindowsAndMessaging::SHOW_WINDOW_CMD(w.0 as i32);
            // SAFETY: ダイアログの窓。
            unsafe {
                let _ = ShowWindow(dlg, sw);
                if [
                    SW_SHOWNORMAL,
                    SW_SHOWMAXIMIZED,
                    SW_SHOW,
                    SW_RESTORE,
                    SW_SHOWDEFAULT,
                ]
                .contains(&sw)
                {
                    let _ = SetForegroundWindow(dlg);
                }
            }
            1
        }
        WM_COMMAND => match state::<LogDlg>(dlg) {
            Some(s) => s.command(dlg, loword(w)) as isize,
            None => 0,
        },
        WM_SIZE => {
            if let Some(s) = state::<LogDlg>(dlg) {
                s.layout
                    .resize((l.0 & 0xFFFF) as i32, ((l.0 >> 16) & 0xFFFF) as i32);
            }
            1
        }
        WM_GETMINMAXINFO => {
            if let Some(s) = state::<LogDlg>(dlg) {
                // SAFETY: WM_GETMINMAXINFO の lparam は MINMAXINFO を指す。
                s.layout.limit(unsafe { &mut *(l.0 as *mut MINMAXINFO) });
            }
            1
        }
        WM_CLOSE => {
            // SAFETY: ダイアログの窓。閉じても隠すだけ（yamy と同じ）。
            let _ = unsafe { ShowWindow(dlg, SW_HIDE) };
            1
        }
        WM_NCDESTROY => {
            if let Some(s) = state::<LogDlg>(dlg)
                && let Some(f) = s.hfont.take()
            {
                // SAFETY: 窓と一緒に消えるので、作ったフォントも消す。
                let _ = unsafe { DeleteObject(f.into()) };
            }
            drop_state::<LogDlg>(dlg);
            0
        }
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_round_trips_like_yamy() {
        let lf = parse_font(DEFAULT_FONT).unwrap();
        assert_eq!(lf.lfHeight, -13);
        assert_eq!(lf.lfWeight, 400);
        assert_eq!(lf.lfCharSet.0, 128);
        assert_eq!(format_font(&lf), DEFAULT_FONT);
        // 名前の , はそのまま名前の一部
        let lf = parse_font("1,2,3,4,5,0,0,0,1,0,0,0,0,A,B").unwrap();
        assert_eq!(format_font(&lf), "1,2,3,4,5,0,0,0,1,0,0,0,0,A,B");
        assert!(parse_font("1,2,3").is_none());
        assert!(parse_font("1,2,3,4,5,0,0,0,1,0,0,0,0,").is_none());
        assert!(parse_font("x,2,3,4,5,0,0,0,1,0,0,0,0,A").is_none());
    }

    #[test]
    fn newlines_become_crlf() {
        assert_eq!(crlf("a\nb\n"), "a\r\nb\r\n");
    }
}

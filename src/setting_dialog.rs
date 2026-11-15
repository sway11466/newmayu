//! 設定ダイアログ（yamy の `dlgsetting.cpp` / `dlgeditsetting.cpp`）。
//!
//! 一覧のダイアログで設定の追加・編集・削除・並べ替えをし、OK で `yamy.ini` に書く。
//! 1 件の編集は別のダイアログ（名前・ファイル名・シンボル）。
//! ダイアログの形は yamy の `mayu.rc` と同じ。リソースは使わず、テンプレートをメモリに作る。
//! 大きさを変えたときのコントロールの動きも yamy の `LayoutManager` と同じ。

use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::MapWindowPoints;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::Dialogs::{
    GetOpenFileNameW, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_HIDEREADONLY, OFN_PATHMUSTEXIST,
    OPENFILENAMEW,
};
use windows::Win32::UI::Controls::{
    ICC_LISTVIEW_CLASSES, INITCOMMONCONTROLSEX, InitCommonControlsEx, LVCF_FMT, LVCF_TEXT,
    LVCF_WIDTH, LVCFMT_LEFT, LVCOLUMNW, LVIF_TEXT, LVIS_SELECTED, LVITEMW, LVM_DELETEITEM,
    LVM_GETITEMCOUNT, LVM_GETITEMTEXTW, LVM_GETNEXTITEM, LVM_INSERTCOLUMNW, LVM_INSERTITEMW,
    LVM_SETCOLUMNWIDTH, LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETITEMSTATE, LVM_SETITEMTEXTW,
    LVNI_SELECTED, LVS_EX_FULLROWSELECT, LVSCW_AUTOSIZE, NM_DBLCLK, NMHDR,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DLGTEMPLATE, DialogBoxIndirectParamW, EndDialog, GWLP_USERDATA, GetClientRect, GetDlgItem,
    GetWindowLongPtrW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW, MINMAXINFO,
    SWP_NOACTIVATE, SWP_NOZORDER, SendMessageW, SetWindowLongPtrW, SetWindowPos, SetWindowTextW,
    WM_CLOSE, WM_COMMAND, WM_GETMINMAXINFO, WM_INITDIALOG, WM_NCDESTROY, WM_NOTIFY, WM_SIZE,
};
use windows::core::{HSTRING, PCWSTR, PWSTR};

use crate::setting::{Entry, Ini};

// ---- ダイアログのテンプレート ----

// 窓とコントロールの形（Win32 の値。型の違う定数を混ぜないよう数で持つ）
const WS_CHILD: u32 = 0x4000_0000;
const WS_VISIBLE: u32 = 0x1000_0000;
const WS_POPUP: u32 = 0x8000_0000;
const WS_CAPTION: u32 = 0x00C0_0000;
const WS_SYSMENU: u32 = 0x0008_0000;
const WS_THICKFRAME: u32 = 0x0004_0000;
const WS_BORDER: u32 = 0x0080_0000;
const WS_TABSTOP: u32 = 0x0001_0000;
const WS_GROUP: u32 = 0x0002_0000;
const DS_SETFONT: u32 = 0x40;
const DS_CENTERMOUSE: u32 = 0x1000;
const BS_PUSHBUTTON: u32 = 0x0;
const BS_DEFPUSHBUTTON: u32 = 0x1;
const BS_GROUPBOX: u32 = 0x7;
const ES_AUTOHSCROLL: u32 = 0x80;
const SS_LEFT: u32 = 0x0;
const LVS_REPORT: u32 = 0x1;
const LVS_SINGLESEL: u32 = 0x4;
const LVS_SHOWSELALWAYS: u32 = 0x8;
const LVS_NOLABELWRAP: u32 = 0x80;

// 定義済みのクラス（テンプレートでは番号で書ける）
const BUTTON: u16 = 0x80;
const EDIT: u16 = 0x81;
const STATIC: u16 = 0x82;

const IDOK: i32 = 1;
const IDCANCEL: i32 = 2;

/// コントロールのクラス。
enum Class {
    Atom(u16),
    Name(&'static str),
}

/// メモリ上の `DLGTEMPLATE` を組み立てる。
struct Template {
    words: Vec<u16>,
}

impl Template {
    /// `FONT 9, "MS UI Gothic"` 付きのダイアログ。
    fn new(style: u32, (x, y, cx, cy): (i16, i16, i16, i16), caption: &str) -> Template {
        let mut t = Template { words: Vec::new() };
        t.u32(style | DS_SETFONT);
        t.u32(0); // 拡張スタイル
        t.words.push(0); // コントロールの数。item で数える
        for v in [x, y, cx, cy] {
            t.words.push(v as u16);
        }
        t.words.push(0); // メニューなし
        t.words.push(0); // 既定のクラス
        t.str(caption);
        t.words.push(9);
        t.str("MS UI Gothic");
        t
    }

    fn item(
        &mut self,
        class: Class,
        text: &str,
        id: i32,
        (x, y, cx, cy): (i16, i16, i16, i16),
        style: u32,
    ) -> &mut Self {
        if self.words.len() % 2 == 1 {
            self.words.push(0); // DWORD 境界にそろえる
        }
        self.u32(style | WS_CHILD | WS_VISIBLE);
        self.u32(0);
        for v in [x, y, cx, cy] {
            self.words.push(v as u16);
        }
        self.words.push(id as u16);
        match class {
            Class::Atom(a) => {
                self.words.push(0xFFFF);
                self.words.push(a);
            }
            Class::Name(n) => self.str(n),
        }
        self.str(text);
        self.words.push(0); // 作成データなし
        self.words[4] += 1;
        self
    }

    fn u32(&mut self, v: u32) {
        self.words.push(v as u16);
        self.words.push((v >> 16) as u16);
    }

    fn str(&mut self, s: &str) {
        self.words.extend(s.encode_utf16());
        self.words.push(0);
    }

    /// モーダルで出し、`EndDialog` の値を返す。
    fn run(&self, owner: Option<HWND>, proc: DlgProc, param: LPARAM) -> isize {
        let instance = unsafe { GetModuleHandleW(None) }.ok().map(Into::into);
        // SAFETY: words は DLGTEMPLATE の並び（u16 の配列は 2 バイト境界。DLGTEMPLATE は
        // packed(2)）。ダイアログが閉じるまで self は生きている。
        unsafe {
            DialogBoxIndirectParamW(
                instance,
                self.words.as_ptr() as *const DLGTEMPLATE,
                owner,
                Some(proc),
                param,
            )
        }
    }
}

type DlgProc = unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> isize;

// ---- 大きさを変えたときの並べ直し（yamy の LayoutManager） ----

/// 辺の基準。左（上）の辺・中央・右（下）の辺。
#[derive(Clone, Copy)]
enum Origin {
    Near,
    Center,
    Far,
}

impl Origin {
    /// 窓の大きさが `delta` 変わったときに動く量。
    fn shift(self, delta: i32) -> i32 {
        match self {
            Origin::Near => 0,
            Origin::Center => delta / 2,
            Origin::Far => delta,
        }
    }
}

/// 1 つのコントロールの、最初の位置と各辺の基準。
struct Placed {
    hwnd: HWND,
    rect: RECT,
    left: Origin,
    top: Origin,
    right: Origin,
    bottom: Origin,
}

/// 並べ直しの状態。
#[derive(Default)]
struct Layout {
    /// 最初のクライアント領域の大きさ。
    base: (i32, i32),
    /// 最初の窓の大きさ（これより小さくしない）。
    smallest: (i32, i32),
    /// 縦はこれより大きくしない（設定の編集）。
    largest_height: Option<i32>,
    items: Vec<Placed>,
}

impl Layout {
    fn new(dlg: HWND) -> Layout {
        let mut client = RECT::default();
        let mut window = RECT::default();
        // SAFETY: dlg は初期化中のダイアログ。
        unsafe {
            let _ = GetClientRect(dlg, &mut client);
            let _ = GetWindowRect(dlg, &mut window);
        }
        Layout {
            base: (client.right, client.bottom),
            smallest: (window.right - window.left, window.bottom - window.top),
            ..Layout::default()
        }
    }

    fn add(
        &mut self,
        dlg: HWND,
        id: i32,
        left: Origin,
        top: Origin,
        right: Origin,
        bottom: Origin,
    ) {
        let Some(hwnd) = item(dlg, id) else { return };
        let mut rect = RECT::default();
        // SAFETY: hwnd は dlg の子。rect を dlg のクライアント座標に直す。
        unsafe {
            let _ = GetWindowRect(hwnd, &mut rect);
            let mut pts = [
                POINT {
                    x: rect.left,
                    y: rect.top,
                },
                POINT {
                    x: rect.right,
                    y: rect.bottom,
                },
            ];
            MapWindowPoints(None, Some(dlg), &mut pts);
            rect = RECT {
                left: pts[0].x,
                top: pts[0].y,
                right: pts[1].x,
                bottom: pts[1].y,
            };
        }
        self.items.push(Placed {
            hwnd,
            rect,
            left,
            top,
            right,
            bottom,
        });
    }

    /// 左上に付いたまま動かない（yamy の `addItem(hwnd)` の既定）。
    fn add_fixed(&mut self, dlg: HWND, id: i32) {
        use Origin::Near;
        self.add(dlg, id, Near, Near, Near, Near);
    }

    fn resize(&self, width: i32, height: i32) {
        let dx = width - self.base.0;
        let dy = height - self.base.1;
        for p in &self.items {
            let l = p.rect.left + p.left.shift(dx);
            let t = p.rect.top + p.top.shift(dy);
            let r = p.rect.right + p.right.shift(dx);
            let b = p.rect.bottom + p.bottom.shift(dy);
            // SAFETY: p.hwnd はダイアログの子。
            unsafe {
                let _ = SetWindowPos(
                    p.hwnd,
                    None,
                    l,
                    t,
                    r - l,
                    b - t,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
        }
    }

    fn limit(&self, info: &mut MINMAXINFO) {
        info.ptMinTrackSize.x = self.smallest.0;
        info.ptMinTrackSize.y = self.smallest.1;
        if let Some(h) = self.largest_height {
            info.ptMaxTrackSize.y = h;
        }
    }
}

// ---- 共通 ----

fn item(dlg: HWND, id: i32) -> Option<HWND> {
    // SAFETY: dlg はダイアログ。
    unsafe { GetDlgItem(Some(dlg), id) }.ok()
}

fn text(hwnd: HWND) -> String {
    // SAFETY: hwnd はコントロール。buf は書き込める。
    unsafe {
        let len = GetWindowTextLengthW(hwnd).max(0) as usize;
        let mut buf = vec![0u16; len + 1];
        let n = GetWindowTextW(hwnd, &mut buf).max(0) as usize;
        String::from_utf16_lossy(&buf[..n])
    }
}

fn set_text(hwnd: HWND, s: &str) {
    // SAFETY: hwnd はコントロール。
    let _ = unsafe { SetWindowTextW(hwnd, &HSTRING::from(s)) };
}

/// ダイアログの `GWLP_USERDATA` に置いた状態。
fn state<'a, T>(dlg: HWND) -> Option<&'a mut T> {
    // SAFETY: USERDATA には WM_INITDIALOG で Box<T> の生ポインタを置き、
    // WM_NCDESTROY で外して落とす。0 なら None。
    unsafe { (GetWindowLongPtrW(dlg, GWLP_USERDATA) as *mut T).as_mut() }
}

fn set_state<T>(dlg: HWND, s: Box<T>) {
    // SAFETY: dlg は初期化中のダイアログ。
    unsafe { SetWindowLongPtrW(dlg, GWLP_USERDATA, Box::into_raw(s) as isize) };
}

fn drop_state<T>(dlg: HWND) {
    // SAFETY: set_state が置いたポインタか 0。外してから落とす。
    unsafe {
        let p = SetWindowLongPtrW(dlg, GWLP_USERDATA, 0) as *mut T;
        if !p.is_null() {
            drop(Box::from_raw(p));
        }
    }
}

fn end(dlg: HWND, result: isize) {
    // SAFETY: dlg はモーダルのダイアログ。
    let _ = unsafe { EndDialog(dlg, result) };
}

fn loword(w: WPARAM) -> i32 {
    (w.0 & 0xFFFF) as i32
}

// ---- 設定の一覧（IDD_DIALOG_setting） ----

const IDC_STATIC_MAYU_PATHS: i32 = 1001;
const IDC_LIST_MAYU_PATHS: i32 = 1002;
const IDC_BUTTON_UP: i32 = 1003;
const IDC_BUTTON_DOWN: i32 = 1004;
const IDC_BUTTON_ADD: i32 = 1005;
const IDC_BUTTON_EDIT: i32 = 1006;
const IDC_BUTTON_DELETE: i32 = 1007;

/// 開いている間は二重に開かない（yamy の `m_isSettingDialogOpened`）。
static OPENED: AtomicBool = AtomicBool::new(false);

/// 設定ダイアログを出す。OK なら `yamy.ini` を書いて `true`。すでに開いていれば何もせず `false`。
pub fn run(ini: &Ini) -> bool {
    if OPENED.swap(true, Ordering::SeqCst) {
        return false;
    }
    let icc = INITCOMMONCONTROLSEX {
        dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
        dwICC: ICC_LISTVIEW_CLASSES,
    };
    // SAFETY: icc は有効。
    let _ = unsafe { InitCommonControlsEx(&icc) };
    let mut t = Template::new(
        DS_CENTERMOUSE | WS_POPUP | WS_CAPTION | WS_SYSMENU | WS_THICKFRAME,
        (0, 0, 302, 135),
        "設定 - newmayu",
    );
    t.item(
        Class::Atom(BUTTON),
        "設定ファイル名リスト(&L)",
        IDC_STATIC_MAYU_PATHS,
        (4, 4, 296, 108),
        BS_GROUPBOX,
    )
    .item(
        Class::Name("SysListView32"),
        "List1",
        IDC_LIST_MAYU_PATHS,
        (8, 16, 268, 72),
        LVS_REPORT | LVS_SINGLESEL | LVS_SHOWSELALWAYS | LVS_NOLABELWRAP | WS_BORDER | WS_TABSTOP,
    )
    .item(
        Class::Atom(BUTTON),
        "↑",
        IDC_BUTTON_UP,
        (280, 36, 14, 14),
        BS_PUSHBUTTON | WS_TABSTOP,
    )
    .item(
        Class::Atom(BUTTON),
        "↓",
        IDC_BUTTON_DOWN,
        (280, 52, 14, 14),
        BS_PUSHBUTTON | WS_TABSTOP,
    )
    .item(
        Class::Atom(BUTTON),
        "追加(&A)",
        IDC_BUTTON_ADD,
        (66, 92, 50, 14),
        BS_PUSHBUTTON | WS_TABSTOP,
    )
    .item(
        Class::Atom(BUTTON),
        "編集(&E)",
        IDC_BUTTON_EDIT,
        (126, 92, 50, 14),
        BS_PUSHBUTTON | WS_TABSTOP,
    )
    .item(
        Class::Atom(BUTTON),
        "削除(&D)",
        IDC_BUTTON_DELETE,
        (186, 92, 50, 14),
        BS_PUSHBUTTON | WS_TABSTOP,
    )
    .item(
        Class::Atom(BUTTON),
        "キャンセル",
        IDCANCEL,
        (156, 116, 50, 14),
        BS_PUSHBUTTON | WS_TABSTOP,
    )
    .item(
        Class::Atom(BUTTON),
        "OK",
        IDOK,
        (96, 116, 50, 14),
        BS_DEFPUSHBUTTON | WS_TABSTOP,
    );
    let r = t.run(None, setting_proc, LPARAM(ini as *const Ini as isize));
    OPENED.store(false, Ordering::SeqCst);
    r == 1
}

struct Setting<'a> {
    ini: &'a Ini,
    list: HWND,
    layout: Layout,
}

impl Setting<'_> {
    fn count(&self) -> i32 {
        self.send(LVM_GETITEMCOUNT, 0, 0) as i32
    }

    fn send(&self, msg: u32, w: usize, l: isize) -> isize {
        // SAFETY: list はダイアログの ListView。
        unsafe { SendMessageW(self.list, msg, Some(WPARAM(w)), Some(LPARAM(l))) }.0
    }

    fn insert(&self, index: i32, e: &Entry) {
        let mut name: Vec<u16> = e.name.encode_utf16().chain([0]).collect();
        let lv = LVITEMW {
            mask: LVIF_TEXT,
            iItem: index,
            pszText: PWSTR(name.as_mut_ptr()),
            ..Default::default()
        };
        self.send(LVM_INSERTITEMW, 0, &lv as *const _ as isize);
        self.set_sub(index, 1, &e.path);
        self.set_sub(index, 2, &e.symbols);
    }

    fn set(&self, index: i32, e: &Entry) {
        self.set_sub(index, 0, &e.name);
        self.set_sub(index, 1, &e.path);
        self.set_sub(index, 2, &e.symbols);
    }

    fn set_sub(&self, index: i32, sub: i32, s: &str) {
        let mut buf: Vec<u16> = s.encode_utf16().chain([0]).collect();
        let lv = LVITEMW {
            iSubItem: sub,
            pszText: PWSTR(buf.as_mut_ptr()),
            ..Default::default()
        };
        self.send(LVM_SETITEMTEXTW, index as usize, &lv as *const _ as isize);
    }

    fn get(&self, index: i32) -> Entry {
        let sub = |s: i32| {
            let mut buf = vec![0u16; 260 * 4];
            let lv = LVITEMW {
                iSubItem: s,
                pszText: PWSTR(buf.as_mut_ptr()),
                cchTextMax: buf.len() as i32,
                ..Default::default()
            };
            let n = self.send(LVM_GETITEMTEXTW, index as usize, &lv as *const _ as isize);
            String::from_utf16_lossy(&buf[..n.max(0) as usize])
        };
        Entry {
            name: sub(0),
            path: sub(1),
            symbols: sub(2),
        }
    }

    fn select(&self, index: i32) {
        let lv = LVITEMW {
            state: LVIS_SELECTED,
            stateMask: LVIS_SELECTED,
            ..Default::default()
        };
        self.send(LVM_SETITEMSTATE, index as usize, &lv as *const _ as isize);
    }

    /// 選ばれている行。無ければ `None`。
    fn selected(&self) -> Option<i32> {
        let i = self.send(LVM_GETNEXTITEM, usize::MAX, LVNI_SELECTED as isize) as i32;
        (i >= 0).then_some(i)
    }

    fn init(&mut self, dlg: HWND) {
        let mut rc = RECT::default();
        // SAFETY: list はダイアログの ListView。
        let _ = unsafe { GetClientRect(self.list, &mut rc) };
        for (i, name) in ["名前", "設定ファイル名", "シンボル"]
            .into_iter()
            .enumerate()
        {
            let mut buf: Vec<u16> = name.encode_utf16().chain([0]).collect();
            let col = LVCOLUMNW {
                mask: LVCF_FMT | LVCF_WIDTH | LVCF_TEXT,
                fmt: LVCFMT_LEFT,
                cx: (rc.right - rc.left) / 3,
                pszText: PWSTR(buf.as_mut_ptr()),
                ..Default::default()
            };
            self.send(LVM_INSERTCOLUMNW, i, &col as *const _ as isize);
        }
        for (i, e) in self
            .ini
            .entries()
            .iter()
            .filter_map(|raw| Entry::parse(raw))
            .enumerate()
        {
            self.insert(i as i32, &e);
        }
        for i in 0..3 {
            self.send(LVM_SETCOLUMNWIDTH, i, LVSCW_AUTOSIZE as isize);
        }
        self.send(
            LVM_SETEXTENDEDLISTVIEWSTYLE,
            LVS_EX_FULLROWSELECT as usize,
            LVS_EX_FULLROWSELECT as isize,
        );
        self.select(self.ini.index() as i32);

        use Origin::*;
        let l = &mut self.layout;
        l.add(dlg, IDC_STATIC_MAYU_PATHS, Near, Near, Far, Far);
        l.add(dlg, IDC_LIST_MAYU_PATHS, Near, Near, Far, Far);
        l.add(dlg, IDC_BUTTON_UP, Far, Center, Far, Center);
        l.add(dlg, IDC_BUTTON_DOWN, Far, Center, Far, Center);
        for id in [
            IDC_BUTTON_ADD,
            IDC_BUTTON_EDIT,
            IDC_BUTTON_DELETE,
            IDCANCEL,
            IDOK,
        ] {
            l.add(dlg, id, Center, Far, Center, Far);
        }
    }

    fn command(&self, dlg: HWND, id: i32) -> bool {
        match id {
            IDC_BUTTON_UP | IDC_BUTTON_DOWN => {
                let count = self.count();
                if count < 2 {
                    return true;
                }
                let Some(index) = self.selected() else {
                    return true;
                };
                if (id == IDC_BUTTON_UP && index == 0)
                    || (id == IDC_BUTTON_DOWN && index == count - 1)
                {
                    return true;
                }
                let target = if id == IDC_BUTTON_UP {
                    index - 1
                } else {
                    index + 1
                };
                let a = self.get(index);
                let b = self.get(target);
                self.set(index, &b);
                self.set(target, &a);
                self.select(target);
            }
            IDC_BUTTON_ADD => {
                let mut e = self.selected().map(|i| self.get(i)).unwrap_or_default();
                if edit(dlg, &mut e) && !e.name.is_empty() {
                    self.insert(0, &e);
                    self.select(0);
                }
            }
            IDC_BUTTON_DELETE => {
                if let Some(index) = self.selected() {
                    self.send(LVM_DELETEITEM, index as usize, 0);
                    let count = self.count();
                    if count == 0 {
                    } else if count == index {
                        self.select(index - 1);
                    } else {
                        self.select(index);
                    }
                }
            }
            IDC_BUTTON_EDIT => {
                let Some(index) = self.selected() else {
                    return true;
                };
                let mut e = self.get(index);
                if edit(dlg, &mut e) {
                    self.set(index, &e);
                    self.select(index);
                }
            }
            IDOK => {
                let entries: Vec<Entry> = (0..self.count()).map(|i| self.get(i)).collect();
                self.ini.write_entries(&entries);
                self.ini.set_index(self.selected().unwrap_or(0) as usize);
                end(dlg, 1);
            }
            IDCANCEL => end(dlg, 0),
            _ => return false,
        }
        true
    }
}

unsafe extern "system" fn setting_proc(dlg: HWND, msg: u32, w: WPARAM, l: LPARAM) -> isize {
    match msg {
        WM_INITDIALOG => {
            // SAFETY: run が渡した &Ini。ダイアログが閉じるまで生きている。
            let ini = unsafe { &*(l.0 as *const Ini) };
            let Some(list) = item(dlg, IDC_LIST_MAYU_PATHS) else {
                end(dlg, 0);
                return 1;
            };
            let mut s = Box::new(Setting {
                ini,
                list,
                layout: Layout::new(dlg),
            });
            s.init(dlg);
            set_state(dlg, s);
            1
        }
        WM_NOTIFY => {
            // SAFETY: WM_NOTIFY の lparam は NMHDR を指す。
            let nm = unsafe { &*(l.0 as *const NMHDR) };
            if nm.idFrom == IDC_LIST_MAYU_PATHS as usize
                && nm.code == NM_DBLCLK
                && let Some(s) = state::<Setting>(dlg)
            {
                s.command(dlg, IDC_BUTTON_EDIT);
            }
            1
        }
        WM_COMMAND => match state::<Setting>(dlg) {
            Some(s) => s.command(dlg, loword(w)) as isize,
            None => 0,
        },
        WM_SIZE => {
            if let Some(s) = state::<Setting>(dlg) {
                s.layout
                    .resize((l.0 & 0xFFFF) as i32, ((l.0 >> 16) & 0xFFFF) as i32);
            }
            1
        }
        WM_GETMINMAXINFO => {
            if let Some(s) = state::<Setting>(dlg) {
                // SAFETY: WM_GETMINMAXINFO の lparam は MINMAXINFO を指す。
                s.layout.limit(unsafe { &mut *(l.0 as *mut MINMAXINFO) });
            }
            1
        }
        WM_CLOSE => {
            end(dlg, 0);
            1
        }
        WM_NCDESTROY => {
            drop_state::<Setting>(dlg);
            0
        }
        _ => 0,
    }
}

// ---- 設定の編集（IDD_DIALOG_editSetting） ----

const IDC_STATIC_MAYU_PATH_NAME: i32 = 1101;
const IDC_EDIT_MAYU_PATH_NAME: i32 = 1102;
const IDC_STATIC_MAYU_PATH_NAME_COMMENT: i32 = 1103;
const IDC_STATIC_MAYU_PATH: i32 = 1104;
const IDC_EDIT_MAYU_PATH: i32 = 1105;
const IDC_BUTTON_BROWSE: i32 = 1106;
const IDC_STATIC_SYMBOLS: i32 = 1107;
const IDC_EDIT_SYMBOLS: i32 = 1108;
const IDC_STATIC_SYMBOLS_COMMENT: i32 = 1109;

/// 1 件を編集する。OK なら `e` を書き換えて `true`。
fn edit(owner: HWND, e: &mut Entry) -> bool {
    let mut t = Template::new(
        DS_CENTERMOUSE | WS_POPUP | WS_CAPTION | WS_SYSMENU | WS_THICKFRAME,
        (0, 0, 254, 75),
        "設定の編集 - newmayu",
    );
    let label = SS_LEFT | WS_GROUP;
    let edit = ES_AUTOHSCROLL | WS_BORDER | WS_TABSTOP;
    t.item(
        Class::Atom(STATIC),
        "名前(&N)",
        IDC_STATIC_MAYU_PATH_NAME,
        (8, 7, 36, 8),
        label,
    )
    .item(
        Class::Atom(EDIT),
        "",
        IDC_EDIT_MAYU_PATH_NAME,
        (44, 4, 152, 14),
        edit,
    )
    .item(
        Class::Atom(STATIC),
        "(セミコロン不可)",
        IDC_STATIC_MAYU_PATH_NAME_COMMENT,
        (200, 6, 44, 8),
        label,
    )
    .item(
        Class::Atom(STATIC),
        "ファイル名(&F)",
        IDC_STATIC_MAYU_PATH,
        (8, 22, 36, 8),
        label,
    )
    .item(
        Class::Atom(EDIT),
        "",
        IDC_EDIT_MAYU_PATH,
        (44, 20, 152, 14),
        edit,
    )
    .item(
        Class::Atom(BUTTON),
        "参照(&B)",
        IDC_BUTTON_BROWSE,
        (200, 20, 50, 14),
        BS_PUSHBUTTON | WS_TABSTOP,
    )
    .item(
        Class::Atom(STATIC),
        "シンボル(&S)",
        IDC_STATIC_SYMBOLS,
        (8, 38, 36, 8),
        label,
    )
    .item(
        Class::Atom(EDIT),
        "",
        IDC_EDIT_SYMBOLS,
        (44, 36, 152, 14),
        edit,
    )
    .item(
        Class::Atom(STATIC),
        "(-D を付け、セミコロンで区切る)",
        IDC_STATIC_SYMBOLS_COMMENT,
        (200, 38, 48, 22),
        label,
    )
    .item(
        Class::Atom(BUTTON),
        "OK",
        IDOK,
        (76, 56, 50, 14),
        BS_DEFPUSHBUTTON | WS_TABSTOP,
    )
    .item(
        Class::Atom(BUTTON),
        "キャンセル",
        IDCANCEL,
        (132, 56, 50, 14),
        BS_PUSHBUTTON | WS_TABSTOP,
    );
    t.run(Some(owner), edit_proc, LPARAM(e as *mut Entry as isize)) == 1
}

struct EditSetting<'a> {
    entry: &'a mut Entry,
    layout: Layout,
}

unsafe extern "system" fn edit_proc(dlg: HWND, msg: u32, w: WPARAM, l: LPARAM) -> isize {
    match msg {
        WM_INITDIALOG => {
            // SAFETY: edit が渡した &mut Entry。ダイアログが閉じるまで生きている。
            let entry = unsafe { &mut *(l.0 as *mut Entry) };
            for (id, s) in [
                (IDC_EDIT_MAYU_PATH_NAME, &entry.name),
                (IDC_EDIT_MAYU_PATH, &entry.path),
                (IDC_EDIT_SYMBOLS, &entry.symbols),
            ] {
                if let Some(h) = item(dlg, id) {
                    set_text(h, s);
                }
            }
            let mut layout = Layout::new(dlg);
            use Origin::*;
            for id in [
                IDC_STATIC_MAYU_PATH_NAME,
                IDC_STATIC_MAYU_PATH,
                IDC_STATIC_SYMBOLS,
            ] {
                layout.add_fixed(dlg, id);
            }
            for id in [
                IDC_EDIT_MAYU_PATH_NAME,
                IDC_EDIT_MAYU_PATH,
                IDC_EDIT_SYMBOLS,
            ] {
                layout.add(dlg, id, Near, Near, Far, Near);
            }
            for id in [
                IDC_STATIC_MAYU_PATH_NAME_COMMENT,
                IDC_BUTTON_BROWSE,
                IDC_STATIC_SYMBOLS_COMMENT,
            ] {
                layout.add(dlg, id, Far, Near, Far, Near);
            }
            for id in [IDOK, IDCANCEL] {
                layout.add(dlg, id, Center, Near, Center, Near);
            }
            layout.largest_height = Some(layout.smallest.1);
            set_state(dlg, Box::new(EditSetting { entry, layout }));
            1
        }
        WM_COMMAND => {
            let Some(s) = state::<EditSetting>(dlg) else {
                return 0;
            };
            match loword(w) {
                IDC_BUTTON_BROWSE => {
                    if let Some(path) = browse(dlg)
                        && let Some(h) = item(dlg, IDC_EDIT_MAYU_PATH)
                    {
                        set_text(h, &path);
                    }
                }
                IDOK => {
                    let get = |id| item(dlg, id).map(text).unwrap_or_default();
                    s.entry.name = get(IDC_EDIT_MAYU_PATH_NAME);
                    s.entry.path = get(IDC_EDIT_MAYU_PATH);
                    s.entry.symbols = get(IDC_EDIT_SYMBOLS);
                    end(dlg, 1);
                }
                IDCANCEL => end(dlg, 0),
                _ => return 0,
            }
            1
        }
        WM_SIZE => {
            if let Some(s) = state::<EditSetting>(dlg) {
                s.layout
                    .resize((l.0 & 0xFFFF) as i32, ((l.0 >> 16) & 0xFFFF) as i32);
            }
            1
        }
        WM_GETMINMAXINFO => {
            if let Some(s) = state::<EditSetting>(dlg) {
                // SAFETY: WM_GETMINMAXINFO の lparam は MINMAXINFO を指す。
                s.layout.limit(unsafe { &mut *(l.0 as *mut MINMAXINFO) });
            }
            1
        }
        WM_CLOSE => {
            end(dlg, 0);
            1
        }
        WM_NCDESTROY => {
            drop_state::<EditSetting>(dlg);
            0
        }
        _ => 0,
    }
}

/// 「参照」: `.mayu` を選ぶ（yamy の `GetOpenFileName`）。
fn browse(owner: HWND) -> Option<String> {
    let title: Vec<u16> = "設定ファイルを探す - newmayu"
        .encode_utf16()
        .chain([0])
        .collect();
    // 区切りは NUL、最後は NUL 2 つ
    let filter: Vec<u16> = "newmayuファイル (*.mayu) \0*.mayu\0全てのファイル\0*\0\0"
        .encode_utf16()
        .collect();
    let mut file = vec![0u16; 260 * 4];
    for (d, s) in file.iter_mut().zip(".mayu".encode_utf16()) {
        *d = s;
    }
    let mut of = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: owner,
        lpstrFilter: PCWSTR(filter.as_ptr()),
        nFilterIndex: 1,
        lpstrFile: PWSTR(file.as_mut_ptr()),
        nMaxFile: file.len() as u32,
        lpstrTitle: PCWSTR(title.as_ptr()),
        Flags: OFN_EXPLORER | OFN_FILEMUSTEXIST | OFN_HIDEREADONLY | OFN_PATHMUSTEXIST,
        ..Default::default()
    };
    // SAFETY: of の文字列は関数を抜けるまで生きている。
    if unsafe { GetOpenFileNameW(&mut of) }.as_bool() {
        let n = file.iter().position(|&c| c == 0).unwrap_or(file.len());
        Some(String::from_utf16_lossy(&file[..n]))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_counts_items_and_aligns_them() {
        let mut t = Template::new(WS_POPUP, (0, 0, 10, 10), "a");
        t.item(Class::Atom(BUTTON), "OK", IDOK, (1, 2, 3, 4), BS_PUSHBUTTON)
            .item(Class::Name("SysListView32"), "", 5, (0, 0, 1, 1), 0);
        assert_eq!(t.words[4], 2);
        // ヘッダは 13 語（style 2・exstyle 2・数 1・座標 4・メニュー 1・クラス 1・"a" 2）
        // と 9・"MS UI Gothic"（13 語）。最初の項目は DWORD 境界から
        let header = 13 + 1 + 13;
        assert_eq!(header % 2, 1);
        assert_eq!(t.words[header], 0);
        assert_eq!(
            t.words[header + 1] as u32 | (t.words[header + 2] as u32) << 16,
            BS_PUSHBUTTON | WS_CHILD | WS_VISIBLE
        );
    }

    #[test]
    fn origin_shift_follows_layout_manager() {
        assert_eq!(Origin::Near.shift(10), 0);
        assert_eq!(Origin::Center.shift(10), 5);
        assert_eq!(Origin::Far.shift(10), 10);
    }
}

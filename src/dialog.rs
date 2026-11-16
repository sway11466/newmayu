//! ダイアログの共通部品。リソースを使わず、テンプレートをメモリに作る（yamy の `mayu.rc` と同じ形）。
//! 大きさを変えたときのコントロールの動きは yamy の `LayoutManager` と同じ。

use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::MapWindowPoints;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateDialogIndirectParamW, DLGTEMPLATE, DialogBoxIndirectParamW, EndDialog, GWLP_USERDATA,
    GetClientRect, GetDlgItem, GetWindowLongPtrW, GetWindowRect, GetWindowTextLengthW,
    GetWindowTextW, MINMAXINFO, SWP_NOACTIVATE, SWP_NOZORDER, SetWindowLongPtrW, SetWindowPos,
    SetWindowTextW,
};
use windows::core::HSTRING;

// ---- ダイアログのテンプレート ----

// 窓とコントロールの形（Win32 の値。型の違う定数を混ぜないよう数で持つ）
pub(crate) const WS_CHILD: u32 = 0x4000_0000;
pub(crate) const WS_VISIBLE: u32 = 0x1000_0000;
pub(crate) const WS_POPUP: u32 = 0x8000_0000;
pub(crate) const WS_CAPTION: u32 = 0x00C0_0000;
pub(crate) const WS_SYSMENU: u32 = 0x0008_0000;
pub(crate) const WS_THICKFRAME: u32 = 0x0004_0000;
pub(crate) const WS_BORDER: u32 = 0x0080_0000;
pub(crate) const WS_TABSTOP: u32 = 0x0001_0000;
pub(crate) const WS_GROUP: u32 = 0x0002_0000;
pub(crate) const DS_SETFONT: u32 = 0x40;
pub(crate) const DS_CENTERMOUSE: u32 = 0x1000;
pub(crate) const BS_PUSHBUTTON: u32 = 0x0;
pub(crate) const BS_DEFPUSHBUTTON: u32 = 0x1;
pub(crate) const BS_GROUPBOX: u32 = 0x7;
pub(crate) const ES_AUTOHSCROLL: u32 = 0x80;
pub(crate) const SS_LEFT: u32 = 0x0;
pub(crate) const LVS_REPORT: u32 = 0x1;
pub(crate) const LVS_SINGLESEL: u32 = 0x4;
pub(crate) const LVS_SHOWSELALWAYS: u32 = 0x8;
pub(crate) const LVS_NOLABELWRAP: u32 = 0x80;
pub(crate) const WS_MINIMIZEBOX: u32 = 0x0002_0000;
pub(crate) const WS_VSCROLL: u32 = 0x0020_0000;
pub(crate) const WS_HSCROLL: u32 = 0x0010_0000;
pub(crate) const WS_EX_CLIENTEDGE: u32 = 0x200;
pub(crate) const BS_AUTOCHECKBOX: u32 = 0x3;
pub(crate) const ES_MULTILINE: u32 = 0x4;
pub(crate) const ES_AUTOVSCROLL: u32 = 0x40;
pub(crate) const ES_NOHIDESEL: u32 = 0x100;
pub(crate) const ES_READONLY: u32 = 0x800;
pub(crate) const ES_WANTRETURN: u32 = 0x1000;

// 定義済みのクラス（テンプレートでは番号で書ける）
pub(crate) const BUTTON: u16 = 0x80;
pub(crate) const EDIT: u16 = 0x81;
pub(crate) const STATIC: u16 = 0x82;

pub(crate) const IDOK: i32 = 1;
pub(crate) const IDCANCEL: i32 = 2;

/// コントロールのクラス。
pub(crate) enum Class {
    Atom(u16),
    Name(&'static str),
}

/// メモリ上の `DLGTEMPLATE` を組み立てる。
pub(crate) struct Template {
    pub(crate) words: Vec<u16>,
}

impl Template {
    /// `FONT 9, "MS UI Gothic"` 付きのダイアログ。
    pub(crate) fn new(style: u32, (x, y, cx, cy): (i16, i16, i16, i16), caption: &str) -> Template {
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

    pub(crate) fn item(
        &mut self,
        class: Class,
        text: &str,
        id: i32,
        (x, y, cx, cy): (i16, i16, i16, i16),
        style: u32,
    ) -> &mut Self {
        self.item_ex(class, text, id, (x, y, cx, cy), style, 0)
    }

    /// 拡張スタイル付きのコントロール。
    pub(crate) fn item_ex(
        &mut self,
        class: Class,
        text: &str,
        id: i32,
        (x, y, cx, cy): (i16, i16, i16, i16),
        style: u32,
        ex_style: u32,
    ) -> &mut Self {
        if self.words.len() % 2 == 1 {
            self.words.push(0); // DWORD 境界にそろえる
        }
        self.u32(style | WS_CHILD | WS_VISIBLE);
        self.u32(ex_style);
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
    pub(crate) fn run(&self, owner: Option<HWND>, proc: DlgProc, param: LPARAM) -> isize {
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

    /// モードレスで作る（見せるかは `WS_VISIBLE` 次第）。テンプレートは作った後は要らない。
    pub(crate) fn create(&self, proc: DlgProc, param: LPARAM) -> Option<HWND> {
        let instance = unsafe { GetModuleHandleW(None) }.ok().map(Into::into);
        // SAFETY: run と同じ。CreateDialogIndirectParamW はテンプレートを読み終えてから返る。
        unsafe {
            CreateDialogIndirectParamW(
                instance,
                self.words.as_ptr() as *const DLGTEMPLATE,
                None,
                Some(proc),
                param,
            )
        }
        .ok()
    }
}

pub(crate) type DlgProc = unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> isize;

// ---- 大きさを変えたときの並べ直し（yamy の LayoutManager） ----

/// 辺の基準。左（上）の辺・中央・右（下）の辺。
#[derive(Clone, Copy)]
pub(crate) enum Origin {
    Near,
    Center,
    Far,
}

impl Origin {
    /// 窓の大きさが `delta` 変わったときに動く量。
    pub(crate) fn shift(self, delta: i32) -> i32 {
        match self {
            Origin::Near => 0,
            Origin::Center => delta / 2,
            Origin::Far => delta,
        }
    }
}

/// 1 つのコントロールの、最初の位置と各辺の基準。
pub(crate) struct Placed {
    hwnd: HWND,
    rect: RECT,
    left: Origin,
    top: Origin,
    right: Origin,
    bottom: Origin,
}

/// 並べ直しの状態。
#[derive(Default)]
pub(crate) struct Layout {
    /// 最初のクライアント領域の大きさ。
    pub(crate) base: (i32, i32),
    /// 最初の窓の大きさ（これより小さくしない）。
    pub(crate) smallest: (i32, i32),
    /// 縦はこれより大きくしない（設定の編集）。
    pub(crate) largest_height: Option<i32>,
    items: Vec<Placed>,
}

impl Layout {
    pub(crate) fn new(dlg: HWND) -> Layout {
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

    pub(crate) fn add(
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
    pub(crate) fn add_fixed(&mut self, dlg: HWND, id: i32) {
        use Origin::Near;
        self.add(dlg, id, Near, Near, Near, Near);
    }

    pub(crate) fn resize(&self, width: i32, height: i32) {
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

    pub(crate) fn limit(&self, info: &mut MINMAXINFO) {
        info.ptMinTrackSize.x = self.smallest.0;
        info.ptMinTrackSize.y = self.smallest.1;
        if let Some(h) = self.largest_height {
            info.ptMaxTrackSize.y = h;
        }
    }
}

// ---- 共通 ----

pub(crate) fn item(dlg: HWND, id: i32) -> Option<HWND> {
    // SAFETY: dlg はダイアログ。
    unsafe { GetDlgItem(Some(dlg), id) }.ok()
}

pub(crate) fn text(hwnd: HWND) -> String {
    // SAFETY: hwnd はコントロール。buf は書き込める。
    unsafe {
        let len = GetWindowTextLengthW(hwnd).max(0) as usize;
        let mut buf = vec![0u16; len + 1];
        let n = GetWindowTextW(hwnd, &mut buf).max(0) as usize;
        String::from_utf16_lossy(&buf[..n])
    }
}

pub(crate) fn set_text(hwnd: HWND, s: &str) {
    // SAFETY: hwnd はコントロール。
    let _ = unsafe { SetWindowTextW(hwnd, &HSTRING::from(s)) };
}

/// ダイアログの `GWLP_USERDATA` に置いた状態。
pub(crate) fn state<'a, T>(dlg: HWND) -> Option<&'a mut T> {
    // SAFETY: USERDATA には WM_INITDIALOG で Box<T> の生ポインタを置き、
    // WM_NCDESTROY で外して落とす。0 なら None。
    unsafe { (GetWindowLongPtrW(dlg, GWLP_USERDATA) as *mut T).as_mut() }
}

pub(crate) fn set_state<T>(dlg: HWND, s: Box<T>) {
    // SAFETY: dlg は初期化中のダイアログ。
    unsafe { SetWindowLongPtrW(dlg, GWLP_USERDATA, Box::into_raw(s) as isize) };
}

pub(crate) fn drop_state<T>(dlg: HWND) {
    // SAFETY: set_state が置いたポインタか 0。外してから落とす。
    unsafe {
        let p = SetWindowLongPtrW(dlg, GWLP_USERDATA, 0) as *mut T;
        if !p.is_null() {
            drop(Box::from_raw(p));
        }
    }
}

pub(crate) fn end(dlg: HWND, result: isize) {
    // SAFETY: dlg はモーダルのダイアログ。
    let _ = unsafe { EndDialog(dlg, result) };
}

pub(crate) fn loword(w: WPARAM) -> i32 {
    (w.0 & 0xFFFF) as i32
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

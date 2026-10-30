//! フック DLL から本体へ届く通知の形。DLL と本体の両方がこのクレートを使う。
//!
//! DLL は他プロセスの中で動くので、本体とはメールスロットで話す。
//! 1 通知 = 固定長の見出し（24 バイト）＋ 種類ごとの本文。構造体を直接書かず、
//! フィールドごとにリトルエンディアンで並べる（DLL と本体のビット数が違っても同じ形になる）。
//! DLL 側はヒープを使わずに書けるよう、書き込みはスタックの配列に対して行う。

/// 本体が開き、DLL が書くメールスロット。
pub const MAILSLOT_NAME: &str = r"\\.\mailslot\newmayu\notify";

/// 通知の種類。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// IME の状態。`flags` の bit0 = IME オン、bit1 = 変換中。本文なし。
    Ime,
    /// フォーカスが移った。本文はクラス名の列とタイトルの列（UTF-8）。
    Focus,
}

impl Kind {
    const fn code(self) -> u32 {
        match self {
            Kind::Ime => 1,
            Kind::Focus => 2,
        }
    }
    const fn from_code(c: u32) -> Option<Kind> {
        match c {
            1 => Some(Kind::Ime),
            2 => Some(Kind::Focus),
            _ => None,
        }
    }
}

pub const IME_OPEN: u32 = 1 << 0;
pub const IME_COMPOSING: u32 = 1 << 1;

/// 通知の見出し。全種類に共通。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub kind: Kind,
    /// どこで気付いたか（デバッグ用。DLL 側の分岐番号）。
    pub cause: u32,
    /// 通知元のスレッド。
    pub thread_id: u32,
    /// 通知元の窓（Focus ではフォーカスを持つ窓）。
    pub hwnd: u64,
    pub flags: u32,
}

impl Header {
    pub const SIZE: usize = 24;

    fn write(&self, b: &mut [u8]) {
        b[0..4].copy_from_slice(&self.kind.code().to_le_bytes());
        b[4..8].copy_from_slice(&self.cause.to_le_bytes());
        b[8..12].copy_from_slice(&self.thread_id.to_le_bytes());
        b[12..16].copy_from_slice(&self.flags.to_le_bytes());
        b[16..24].copy_from_slice(&self.hwnd.to_le_bytes());
    }

    fn read(b: &[u8]) -> Option<Header> {
        if b.len() < Self::SIZE {
            return None;
        }
        let u32_at = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
        Some(Header {
            kind: Kind::from_code(u32_at(0))?,
            cause: u32_at(4),
            thread_id: u32_at(8),
            flags: u32_at(12),
            hwnd: u64::from_le_bytes(b[16..24].try_into().unwrap()),
        })
    }

    pub const fn ime_open(&self) -> bool {
        self.flags & IME_OPEN != 0
    }
    pub const fn ime_composing(&self) -> bool {
        self.flags & IME_COMPOSING != 0
    }
}

/// クラス名の列・タイトルの列それぞれの上限（UTF-8 のバイト数）。超えた分は切る。
pub const MAX_NAME: usize = 1024;
/// 1 通知の最大長。メールスロットの読み取りバッファはこれで足りる。
pub const MAX_SIZE: usize = Header::SIZE + 4 + 2 * MAX_NAME;

/// IME 通知を書く。戻り値は書いた長さ。
pub fn write_ime(buf: &mut [u8; MAX_SIZE], h: &Header) -> usize {
    h.write(buf);
    Header::SIZE
}

/// Focus 通知を書く。`class` `title` は UTF-8 のバイト列で、`MAX_NAME` を超えた分は切る。
/// 戻り値は書いた長さ。
pub fn write_focus(buf: &mut [u8; MAX_SIZE], h: &Header, class: &[u8], title: &[u8]) -> usize {
    h.write(buf);
    let class = &class[..class.len().min(MAX_NAME)];
    let title = &title[..title.len().min(MAX_NAME)];
    let mut i = Header::SIZE;
    buf[i..i + 2].copy_from_slice(&(class.len() as u16).to_le_bytes());
    buf[i + 2..i + 4].copy_from_slice(&(title.len() as u16).to_le_bytes());
    i += 4;
    buf[i..i + class.len()].copy_from_slice(class);
    i += class.len();
    buf[i..i + title.len()].copy_from_slice(title);
    i + title.len()
}

/// 本体側で読んだ 1 通知。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notify {
    pub header: Header,
    /// Focus のとき: `exe名:クラス:…:クラス`（親から順、最後がフォーカス窓）。
    pub class: String,
    /// Focus のとき: タイトルの列（同じ順）。
    pub title: String,
}

impl Notify {
    /// 形が違う・種類が知らない値なら `None`。
    pub fn from_bytes(b: &[u8]) -> Option<Notify> {
        let header = Header::read(b)?;
        let (class, title) = match header.kind {
            Kind::Ime => {
                if b.len() != Header::SIZE {
                    return None;
                }
                (String::new(), String::new())
            }
            Kind::Focus => {
                let i = Header::SIZE;
                if b.len() < i + 4 {
                    return None;
                }
                let cl = u16::from_le_bytes([b[i], b[i + 1]]) as usize;
                let tl = u16::from_le_bytes([b[i + 2], b[i + 3]]) as usize;
                if b.len() != i + 4 + cl + tl {
                    return None;
                }
                let class = String::from_utf8_lossy(&b[i + 4..i + 4 + cl]).into_owned();
                let title = String::from_utf8_lossy(&b[i + 4 + cl..]).into_owned();
                (class, title)
            }
        };
        Some(Notify {
            header,
            class,
            title,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ime_roundtrip() {
        let h = Header {
            kind: Kind::Ime,
            cause: 3,
            thread_id: 0x1234,
            hwnd: 0x0000_0001_0002_0003,
            flags: IME_OPEN | IME_COMPOSING,
        };
        let mut buf = [0u8; MAX_SIZE];
        let n = write_ime(&mut buf, &h);
        assert_eq!(n, Header::SIZE);
        let got = Notify::from_bytes(&buf[..n]).unwrap();
        assert_eq!(got.header, h);
        assert!(got.header.ime_open() && got.header.ime_composing());
        assert_eq!(Notify::from_bytes(&buf[..10]), None);
        let mut bad = buf;
        bad[0] = 99;
        assert_eq!(Notify::from_bytes(&bad[..n]), None);
    }

    #[test]
    fn focus_roundtrip_and_truncation() {
        let h = Header {
            kind: Kind::Focus,
            cause: 20,
            thread_id: 7,
            hwnd: 0x10,
            flags: 0,
        };
        let mut buf = [0u8; MAX_SIZE];
        let n = write_focus(
            &mut buf,
            &h,
            "notepad.exe:Notepad:Edit".as_bytes(),
            "無題 - メモ帳".as_bytes(),
        );
        let got = Notify::from_bytes(&buf[..n]).unwrap();
        assert_eq!(got.header, h);
        assert_eq!(got.class, "notepad.exe:Notepad:Edit");
        assert_eq!(got.title, "無題 - メモ帳");
        // 長すぎる名前は切る
        let long = vec![b'x'; MAX_NAME + 50];
        let n = write_focus(&mut buf, &h, &long, &long);
        assert_eq!(n, MAX_SIZE);
        let got = Notify::from_bytes(&buf[..n]).unwrap();
        assert_eq!(got.class.len(), MAX_NAME);
        // 長さが合わなければ読まない
        assert_eq!(Notify::from_bytes(&buf[..n - 1]), None);
    }
}

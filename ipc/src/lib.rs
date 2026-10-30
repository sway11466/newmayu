//! フック DLL から本体へ届く通知の形。DLL と本体の両方がこのクレートを使う。
//!
//! DLL は他プロセスの中で動くので、本体とはメールスロットで話す。
//! 1 通知 = 固定長のバイト列。構造体を直接書かず、フィールドごとにリトルエンディアンで並べる
//! （DLL と本体のビット数が違っても同じ形になる）。

/// 本体が開き、DLL が書くメールスロット。
pub const MAILSLOT_NAME: &str = r"\\.\mailslot\newmayu\notify";

/// 通知の種類。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// IME の状態。`flags` の bit0 = IME オン、bit1 = 変換中。
    Ime,
}

impl Kind {
    const fn code(self) -> u32 {
        match self {
            Kind::Ime => 1,
        }
    }
    const fn from_code(c: u32) -> Option<Kind> {
        match c {
            1 => Some(Kind::Ime),
            _ => None,
        }
    }
}

pub const IME_OPEN: u32 = 1 << 0;
pub const IME_COMPOSING: u32 = 1 << 1;

/// 1 通知。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Notify {
    pub kind: Kind,
    /// どこで気付いたか（デバッグ用。DLL 側の分岐番号）。
    pub cause: u32,
    /// 通知元のスレッド。
    pub thread_id: u32,
    /// 通知元の窓。
    pub hwnd: u64,
    pub flags: u32,
}

impl Notify {
    pub const SIZE: usize = 24;

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut b = [0u8; Self::SIZE];
        b[0..4].copy_from_slice(&self.kind.code().to_le_bytes());
        b[4..8].copy_from_slice(&self.cause.to_le_bytes());
        b[8..12].copy_from_slice(&self.thread_id.to_le_bytes());
        b[12..16].copy_from_slice(&self.flags.to_le_bytes());
        b[16..24].copy_from_slice(&self.hwnd.to_le_bytes());
        b
    }

    /// 長さが違う・種類が知らない値なら `None`。
    pub fn from_bytes(b: &[u8]) -> Option<Notify> {
        if b.len() != Self::SIZE {
            return None;
        }
        let u32_at = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
        let kind = Kind::from_code(u32_at(0))?;
        Some(Notify {
            kind,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let n = Notify {
            kind: Kind::Ime,
            cause: 3,
            thread_id: 0x1234,
            hwnd: 0x0000_0001_0002_0003,
            flags: IME_OPEN | IME_COMPOSING,
        };
        let b = n.to_bytes();
        assert_eq!(b.len(), Notify::SIZE);
        assert_eq!(Notify::from_bytes(&b), Some(n));
        assert!(n.ime_open() && n.ime_composing());
        assert_eq!(Notify::from_bytes(&b[..10]), None);
        let mut bad = b;
        bad[0] = 99;
        assert_eq!(Notify::from_bytes(&bad), None);
    }
}

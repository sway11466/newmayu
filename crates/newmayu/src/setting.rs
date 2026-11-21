//! 設定の一覧（yamy の `yamy.ini`）。
//!
//! exe と同じフォルダの `yamy.ini` の `[yamy]` 節に、yamy と同じ形で持つ。
//! - `.mayu0`〜`.mayu255` = `名前;パス;シンボル`。シンボルは `-D名前` を `;` で区切る
//! - `.mayuIndex` = 選択中の番号
//! - `logFont` = ログダイアログのフォント
//!
//! `.mayu0` から順に読み、無い番号で止まる（yamy と同じ）。

use std::path::{Path, PathBuf};

use windows::Win32::System::WindowsProgramming::{
    GetPrivateProfileIntW, GetPrivateProfileStringW, WritePrivateProfileStringW,
};
use windows::core::{HSTRING, PCWSTR};

/// 一覧の上限（yamy の `MAX_MAYU_REGISTRY_ENTRIES`）。
pub const MAX_ENTRIES: usize = 256;
/// 節の名前。
const SECTION: &str = "yamy";
/// 読み出しの上限（yamy の `GANA_MAX_PATH` = `MAX_PATH * 4`）。
const BUF_LEN: usize = 260 * 4;

/// 一覧の 1 件（yamy の `DlgEditSettingData`）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Entry {
    /// メニューに出る名前。
    pub name: String,
    /// `.mayu` のパス。空なら exe のフォルダの `.mayu`。
    pub path: String,
    /// `-D名前;-D名前…`。書かれたままの文字列。
    pub symbols: String,
}

impl Entry {
    /// `名前;パス;シンボル` を分ける（yamy の `^([^;]*);([^;]*);(.*)$`）。
    pub fn parse(s: &str) -> Option<Entry> {
        let mut it = s.splitn(3, ';');
        let name = it.next()?;
        let path = it.next()?;
        let symbols = it.next()?;
        Some(Entry {
            name: name.to_string(),
            path: path.to_string(),
            symbols: symbols.to_string(),
        })
    }

    /// `yamy.ini` に書く形。
    pub fn to_ini(&self) -> String {
        format!("{};{};{}", self.name, self.path, self.symbols)
    }

    /// シンボルの名前。`-D` の後ろから `;` の手前まで（yamy の `-D([^;]*)(.*)$` のくり返し）。
    pub fn symbol_names(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut rest = self.symbols.as_str();
        while let Some(i) = rest.find("-D") {
            let after = &rest[i + 2..];
            let end = after.find(';').unwrap_or(after.len());
            out.push(after[..end].to_string());
            rest = &after[end..];
        }
        out
    }
}

/// トレイのメニューに出す名前（yamy の `^([^;]*);` の検索）。`;` が無ければ出さない。
pub fn menu_name(raw: &str) -> Option<&str> {
    raw.find(';').map(|i| &raw[..i])
}

/// 起動時・再読込で何を読むか（yamy の `SettingLoader::getFilename`）。
#[derive(Debug, PartialEq, Eq)]
pub enum Lookup {
    /// このファイルを、このシンボルを `define` 済みにして読む。
    Load { path: PathBuf, symbols: Vec<String> },
    /// 選択中の設定のファイルが読めない。
    Unreadable(PathBuf),
    /// 選択中の設定が無く、exe のフォルダに `.mayu` も無い。yamy は設定ダイアログを出す。
    NoSetting,
}

/// 選択中の設定 `selected`（`.mayuIndex` の番号の生の値）から、読むファイルを決める。
pub fn lookup(selected: Option<&str>, exe_dir: &Path, readable: impl Fn(&Path) -> bool) -> Lookup {
    let home = exe_dir.join(".mayu");
    if let Some(entry) = selected.and_then(Entry::parse) {
        let symbols = entry.symbol_names();
        let path = if entry.path.is_empty() {
            home
        } else {
            PathBuf::from(&entry.path)
        };
        return if readable(&path) {
            Lookup::Load { path, symbols }
        } else {
            Lookup::Unreadable(path)
        };
    }
    if readable(&home) {
        Lookup::Load {
            path: home,
            symbols: Vec::new(),
        }
    } else {
        Lookup::NoSetting
    }
}

/// exe と同じフォルダの `yamy.ini`。
#[derive(Clone, Debug)]
pub struct Ini {
    path: PathBuf,
}

impl Ini {
    /// exe のフォルダの `yamy.ini`。
    pub fn beside_exe() -> Result<Ini, String> {
        Ok(Ini {
            path: exe_dir()?.join("yamy.ini"),
        })
    }

    /// `.mayu0` から順に、無い番号まで（生の文字列）。
    pub fn entries(&self) -> Vec<String> {
        (0..MAX_ENTRIES)
            .map_while(|i| self.read(&entry_key(i)))
            .collect()
    }

    #[cfg(test)]
    fn at(path: PathBuf) -> Ini {
        Ini { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// `.mayuIndex`。無ければ 0。
    pub fn index(&self) -> usize {
        // SAFETY: 文字列はすべて NUL 終端の UTF-16（HSTRING）。
        let n = unsafe {
            GetPrivateProfileIntW(
                &HSTRING::from(SECTION),
                &HSTRING::from(".mayuIndex"),
                0,
                &HSTRING::from(self.path.as_os_str()),
            )
        };
        usize::try_from(n).unwrap_or(0)
    }

    /// 選択中の設定（生の文字列）。
    pub fn selected(&self) -> Option<String> {
        self.read(&entry_key(self.index()))
    }

    pub fn set_index(&self, index: usize) -> bool {
        self.write(".mayuIndex", &index.to_string())
    }

    /// 一覧を書き直す（yamy の設定ダイアログの OK）。書いた件数より後ろの番号は消す。
    ///
    /// yamy は `yamy.ini` では後ろの番号を消せず（消す処理が何もせず失敗を返す）、
    /// 削除した件数ぶん一覧の最後の設定が重複して見える。バグと想定し、消すようにした。
    pub fn write_entries(&self, entries: &[Entry]) -> bool {
        let written = entries
            .iter()
            .enumerate()
            .all(|(i, e)| self.write(&entry_key(i), &e.to_ini()));
        let mut removed = true;
        for i in entries.len()..MAX_ENTRIES {
            let key = entry_key(i);
            if self.read(&key).is_none() {
                break;
            }
            removed &= self.remove(&key);
        }
        written && removed
    }

    /// 名前の一致する設定を選ぶ（`&LoadSetting(名前)`）。無ければ `false`。
    pub fn select_by_name(&self, name: &str) -> bool {
        match self
            .entries()
            .iter()
            .position(|raw| Entry::parse(raw).is_some_and(|e| e.name == name))
        {
            Some(i) => self.set_index(i),
            None => false,
        }
    }

    /// ログダイアログのフォント（yamy の `logFont`。`LOGFONT` を `,` で並べた形）。
    pub fn log_font(&self) -> Option<String> {
        self.read("logFont")
    }

    pub fn set_log_font(&self, value: &str) -> bool {
        self.write("logFont", value)
    }

    /// 値。空の値は無いのと同じ（yamy と同じ）。
    fn read(&self, key: &str) -> Option<String> {
        let mut buf = vec![0u16; BUF_LEN];
        // SAFETY: 文字列はすべて NUL 終端の UTF-16（HSTRING）。buf は書き込める。
        let len = unsafe {
            GetPrivateProfileStringW(
                &HSTRING::from(SECTION),
                &HSTRING::from(key),
                &HSTRING::new(),
                Some(&mut buf),
                &HSTRING::from(self.path.as_os_str()),
            )
        } as usize;
        (len > 0).then(|| String::from_utf16_lossy(&buf[..len]))
    }

    fn write(&self, key: &str, value: &str) -> bool {
        // SAFETY: 文字列はすべて NUL 終端の UTF-16（HSTRING）。
        unsafe {
            WritePrivateProfileStringW(
                &HSTRING::from(SECTION),
                &HSTRING::from(key),
                &HSTRING::from(value),
                &HSTRING::from(self.path.as_os_str()),
            )
        }
        .is_ok()
    }

    /// 値を消す（値に NULL を渡すと消える）。
    fn remove(&self, key: &str) -> bool {
        // SAFETY: 文字列はすべて NUL 終端の UTF-16（HSTRING）。値は NULL。
        unsafe {
            WritePrivateProfileStringW(
                &HSTRING::from(SECTION),
                &HSTRING::from(key),
                PCWSTR::null(),
                &HSTRING::from(self.path.as_os_str()),
            )
        }
        .is_ok()
    }
}

fn entry_key(i: usize) -> String {
    format!(".mayu{i}")
}

/// exe のフォルダ。
pub fn exe_dir() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("自分のパスが分からない: {e}"))?;
    exe.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| format!("{} のフォルダが分からない", exe.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_splits_name_path_and_symbols() {
        let e = Entry::parse("109 Emacs;C:\\a\\dot.mayu;-DUSE109;-DUSEdefault").unwrap();
        assert_eq!(e.name, "109 Emacs");
        assert_eq!(e.path, "C:\\a\\dot.mayu");
        assert_eq!(e.symbols, "-DUSE109;-DUSEdefault");
        assert_eq!(e.symbol_names(), ["USE109", "USEdefault"]);
        assert_eq!(
            e.to_ini(),
            "109 Emacs;C:\\a\\dot.mayu;-DUSE109;-DUSEdefault"
        );
        // 区切りが 2 つ無いものは設定ではない
        assert_eq!(Entry::parse("名前;パス"), None);
        assert_eq!(Entry::parse(";;").unwrap(), Entry::default());
    }

    #[test]
    fn symbols_are_taken_after_each_dash_d() {
        let e = Entry {
            symbols: "x-Da;;-Db".to_string(),
            ..Entry::default()
        };
        assert_eq!(e.symbol_names(), ["a", "b"]);
    }

    #[test]
    fn menu_name_needs_a_semicolon() {
        assert_eq!(menu_name("名前;パス;"), Some("名前"));
        assert_eq!(menu_name("名前"), None);
    }

    #[test]
    fn write_entries_removes_the_rest() {
        let ini =
            Ini::at(std::env::temp_dir().join(format!("newmayu-ini-{}.ini", std::process::id())));
        let e = |n: &str| Entry {
            name: n.to_string(),
            ..Entry::default()
        };
        assert!(ini.write_entries(&[e("a"), e("b"), e("c")]));
        assert!(ini.write_entries(&[e("b"), e("c")]));
        // yamy では .mayu2 に c が残り、b・c・c になる
        assert_eq!(ini.entries(), ["b;;", "c;;"]);
        let _ = std::fs::remove_file(ini.path());
    }

    #[test]
    fn lookup_follows_yamy() {
        let dir = Path::new("C:\\bin");
        let home = dir.join(".mayu");
        let all = |_: &Path| true;
        let none = |_: &Path| false;
        // 選択中の設定のパス
        assert_eq!(
            lookup(Some("a;C:\\x.mayu;-DA"), dir, all),
            Lookup::Load {
                path: PathBuf::from("C:\\x.mayu"),
                symbols: vec!["A".to_string()]
            }
        );
        // パスが空なら exe のフォルダの .mayu
        assert_eq!(
            lookup(Some("a;;"), dir, all),
            Lookup::Load {
                path: home.clone(),
                symbols: vec![]
            }
        );
        // 選択中の設定が読めなければ、.mayu を探さずに失敗
        assert_eq!(
            lookup(Some("a;C:\\x.mayu;"), dir, |p: &Path| p == home),
            Lookup::Unreadable(PathBuf::from("C:\\x.mayu"))
        );
        // 選択中の設定が無ければ exe のフォルダの .mayu、それも無ければ設定ダイアログ
        assert_eq!(
            lookup(None, dir, all),
            Lookup::Load {
                path: home,
                symbols: vec![]
            }
        );
        assert_eq!(lookup(None, dir, none), Lookup::NoSetting);
        assert_eq!(lookup(Some("形が違う"), dir, none), Lookup::NoSetting);
    }
}

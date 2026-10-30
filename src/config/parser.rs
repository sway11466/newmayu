//! `.mayu` の構文解析と `model::Config` への変換。yamy の `SettingLoader` を写している。
//!
//! 読める文（最小サブセット）:
//!
//! ```text
//! include "file"
//! define SYMBOL
//! if ( [!] SYMBOL ) [文] / and ( [!] SYMBOL ) / else [文] / elseif ( [!] SYMBOL ) / endif
//! def key NAME... = [E0-|E1-]SCANCODE          # NAME を ( ) で囲む形も可。スキャンコードは 1 個
//! def mod (shift|alt|control|windows) = KEY...
//! def alias NAME = KEY
//! def sync = SCANCODE                          # 読むだけ（&Sync は未対応）
//! keymap NAME [: PARENT] [= ACTION...]        # 同名の再宣言はそのキーマップに戻る。= は割付の無いキーの既定動作
//! keymap2 NAME [: PARENT] [= ACTION...]       # keymap と同じ（yamy でも同じ）
//! window NAME [WINDOW] [: PARENT] [= ACTION...]
//!                                              # WINDOW = /クラス/ | ( /クラス/ && /タイトル/ ) | ( /クラス/ || /タイトル/ )
//! key MODKEY... = ACTION...                    # ACTION = 修飾付きキー名 | $keyseq名 | ( ACTION... ) | &Prefix(KEYMAP)
//!                                              #        | &Default | &KeymapParent | &OtherWindowClass
//!                                              #        | &Window…([引数]) | &MouseMove(dx, dy)   （model::Func 参照）
//!                                              # MODKEY の修飾には IL-（IME オン）IC-/I-（変換中）も書ける
//! mod MODNAME (=|+=|-=) KEY...
//! keyseq $NAME = ACTION...                     # 使う側で展開する
//! ```
//!
//! 修飾の書き方は yamy の `load_MODIFIER` と同じ: `*` は直後の修飾（キー名の直前なら未指定の全部）を
//! dontcare、`~` は離し。未指定の基本修飾（S- A- C- W-）は離し、M0-〜M9- は dontcare。
//! `U-` `D-` は押し・離しの区別（両方または無指定なら両方）。
//! 未指定の `IC-` は「変換中でない」を要求し（yamy の既定）、`IL-` は dontcare。
//!
//! これ以外（`event`、`def option`、`def subst`、上記以外の `&関数`、`!`/`!!`/`!!!`、R- NL- CL- 等の割当修飾、2 個以上のスキャンコード）は
//! エラーにする。
//!
//! ファイルは BOM 無し UTF-8 のみ。大文字小文字は区別しない。

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};

use super::lexer::{Line, Token, tokenize};
use crate::model::{
    Action, BasicMod, Config, EdgeSpec, Func, Gravity, Key, Keymap, KeymapId, LockMod, LockSpec,
    ModSpec, ScanPrefix, Target, WindowMatch, WindowOp,
};

/// 最大の include 深さ。循環 include もこれで止まる。
const MAX_INCLUDE_DEPTH: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadError {
    pub file: String,
    pub line: usize,
    pub message: String,
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.file, self.line, self.message)
    }
}

impl std::error::Error for LoadError {}

type Result<T> = std::result::Result<T, LoadError>;

/// ファイルから読み込む。`include` は読んでいるファイルの場所からの相対パス。
pub fn load(path: &Path) -> Result<Config> {
    let mut loader = Loader::new();
    loader.load_file(path, 0)?;
    loader.finish()
}

/// 文字列から読み込む（テスト用）。`include` は `dir` からの相対パス。
pub fn load_str(src: &str, name: &str, dir: &Path) -> Result<Config> {
    let mut loader = Loader::new();
    loader.load_source(src, name, dir, 0)?;
    loader.finish()
}

/// `mod NAME (=|+=|-=) KEY` の 1 項。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ModOp {
    New,
    Add,
    Sub,
}

/// 構築中のキーマップ。`mod` 文は親の確定を待つので、ここでは演算のまま持つ。
struct KeymapBuild {
    keymap: Keymap,
    mod_ops: [Vec<(ModOp, Key)>; BasicMod::ALL.len()],
}

struct Loader {
    /// `def key` で定義したキー。
    keys: Vec<Key>,
    /// キー名（小文字化）→ `keys` の添字。
    names: HashMap<String, usize>,
    /// `def alias` の別名（小文字化）→ `keys` の添字。
    aliases: HashMap<String, usize>,
    /// `def mod`。
    def_mods: [Vec<Key>; BasicMod::ALL.len()],
    /// `define` したシンボル（小文字化）。`if` の条件に使う。
    symbols: HashSet<String>,
    /// `if` の入れ子ごとの「いま読むか」。yamy の `m_canReadStack`。
    can_read: Vec<bool>,
    /// `keyseq $名`（小文字化）→ 展開済みの動作列。
    keyseqs: HashMap<String, Vec<Action>>,
    keymaps: Vec<KeymapBuild>,
    /// キーマップ名（小文字化）→ `keymaps` の添字。
    keymap_names: HashMap<String, usize>,
    current: usize,
    /// エラー表示用。読んでいるファイルと行。
    file: String,
    line: usize,
}

/// 1 文のトークン列を前から読むカーソル。
struct Cur<'a> {
    toks: &'a [Token],
    i: usize,
}

impl<'a> Cur<'a> {
    fn peek(&self) -> Option<&'a Token> {
        self.toks.get(self.i)
    }
    fn next(&mut self) -> Option<&'a Token> {
        let t = self.toks.get(self.i);
        self.i += 1;
        t
    }
    fn at_end(&self) -> bool {
        self.i >= self.toks.len()
    }
    fn peek_sym(&self, s: &str) -> bool {
        matches!(self.peek(), Some(Token::Sym(x)) if *x == s)
    }
}

fn lower(s: &str) -> String {
    s.to_ascii_lowercase()
}

fn basic_mod_by_name(name: &str) -> Option<BasicMod> {
    Some(match lower(name).as_str() {
        "shift" => BasicMod::Shift,
        "alt" | "meta" | "menu" => BasicMod::Alt,
        "control" | "ctrl" => BasicMod::Control,
        "windows" | "win" => BasicMod::Windows,
        "mod0" => BasicMod::Mod0,
        "mod1" => BasicMod::Mod1,
        "mod2" => BasicMod::Mod2,
        "mod3" => BasicMod::Mod3,
        "mod4" => BasicMod::Mod4,
        "mod5" => BasicMod::Mod5,
        "mod6" => BasicMod::Mod6,
        "mod7" => BasicMod::Mod7,
        "mod8" => BasicMod::Mod8,
        "mod9" => BasicMod::Mod9,
        _ => return None,
    })
}

/// `S-` `M3-` など基本修飾の接頭辞。
fn basic_mod_by_prefix(sym: &str) -> Option<BasicMod> {
    Some(match sym {
        "S-" => BasicMod::Shift,
        "A-" | "M-" => BasicMod::Alt,
        "C-" => BasicMod::Control,
        "W-" => BasicMod::Windows,
        "M0-" => BasicMod::Mod0,
        "M1-" => BasicMod::Mod1,
        "M2-" => BasicMod::Mod2,
        "M3-" => BasicMod::Mod3,
        "M4-" => BasicMod::Mod4,
        "M5-" => BasicMod::Mod5,
        "M6-" => BasicMod::Mod6,
        "M7-" => BasicMod::Mod7,
        "M8-" => BasicMod::Mod8,
        "M9-" => BasicMod::Mod9,
        _ => return None,
    })
}

/// 読めるが未対応の修飾接頭辞（割当修飾・ロック類）。
const UNSUPPORTED_MOD_PREFIXES: &[&str] = &[
    "R-", "NL-", "CL-", "SL-", "KL-", "MAX-", "MIN-", "MMAX-", "MMIN-", "T-", "TS-", "L0-", "L1-",
    "L2-", "L3-", "L4-", "L5-", "L6-", "L7-", "L8-", "L9-",
];

fn lock_mod_by_prefix(s: &str) -> Option<LockMod> {
    match s {
        "IL-" => Some(LockMod::ImeLock),
        "IC-" | "I-" => Some(LockMod::ImeComp),
        _ => None,
    }
}

/// `&関数( … )` の引数の読み出し。省略した引数は既定値。
struct Args<'a> {
    fname: &'a str,
    args: &'a [Token],
    loader: &'a Loader,
}

impl Args<'_> {
    fn int(&self, i: usize) -> Result<i32> {
        match self.args.get(i) {
            Some(Token::Number(v, _)) => match i32::try_from(*v) {
                Ok(v) => Ok(v),
                Err(_) => self
                    .loader
                    .err(format!("`&{}`: 数が大きすぎる", self.fname)),
            },
            Some(t) => self.loader.err(format!(
                "`&{}`: {} 番目の引数は数: `{t}`",
                self.fname,
                i + 1
            )),
            None => self.loader.err(format!(
                "`&{}`: {} 番目の引数（数）が無い",
                self.fname,
                i + 1
            )),
        }
    }

    /// 省略時は `Overlapped`。
    fn target(&self, i: usize) -> Result<Target> {
        match self.args.get(i) {
            None => Ok(Target::Overlapped),
            Some(t) => {
                let name = self.loader.name_of(t)?;
                match lower(&name).as_str() {
                    "overlapped" => Ok(Target::Overlapped),
                    "mdi" => Ok(Target::Mdi),
                    _ => self.loader.err(format!(
                        "`&{}`: 対象は overlapped か mdi: `{name}`",
                        self.fname
                    )),
                }
            }
        }
    }

    fn gravity(&self, i: usize) -> Result<Gravity> {
        let Some(t) = self.args.get(i) else {
            return self.loader.err(format!(
                "`&{}`: {} 番目の引数（向き）が無い",
                self.fname,
                i + 1
            ));
        };
        let name = self.loader.name_of(t)?;
        Ok(match lower(&name).as_str() {
            "c" => Gravity::Center,
            "n" => Gravity::North,
            "e" => Gravity::East,
            "w" => Gravity::West,
            "s" => Gravity::South,
            "nw" => Gravity::NorthWest,
            "ne" => Gravity::NorthEast,
            "sw" => Gravity::SouthWest,
            "se" => Gravity::SouthEast,
            _ => {
                return self.loader.err(format!(
                    "`&{}`: 向きは C N E W S NW NE SW SE のどれか: `{name}`",
                    self.fname
                ));
            }
        })
    }

    /// `&WindowClingTo…([対象])` は `&WindowMoveTo(向き, 0, 0 [, 対象])`。
    fn cling(&self, gravity: Gravity) -> Result<Func> {
        Ok(Func::WindowMoveTo {
            gravity,
            dx: 0,
            dy: 0,
            target: self.target(0)?,
        })
    }

    fn no_more(&self, max: usize) -> Result<()> {
        if self.args.len() > max {
            return self
                .loader
                .err(format!("`&{}`: 引数が多い（最大 {max} 個）", self.fname));
        }
        Ok(())
    }
}

/// 修飾の解析結果。
struct Modified {
    mods: ModSpec,
    edge: EdgeSpec,
    locks: LockSpec,
    /// `IL-` `IC-` が書かれていた。出力キー側では禁止なので、呼び手が見る。
    locks_specified: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Flag {
    Press,
    Release,
    Dontcare,
}

impl Loader {
    fn new() -> Self {
        let global = KeymapBuild {
            keymap: Keymap::global(),
            mod_ops: Default::default(),
        };
        let mut keymap_names = HashMap::new();
        keymap_names.insert("global".to_string(), 0);
        Loader {
            keys: Vec::new(),
            names: HashMap::new(),
            aliases: HashMap::new(),
            def_mods: Default::default(),
            symbols: HashSet::new(),
            can_read: Vec::new(),
            keyseqs: HashMap::new(),
            keymaps: vec![global],
            keymap_names,
            current: 0,
            file: String::new(),
            line: 0,
        }
    }

    fn err<T>(&self, message: impl Into<String>) -> Result<T> {
        Err(LoadError {
            file: self.file.clone(),
            line: self.line,
            message: message.into(),
        })
    }

    fn load_file(&mut self, path: &Path, depth: usize) -> Result<()> {
        let name = path.display().to_string();
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                self.file = name;
                self.line = 0;
                return self.err(format!("開けない: {e}"));
            }
        };
        if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
            self.file = name;
            self.line = 1;
            return self.err("BOM 付き UTF-8 は読めない。BOM 無しで保存してください");
        }
        let src = match std::str::from_utf8(&bytes) {
            Ok(s) => s,
            Err(e) => {
                self.file = name;
                self.line = bytes[..e.valid_up_to()]
                    .iter()
                    .filter(|b| **b == b'\n')
                    .count()
                    + 1;
                return self.err("UTF-8 として読めない。CP932 なら UTF-8 に変換してください");
            }
        };
        let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        self.load_source(src, &name, &dir, depth)
    }

    fn load_source(&mut self, src: &str, name: &str, dir: &Path, depth: usize) -> Result<()> {
        self.file = name.to_string();
        self.line = 0;
        if depth > MAX_INCLUDE_DEPTH {
            return self.err(format!("include が {MAX_INCLUDE_DEPTH} 段を超えた"));
        }
        let lines = match tokenize(src) {
            Ok(l) => l,
            Err(e) => {
                self.line = e.line;
                return self.err(e.message);
            }
        };
        for line in &lines {
            self.load_line(line, dir, depth)?;
            // include から戻ったらファイル名を戻す
            self.file = name.to_string();
        }
        Ok(())
    }

    fn load_line(&mut self, line: &Line, dir: &Path, depth: usize) -> Result<()> {
        self.line = line.line;
        let mut cur = Cur {
            toks: &line.tokens,
            i: 0,
        };
        self.load_statement(&mut cur, dir, depth)
    }

    /// 1 文。`if ( SYM ) key A = B` のように if の後ろにも文が続くので、カーソルを受ける。
    fn load_statement(&mut self, cur: &mut Cur, dir: &Path, depth: usize) -> Result<()> {
        let Some(t) = cur.next() else {
            return self.err("文が空");
        };
        let Token::Name(head) = t else {
            return self.err(format!("文の先頭が名前でない: {t}"));
        };
        let head_l = lower(head);
        // if / else / endif は読まない区間でも見る
        match head_l.as_str() {
            "if" | "and" => return self.load_if(cur, dir, depth),
            "else" => return self.load_else(cur, dir, depth, false, head),
            "elseif" | "elsif" | "elif" | "or" => {
                return self.load_else(cur, dir, depth, true, head);
            }
            "endif" => {
                self.load_endif(head)?;
                return self.expect_end(cur);
            }
            _ => {}
        }
        if self.can_read.last() == Some(&false) {
            return Ok(());
        }
        match head_l.as_str() {
            "include" => self.load_include(cur, dir, depth),
            "define" => self.load_define(cur),
            "def" => self.load_def(cur),
            "keymap" | "keymap2" => self.load_keymap(cur, false),
            "window" => self.load_keymap(cur, true),
            "key" => self.load_key(cur),
            "mod" => self.load_mod(cur),
            "keyseq" => self.load_keyseq(cur),
            "event" => self.err(format!("`{head}` は未対応")),
            _ => self.err(format!("知らない文 `{head}`")),
        }
    }

    // ---- if / else / endif ----------------------------------------------
    //
    // yamy と同じ仕組み。`if ( SYM )` は「いま読むか」を積み、`endif` で降ろす。
    // 同じ行に続く文（`if ( SYM ) key A = B`、`if ( A ) and ( B )`）は、その場で処理してから
    // 積んだ分を調整する: 続く文がさらに積んだら（`and`）それと合成して 1 段にまとめ、
    // 積まなかったら（`key`）1 行だけの if なので降ろす。

    /// `if ( [!] SYMBOL ) [文]`
    fn load_if(&mut self, cur: &mut Cur, dir: &Path, depth: usize) -> Result<()> {
        self.expect_sym(cur, "(")?;
        let mut not = false;
        if cur.peek_sym("!") {
            cur.next();
            not = true;
        }
        let Some(t) = cur.next() else {
            return self.err("if ( の後はシンボル名");
        };
        let sym = self.name_of(t)?;
        self.expect_sym(cur, ")")?;
        let exists = self.symbols.contains(&lower(&sym));
        let does_read = (exists != not) && self.can_read.last().copied().unwrap_or(true);
        self.can_read.push(does_read);
        self.load_rest_of_if_line(cur, dir, depth, does_read)
    }

    /// `else [文]` / `elseif ( SYMBOL ) [文]`
    fn load_else(
        &mut self,
        cur: &mut Cur,
        dir: &Path,
        depth: usize,
        is_else_if: bool,
        word: &str,
    ) -> Result<()> {
        let was_read = self.load_endif(word)?;
        let does_read = !was_read && self.can_read.last().copied().unwrap_or(true);
        self.can_read.push(does_read);
        if is_else_if {
            // `elseif ( SYM )` は else の段の上に if を重ね、1 段にまとめる
            if cur.at_end() {
                return self.err(format!("`{word}` の後は ( シンボル )"));
            }
            let len = self.can_read.len();
            self.load_if(cur, dir, depth)?;
            return self.merge_if_level(len, does_read);
        }
        self.load_rest_of_if_line(cur, dir, depth, does_read)
    }

    /// `endif`。降ろした段の値を返す。
    fn load_endif(&mut self, word: &str) -> Result<bool> {
        match self.can_read.pop() {
            Some(r) => Ok(r),
            None => self.err(format!("対応する if の無い `{word}`")),
        }
    }

    fn load_rest_of_if_line(
        &mut self,
        cur: &mut Cur,
        dir: &Path,
        depth: usize,
        does_read: bool,
    ) -> Result<()> {
        if cur.at_end() {
            return Ok(());
        }
        let len = self.can_read.len();
        self.load_statement(cur, dir, depth)?;
        self.merge_if_level(len, does_read)
    }

    /// 同じ行の続きの文を処理した後の段の調整。`len` は続きを読む前の段数。
    fn merge_if_level(&mut self, len: usize, does_read: bool) -> Result<()> {
        if self.can_read.len() > len {
            // 続きが `and ( SYM )` などで積んだ: 自分の段に合成する
            let r = self.can_read.pop().unwrap_or(true);
            self.can_read[len - 1] = r && does_read;
        } else if self.can_read.len() == len {
            // 続きが普通の文: 1 行だけの if なので降ろす
            self.can_read.pop();
        }
        // 続きが `endif` で自分の段を降ろした場合は何もしない
        Ok(())
    }

    // ---- 文ごとの読み込み ------------------------------------------------

    fn load_include(&mut self, cur: &mut Cur, dir: &Path, depth: usize) -> Result<()> {
        let Some(Token::Str(file)) = cur.next() else {
            return self.err("include の後はファイル名の文字列");
        };
        self.expect_end(cur)?;
        let path = PathBuf::from(file);
        let path = if path.is_absolute() {
            path
        } else {
            dir.join(path)
        };
        let saved_line = self.line;
        self.load_file(&path, depth + 1)?;
        self.line = saved_line;
        Ok(())
    }

    /// `define SYMBOL`
    fn load_define(&mut self, cur: &mut Cur) -> Result<()> {
        let Some(t) = cur.next() else {
            return self.err("define の後はシンボル名");
        };
        let sym = self.name_of(t)?;
        self.expect_end(cur)?;
        self.symbols.insert(lower(&sym));
        Ok(())
    }

    fn load_def(&mut self, cur: &mut Cur) -> Result<()> {
        let Some(Token::Name(what)) = cur.next() else {
            return self.err("def の後は key / mod / alias / sync");
        };
        match lower(what).as_str() {
            "key" => self.load_def_key(cur),
            "mod" => self.load_def_mod(cur),
            "alias" => self.load_def_alias(cur),
            "sync" => {
                self.expect_sym(cur, "=")?;
                // 読むだけ。&Sync はサブセット外
                let _ = self.load_scan_code(cur)?;
                self.expect_end(cur)
            }
            "option" | "subst" => self.err(format!("`def {what}` は未対応")),
            _ => self.err(format!("知らない定義 `def {what}`")),
        }
    }

    /// `def key NAME... = SCANCODE` / `def key ( NAME... ) = SCANCODE`
    fn load_def_key(&mut self, cur: &mut Cur) -> Result<()> {
        let mut names = Vec::new();
        if cur.peek_sym("(") {
            cur.next();
            loop {
                match cur.next() {
                    Some(Token::Sym(")")) => break,
                    Some(t) => names.push(self.name_of(t)?),
                    None => return self.err("`)` が無い"),
                }
            }
            self.expect_sym(cur, "=")?;
        } else {
            loop {
                match cur.next() {
                    Some(Token::Sym("=")) => break,
                    Some(t) => names.push(self.name_of(t)?),
                    None => return self.err("`=` が無い"),
                }
            }
        }
        if names.is_empty() {
            return self.err("キー名が無い");
        }
        let key = self.load_scan_code(cur)?;
        if !cur.at_end() {
            return self.err("スキャンコードが 2 個以上のキーは未対応（backlog feature-2）");
        }
        let idx = self.keys.len();
        for n in &names {
            let l = lower(n);
            if self.names.contains_key(&l) || self.aliases.contains_key(&l) {
                return self.err(format!("キー名 `{n}` は定義済み"));
            }
            self.names.insert(l, idx);
        }
        self.keys.push(key);
        Ok(())
    }

    /// `[E0-|E1-] NUMBER`
    fn load_scan_code(&mut self, cur: &mut Cur) -> Result<Key> {
        let mut prefix = ScanPrefix::None;
        loop {
            match cur.next() {
                Some(Token::Number(n, text)) => {
                    let Ok(code) = u8::try_from(*n) else {
                        return self.err(format!("スキャンコード {text} が 0..=0xff の外"));
                    };
                    return Ok(Key::new(prefix, code));
                }
                Some(Token::Sym("E0-")) => prefix = ScanPrefix::E0,
                Some(Token::Sym("E1-")) => prefix = ScanPrefix::E1,
                Some(t) => return self.err(format!("スキャンコードの位置に `{t}`")),
                None => return self.err("スキャンコードが無い"),
            }
        }
    }

    /// `def mod MODNAME = KEY...`
    fn load_def_mod(&mut self, cur: &mut Cur) -> Result<()> {
        let Some(Token::Name(name)) = cur.next() else {
            return self.err("def mod の後は修飾名");
        };
        let m = match basic_mod_by_name(name) {
            Some(m @ (BasicMod::Shift | BasicMod::Alt | BasicMod::Control | BasicMod::Windows)) => {
                m
            }
            _ => {
                return self.err(format!(
                    "`{name}` は def mod にできない（shift / alt / control / windows）"
                ));
            }
        };
        self.expect_sym(cur, "=")?;
        if cur.at_end() {
            return self.err("キー名が無い");
        }
        while let Some(t) = cur.next() {
            let key = self.key_by_non_alias_name(t)?;
            if !self.def_mods[m.index()].contains(&key) {
                self.def_mods[m.index()].push(key);
            }
        }
        Ok(())
    }

    /// `def alias NAME = KEY`
    fn load_def_alias(&mut self, cur: &mut Cur) -> Result<()> {
        let Some(t) = cur.next() else {
            return self.err("def alias の後は別名");
        };
        let alias = self.name_of(t)?;
        self.expect_sym(cur, "=")?;
        let Some(t) = cur.next() else {
            return self.err("別名の後はキー名");
        };
        let idx = self.key_index_by_non_alias_name(t)?;
        self.expect_end(cur)?;
        let l = lower(&alias);
        if self.names.contains_key(&l) || self.aliases.contains_key(&l) {
            return self.err(format!("キー名 `{alias}` は定義済み"));
        }
        self.aliases.insert(l, idx);
        Ok(())
    }

    /// `keymap NAME [: PARENT] [= ACTION...]` / `window NAME [WINDOW] [: PARENT] [= ACTION...]`
    fn load_keymap(&mut self, cur: &mut Cur, is_window: bool) -> Result<()> {
        let Some(t) = cur.next() else {
            return self.err("keymap の後は名前");
        };
        let name = self.name_of(t)?;
        // 窓の条件。`window NAME : PARENT` のように条件が無ければ、yamy と同じく普通の keymap
        let mut window = None;
        if is_window {
            if cur.peek_sym("(") {
                cur.next();
                let class = self.regex_of(cur.next())?;
                let op = match cur.next() {
                    Some(Token::Sym("&&")) => WindowOp::And,
                    Some(Token::Sym("||")) => WindowOp::Or,
                    Some(t) => return self.err(format!("`&&` か `||` が要る所に `{t}`")),
                    None => return self.err("`&&` か `||` が無い"),
                };
                let title = self.regex_of(cur.next())?;
                self.expect_sym(cur, ")")?;
                window = Some(self.window_match(&class, Some(&title), op)?);
            } else if let Some(Token::Regex(_)) = cur.peek() {
                let class = self.regex_of(cur.next())?;
                window = Some(self.window_match(&class, None, WindowOp::And)?);
            }
        }
        let mut parent = None;
        if cur.peek_sym(":") {
            cur.next();
            let Some(t) = cur.next() else {
                return self.err("`:` の後は親キーマップ名");
            };
            let pname = self.name_of(t)?;
            let Some(&idx) = self.keymap_names.get(&lower(&pname)) else {
                return self.err(format!("キーマップ `{pname}` は未定義"));
            };
            parent = Some(KeymapId(idx));
        }
        // 既定動作（割付の無いキーに使う）
        let mut default_actions = None;
        if cur.peek_sym("=") || cur.peek_sym("=>") {
            cur.next();
            default_actions = Some(self.load_actions(cur, false)?);
        }
        self.expect_end(cur)?;
        let l = lower(&name);
        // 同名の再宣言はそのキーマップに戻る（yamy の `Keymaps::add` は既存を返す）。
        // yamy は 2 回目の親指定・既定動作を黙って捨てるが、ここでは食い違えばエラーにする。
        if let Some(&idx) = self.keymap_names.get(&l) {
            let km = &self.keymaps[idx].keymap;
            if parent.is_some() && parent != km.parent {
                return self.err(format!(
                    "キーマップ `{name}` は定義済みで、親は変えられない"
                ));
            }
            if default_actions.is_some_and(|a| a != km.default_actions) {
                return self.err(format!(
                    "キーマップ `{name}` は定義済みで、既定動作は変えられない"
                ));
            }
            if window.is_some() && window != km.window {
                return self.err(format!(
                    "キーマップ `{name}` は定義済みで、窓の条件は変えられない"
                ));
            }
            self.current = idx;
            return Ok(());
        }
        let idx = self.keymaps.len();
        let mut keymap = Keymap::new(name, parent);
        keymap.window = window;
        if let Some(a) = default_actions {
            keymap.default_actions = a;
        }
        self.keymaps.push(KeymapBuild {
            keymap,
            mod_ops: Default::default(),
        });
        self.keymap_names.insert(l, idx);
        self.current = idx;
        Ok(())
    }

    fn regex_of(&self, t: Option<&Token>) -> Result<String> {
        match t {
            Some(Token::Regex(s)) => Ok(s.clone()),
            Some(t) => self.err(format!("正規表現 `/…/` が要る所に `{t}`")),
            None => self.err("正規表現 `/…/` が無い"),
        }
    }

    fn window_match(&self, class: &str, title: Option<&str>, op: WindowOp) -> Result<WindowMatch> {
        match WindowMatch::new(class, title, op) {
            Ok(w) => Ok(w),
            Err(e) => self.err(format!("正規表現が読めない: {e}")),
        }
    }

    /// `key MODKEY... = ACTION...`
    fn load_key(&mut self, cur: &mut Cur) -> Result<()> {
        let mut lhs = Vec::new();
        loop {
            let m = self.load_modifier(cur)?;
            match cur.peek() {
                Some(Token::Sym("=")) | Some(Token::Sym("=>")) if lhs.is_empty() => {
                    return self.err("既定修飾の変更（`key 修飾 = ...`）は未対応");
                }
                _ => {}
            }
            let Some(t) = cur.next() else {
                return self.err("`=` が無い");
            };
            let key = self.key_by_name(t)?;
            lhs.push((key, m));
            match cur.peek() {
                Some(Token::Sym("=")) | Some(Token::Sym("=>")) => {
                    cur.next();
                    break;
                }
                _ => {}
            }
        }
        let actions = self.load_actions(cur, false)?;
        let km = &mut self.keymaps[self.current].keymap;
        for (key, m) in lhs {
            km.assign(key, m.mods, m.edge, m.locks, actions.clone());
        }
        Ok(())
    }

    /// `mod MODNAME (=|+=|-=) KEY...`
    fn load_mod(&mut self, cur: &mut Cur) -> Result<()> {
        let Some(t) = cur.next() else {
            return self.err("mod の後は修飾名");
        };
        if let Token::Sym(s @ ("!" | "!!" | "!!!")) = t {
            return self.err(format!("`{s}`（修飾キーの単独押し動作）は未対応"));
        }
        let name = self.name_of(t)?;
        let Some(m) = basic_mod_by_name(&name) else {
            return self.err(format!("`{name}` は修飾名でない"));
        };
        let mut op = match cur.next() {
            Some(Token::Sym("=")) => ModOp::New,
            Some(Token::Sym("+=")) => ModOp::Add,
            Some(Token::Sym("-=")) => ModOp::Sub,
            Some(t) => return self.err(format!("修飾名の後は = / += / -=（`{t}`）")),
            None => return self.err("修飾名の後は = / += / -="),
        };
        if cur.at_end() {
            return self.err("キー名が無い");
        }
        while let Some(t) = cur.next() {
            if let Token::Sym(s @ ("!" | "!!" | "!!!")) = t {
                return self.err(format!("`{s}`（修飾キーの単独押し動作）は未対応"));
            }
            let key = self.key_by_name(t)?;
            let ops = &mut self.keymaps[self.current].mod_ops[m.index()];
            match op {
                ModOp::New => {
                    ops.clear();
                    ops.push((ModOp::New, key));
                    op = ModOp::Add;
                }
                _ => {
                    if let Some(e) = ops.iter_mut().find(|(_, k)| *k == key) {
                        e.0 = op;
                    } else {
                        ops.push((op, key));
                    }
                }
            }
        }
        Ok(())
    }

    /// `keyseq $NAME = ACTION...`
    fn load_keyseq(&mut self, cur: &mut Cur) -> Result<()> {
        if !cur.peek_sym("$") {
            return self.err("keyseq の後は `$名`");
        }
        cur.next();
        let Some(t) = cur.next() else {
            return self.err("`$` の後は名前");
        };
        let name = self.name_of(t)?;
        self.expect_sym(cur, "=")?;
        let actions = self.load_actions(cur, false)?;
        self.keyseqs.insert(lower(&name), actions);
        Ok(())
    }

    // ---- 動作列と修飾 ----------------------------------------------------

    /// `ACTION...`。`in_paren` なら `)` で止まる。
    fn load_actions(&mut self, cur: &mut Cur, in_paren: bool) -> Result<Vec<Action>> {
        let mut out = Vec::new();
        while !cur.at_end() {
            let m = self.load_modifier(cur)?;
            if m.locks_specified {
                return self.err("IL- IC- は出力キーには付けられない");
            }
            let Some(t) = cur.next() else {
                return self.err("修飾の後にキー名が無い");
            };
            match t {
                Token::Sym(")") if in_paren => return Ok(out),
                Token::Sym("(") => {
                    let inner = self.load_actions(cur, true)?;
                    out.extend(inner);
                }
                Token::Sym("$") => {
                    let Some(t) = cur.next() else {
                        return self.err("`$` の後は keyseq 名");
                    };
                    let name = self.name_of(t)?;
                    let Some(seq) = self.keyseqs.get(&lower(&name)) else {
                        return self.err(format!("keyseq `${name}` は未定義"));
                    };
                    out.extend(seq.iter().cloned());
                }
                Token::Sym("&") => {
                    let Some(t) = cur.next() else {
                        return self.err("`&` の後は関数名");
                    };
                    let fname = self.name_of(t)?;
                    if fname.eq_ignore_ascii_case("Default") {
                        out.push(Action::Default);
                        continue;
                    }
                    if fname.eq_ignore_ascii_case("KeymapParent") {
                        out.push(Action::KeymapParent);
                        continue;
                    }
                    if fname.eq_ignore_ascii_case("OtherWindowClass") {
                        out.push(Action::OtherWindowClass);
                        continue;
                    }
                    if let Some(f) = self.load_func(&fname, cur)? {
                        out.push(Action::Func(f));
                        continue;
                    }
                    if !fname.eq_ignore_ascii_case("Prefix") {
                        return self.err(format!("`&{fname}` は未対応"));
                    }
                    self.expect_sym(cur, "(")?;
                    let Some(t) = cur.next() else {
                        return self.err("&Prefix( の後はキーマップ名");
                    };
                    let kname = self.name_of(t)?;
                    let Some(&idx) = self.keymap_names.get(&lower(&kname)) else {
                        return self.err(format!("キーマップ `{kname}` は未定義"));
                    };
                    self.expect_sym(cur, ")")?;
                    out.push(Action::Prefix(KeymapId(idx)));
                }
                t => {
                    let key = self.key_by_name(t)?;
                    out.push(Action::Key {
                        key,
                        mods: m.mods,
                        edge: m.edge,
                    });
                }
            }
        }
        if in_paren {
            return self.err("`)` が無い");
        }
        Ok(out)
    }

    /// OS 側で実行する `&関数` とその引数。知らない名前なら `None`。
    /// 引数の `( … )` は省略でき、省略した引数は yamy と同じ既定値になる。
    fn load_func(&self, fname: &str, cur: &mut Cur) -> Result<Option<Func>> {
        let lname = lower(fname);
        // 引数の最大数。知らない名前は None（引数を読まずに返す）
        let max = match lname.as_str() {
            "windowmove" | "windowresizeto" => 3,
            "windowmoveto" => 4,
            "mousemove" => 2,
            "windowtoggletopmost" | "windowredraw" => 0,
            "windowraise"
            | "windowlower"
            | "windowminimize"
            | "windowmaximize"
            | "windowhmaximize"
            | "windowvmaximize"
            | "windowmovevisibly"
            | "windowclose"
            | "windowclingtoleft"
            | "windowclingtoright"
            | "windowclingtotop"
            | "windowclingtobottom"
            | "windowsetalpha" => 1,
            _ => return Ok(None),
        };
        let args = self.load_args(cur)?;
        let a = Args {
            fname,
            args: &args,
            loader: self,
        };
        a.no_more(max)?;
        let f = match lname.as_str() {
            "windowraise" => Func::WindowRaise(a.target(0)?),
            "windowlower" => Func::WindowLower(a.target(0)?),
            "windowminimize" => Func::WindowMinimize(a.target(0)?),
            "windowmaximize" => Func::WindowMaximize(a.target(0)?),
            "windowhmaximize" => Func::WindowHMaximize(a.target(0)?),
            "windowvmaximize" => Func::WindowVMaximize(a.target(0)?),
            "windowmovevisibly" => Func::WindowMoveVisibly(a.target(0)?),
            "windowclose" => Func::WindowClose(a.target(0)?),
            "windowmove" => Func::WindowMove {
                dx: a.int(0)?,
                dy: a.int(1)?,
                target: a.target(2)?,
            },
            "windowmoveto" => Func::WindowMoveTo {
                gravity: a.gravity(0)?,
                dx: a.int(1)?,
                dy: a.int(2)?,
                target: a.target(3)?,
            },
            "windowclingtoleft" => a.cling(Gravity::West)?,
            "windowclingtoright" => a.cling(Gravity::East)?,
            "windowclingtotop" => a.cling(Gravity::North)?,
            "windowclingtobottom" => a.cling(Gravity::South)?,
            "windowtoggletopmost" => Func::WindowToggleTopMost,
            "windowsetalpha" => Func::WindowSetAlpha(a.int(0)?),
            "windowredraw" => Func::WindowRedraw,
            "windowresizeto" => Func::WindowResizeTo {
                width: a.int(0)?,
                height: a.int(1)?,
                target: a.target(2)?,
            },
            "mousemove" => Func::MouseMove {
                dx: a.int(0)?,
                dy: a.int(1)?,
            },
            _ => unreachable!("上の max の表と同じ名前の集合"),
        };
        Ok(Some(f))
    }

    /// `( 引数 , 引数 … )` を読む。`(` が無ければ空。
    fn load_args(&self, cur: &mut Cur) -> Result<Vec<Token>> {
        let mut args = Vec::new();
        if !cur.peek_sym("(") {
            return Ok(args);
        }
        cur.next();
        if cur.peek_sym(")") {
            cur.next();
            return Ok(args);
        }
        loop {
            match cur.next() {
                Some(Token::Sym(")")) | Some(Token::Sym(",")) => {
                    return self.err("引数が空");
                }
                Some(t) => args.push(t.clone()),
                None => return self.err("`)` が無い"),
            }
            match cur.next() {
                Some(Token::Sym(")")) => return Ok(args),
                Some(Token::Sym(",")) => continue,
                Some(t) => return self.err(format!("`,` か `)` が要る所に `{t}`")),
                None => return self.err("`)` が無い"),
            }
        }
    }

    /// 修飾の接頭辞列を読む。キー名の手前で止まる。yamy の `load_MODIFIER`。
    fn load_modifier(&mut self, cur: &mut Cur) -> Result<Modified> {
        // 既定: 基本修飾は離し、M0-9 は dontcare、U-/D- は無指定、
        //       IC- は「変換中でない」、IL- は dontcare
        let mut mods = ModSpec::none();
        for m in BasicMod::ALL.iter().skip(4) {
            mods = mods.dontcare(*m);
        }
        let mut specified = [false; BasicMod::ALL.len()];
        let mut locks = LockSpec::assign_default();
        let mut locks_specified = [false; LockMod::ALL.len()];
        let mut up: Option<Option<bool>> = None; // None=未指定, Some(None)=dontcare, Some(Some(b))
        let mut down: Option<Option<bool>> = None;
        let mut flag = Flag::Press;

        while let Some(Token::Sym(s)) = cur.peek() {
            if let Some(m) = basic_mod_by_prefix(s) {
                cur.next();
                mods = match flag {
                    Flag::Press => mods.with(m),
                    Flag::Release => mods.without(m),
                    Flag::Dontcare => mods.dontcare(m),
                };
                specified[m.index()] = true;
                flag = Flag::Press;
            } else if let Some(m) = lock_mod_by_prefix(s) {
                cur.next();
                locks = match flag {
                    Flag::Press => locks.with(m),
                    Flag::Release => locks.without(m),
                    Flag::Dontcare => locks.dontcare(m),
                };
                locks_specified[m.index()] = true;
                flag = Flag::Press;
            } else if *s == "U-" || *s == "D-" {
                cur.next();
                let v = match flag {
                    Flag::Press => Some(true),
                    Flag::Release => Some(false),
                    Flag::Dontcare => None,
                };
                if *s == "U-" {
                    up = Some(v);
                } else {
                    down = Some(v);
                }
                flag = Flag::Press;
            } else if *s == "*" {
                cur.next();
                flag = Flag::Dontcare;
            } else if *s == "~" {
                cur.next();
                flag = Flag::Release;
            } else if UNSUPPORTED_MOD_PREFIXES.contains(s) {
                return self.err(format!("修飾 `{s}` は未対応"));
            } else {
                break;
            }
        }

        // キー名の直前の `*` / `~` は未指定の修飾すべてに効く
        if flag != Flag::Press {
            for m in BasicMod::ALL {
                if !specified[m.index()] {
                    mods = match flag {
                        Flag::Release => mods.without(m),
                        _ => mods.dontcare(m),
                    };
                }
            }
            for m in LockMod::ALL {
                if !locks_specified[m.index()] {
                    locks = match flag {
                        Flag::Release => locks.without(m),
                        _ => locks.dontcare(m),
                    };
                }
            }
            let v = if flag == Flag::Release {
                Some(false)
            } else {
                None
            };
            up.get_or_insert(v);
            down.get_or_insert(v);
        }

        // U-/D- を押し・離しの区別に落とす
        let edge = match (up.flatten(), down.flatten()) {
            (None, None) => EdgeSpec::Both,
            (None, Some(d)) => {
                if d {
                    EdgeSpec::Down
                } else {
                    EdgeSpec::Up
                }
            }
            (Some(u), None) => {
                if u {
                    EdgeSpec::Up
                } else {
                    EdgeSpec::Down
                }
            }
            (Some(u), Some(d)) => {
                if u == d {
                    EdgeSpec::Both
                } else if u {
                    EdgeSpec::Up
                } else {
                    EdgeSpec::Down
                }
            }
        };
        Ok(Modified {
            mods,
            edge,
            locks,
            locks_specified: locks_specified.contains(&true),
        })
    }

    // ---- 補助 ------------------------------------------------------------

    fn expect_sym(&self, cur: &mut Cur, s: &str) -> Result<()> {
        match cur.next() {
            Some(Token::Sym(x)) if *x == s => Ok(()),
            Some(t) => self.err(format!("`{s}` が要る所に `{t}`")),
            None => self.err(format!("`{s}` が無い")),
        }
    }

    fn expect_end(&self, cur: &mut Cur) -> Result<()> {
        match cur.peek() {
            None => Ok(()),
            Some(t) => self.err(format!("余分なトークン `{t}`")),
        }
    }

    /// 名前として使えるトークンの文字列。数値もキー名になりうる（yamy は文字列で比較する）。
    fn name_of(&self, t: &Token) -> Result<String> {
        match t {
            Token::Name(s) => Ok(s.clone()),
            Token::Number(_, s) => Ok(s.clone()),
            t => self.err(format!("名前が要る所に `{t}`")),
        }
    }

    fn key_by_name(&self, t: &Token) -> Result<Key> {
        let name = self.name_of(t)?;
        let l = lower(&name);
        match self.names.get(&l).or_else(|| self.aliases.get(&l)) {
            Some(&i) => Ok(self.keys[i]),
            None => self.err(format!("キー `{name}` は未定義")),
        }
    }

    fn key_index_by_non_alias_name(&self, t: &Token) -> Result<usize> {
        let name = self.name_of(t)?;
        match self.names.get(&lower(&name)) {
            Some(&i) => Ok(i),
            None => self.err(format!("キー `{name}` は未定義（別名は使えない）")),
        }
    }

    fn key_by_non_alias_name(&self, t: &Token) -> Result<Key> {
        Ok(self.keys[self.key_index_by_non_alias_name(t)?])
    }

    // ---- 仕上げ ----------------------------------------------------------

    /// `mod` の継承と加減算を解決して `Config` にする。yamy の `Keymaps::adjustModifier`。
    /// 親は必ず先に定義されているので、定義順に処理すれば親は解決済み。
    fn finish(mut self) -> Result<Config> {
        let mut resolved: Vec<Keymap> = Vec::with_capacity(self.keymaps.len());
        for build in self.keymaps.drain(..) {
            let mut km = build.keymap;
            for (i, ops) in build.mod_ops.iter().enumerate() {
                let mut keys: Vec<Key> = match km.parent {
                    Some(p) => resolved[p.0].mod_keys[i].clone(),
                    None => self.def_mods[i].clone(),
                };
                for (op, key) in ops {
                    match op {
                        ModOp::New => {
                            keys.clear();
                            keys.push(*key);
                        }
                        ModOp::Add => keys.push(*key),
                        ModOp::Sub => {
                            if let Some(pos) = keys.iter().position(|k| k == key) {
                                keys.remove(pos);
                            }
                        }
                    }
                }
                let mut dedup = Vec::with_capacity(keys.len());
                for k in keys {
                    if !dedup.contains(&k) {
                        dedup.push(k);
                    }
                }
                km.mod_keys[i] = dedup;
            }
            resolved.push(km);
        }
        let mut config = Config::new(resolved);
        for (i, keys) in self.def_mods.iter().enumerate() {
            config.def_mod(BasicMod::ALL[i], keys.iter().copied());
        }
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Assignment, Edge, LockState, ModState};

    const KEYS: &str = "
        def key A = 0x1e
        def key B = 0x30
        def key H = 0x23
        def key Space = 0x39
        def key BackSpace BS = 0x0e
        def key LShift = 0x2a
        def key RShift = 0x36
        def key LControl = 0x1d
        def key RControl = E0-0x1d
        def key LAlt = 0x38
        def key 英数 Eisuu = 0x3a
        def key E0RShift = E0-0x36
        def mod shift = LShift RShift
        def mod control = LControl RControl
        def mod alt = LAlt
    ";
    const A: Key = Key::plain(0x1e);
    const B: Key = Key::plain(0x30);
    const H: Key = Key::plain(0x23);
    const SPACE: Key = Key::plain(0x39);
    const BS: Key = Key::plain(0x0e);
    const LSHIFT: Key = Key::plain(0x2a);
    const RSHIFT: Key = Key::plain(0x36);
    const LCONTROL: Key = Key::plain(0x1d);
    const RCONTROL: Key = Key::e0(0x1d);
    const LALT: Key = Key::plain(0x38);
    const EISUU: Key = Key::plain(0x3a);
    const E0RSHIFT: Key = Key::e0(0x36);

    /// 修飾を何も書かなかったときの既定: 基本修飾は離し、M0-9 は dontcare。
    fn base() -> ModSpec {
        let mut m = ModSpec::none();
        for b in BasicMod::ALL.iter().skip(4) {
            m = m.dontcare(*b);
        }
        m
    }

    fn load_ok(body: &str) -> Config {
        let src = format!("{KEYS}\n{body}");
        match load_str(&src, "test", Path::new(".")) {
            Ok(c) => c,
            Err(e) => panic!("{e}"),
        }
    }

    fn load_err(body: &str) -> LoadError {
        let src = format!("{KEYS}\n{body}");
        load_str(&src, "test", Path::new(".")).expect_err("エラーになるはず")
    }

    fn global(c: &Config) -> &Keymap {
        &c.keymaps[0]
    }

    fn only(km: &Keymap, key: Key) -> &Assignment {
        let v: Vec<_> = km.assignments.iter().filter(|a| a.key == key).collect();
        assert_eq!(v.len(), 1, "{key:?} の割付が 1 件でない: {v:?}");
        v[0]
    }

    #[test]
    fn def_key_mod_alias_and_simple_assign() {
        let c = load_ok("key A = B\nkey bs = Space");
        assert_eq!(c.def_mods[BasicMod::Shift.index()], vec![LSHIFT, RSHIFT]);
        assert_eq!(
            c.def_mods[BasicMod::Control.index()],
            vec![LCONTROL, RCONTROL]
        );
        let g = global(&c);
        assert_eq!(g.name, "Global");
        assert_eq!(g.mod_keys[BasicMod::Shift.index()], vec![LSHIFT, RSHIFT]);
        let a = only(g, A);
        assert_eq!(a.mods, base());
        assert_eq!(a.edge, EdgeSpec::Both);
        assert_eq!(a.actions, vec![Action::key_with(B, base())]);
        // 別名 BS（大文字小文字は無視）で引ける
        assert_eq!(only(g, BS).actions[0], Action::key_with(SPACE, base()));
    }

    #[test]
    fn modifier_prefixes() {
        let c = load_ok(
            "key C-S-A = B
             key *H = B
             key ~S-*Space = B
             key D-BackSpace = B
             key U-LAlt = S-*B D-LShift U-LShift",
        );
        let g = global(&c);
        assert_eq!(
            only(g, A).mods,
            base().with(BasicMod::Control).with(BasicMod::Shift)
        );
        // `*` がキー名の直前: 全部 dontcare
        assert_eq!(only(g, H).mods, ModSpec::any());
        // `~S-` は離し、残りは dontcare
        assert_eq!(only(g, SPACE).mods, ModSpec::any().without(BasicMod::Shift));
        assert_eq!(only(g, BS).edge, EdgeSpec::Down);
        let alt = only(g, LALT);
        assert_eq!(alt.edge, EdgeSpec::Up);
        assert_eq!(
            alt.actions,
            vec![
                Action::Key {
                    key: B,
                    mods: ModSpec::any().with(BasicMod::Shift),
                    edge: EdgeSpec::Both
                },
                Action::Key {
                    key: LSHIFT,
                    mods: base(),
                    edge: EdgeSpec::Down
                },
                Action::Key {
                    key: LSHIFT,
                    mods: base(),
                    edge: EdgeSpec::Up
                },
            ]
        );
    }

    #[test]
    fn keyseq_expands_and_prefix_switches() {
        let c = load_ok(
            "define KBD109
             def sync = 0x7e
             keyseq $CapsLock = S-英数
             keymap Sub
              key B = A
             keymap Main
              key A = $capslock ( B H ) &Prefix(Sub)
              key C-B = &default",
        );
        let main = &c.keymaps[2];
        assert_eq!(main.name, "Main");
        let sub_id = KeymapId(1);
        assert_eq!(c.keymap(sub_id).name, "Sub");
        assert_eq!(
            only(main, A).actions,
            vec![
                Action::key_with(EISUU, base().with(BasicMod::Shift)),
                Action::key_with(B, base()),
                Action::key_with(H, base()),
                Action::Prefix(sub_id),
            ]
        );
        let cb = only(main, B);
        assert!(cb.mods.is_pressed(BasicMod::Control));
        assert_eq!(cb.actions, vec![Action::Default]);
    }

    #[test]
    fn mod_inheritance_is_resolved() {
        let c = load_ok(
            "mod shift += E0RShift
             keymap Child : Global
              mod control -= LControl
              mod alt = 英数
             keymap Root
              mod shift = 英数",
        );
        let g = &c.keymaps[0];
        assert_eq!(
            g.mod_keys[BasicMod::Shift.index()],
            vec![LSHIFT, RSHIFT, E0RSHIFT]
        );
        let child = &c.keymaps[1];
        assert_eq!(child.parent, Some(KeymapId(0)));
        assert_eq!(
            child.mod_keys[BasicMod::Shift.index()],
            vec![LSHIFT, RSHIFT, E0RSHIFT]
        );
        assert_eq!(child.mod_keys[BasicMod::Control.index()], vec![RCONTROL]);
        assert_eq!(child.mod_keys[BasicMod::Alt.index()], vec![EISUU]);
        // 親無しのキーマップは def mod から始まる
        let root = &c.keymaps[2];
        assert_eq!(root.parent, None);
        assert_eq!(root.mod_keys[BasicMod::Shift.index()], vec![EISUU]);
        assert_eq!(
            root.mod_keys[BasicMod::Control.index()],
            vec![LCONTROL, RCONTROL]
        );
    }

    #[test]
    fn reopening_a_keymap_returns_to_it() {
        let c = load_ok(
            "keymap Sub
              key A = B
             keymap Global
              key H = B
             keymap Sub
              key H = A",
        );
        assert_eq!(c.keymaps.len(), 2);
        assert_eq!(c.keymaps[0].assignments.len(), 1);
        assert_eq!(c.keymaps[1].assignments.len(), 2);
        assert!(
            load_err(
                "keymap Sub
keymap Sub : Global"
            )
            .message
            .contains("親は変えられない")
        );
    }

    #[test]
    fn if_else_endif() {
        let c = load_ok(
            "define KBD109
             if ( KBD109 )
               key A = B
               if ( !KBD109 )
                 key H = B
               else
                 key H = A
               endif
             else
               key Space = B
             endif
             if ( KBD104 ) key BackSpace = B
             if ( KBD109 ) and ( ! KBD104 )
               key LShift = B
             endif
             if ( KBD104 ) and ( KBD109 )
               key RShift = B
               define KBD104
             elseif ( KBD109 )
               key LControl = B
             endif
             if ( KBD104 )
               key RControl = B
             endif
             key LAlt = B",
        );
        let g = global(&c);
        let keys: Vec<Key> = g.assignments.iter().map(|a| a.key).collect();
        assert!(keys.contains(&A));
        assert_eq!(only(g, H).actions[0], Action::key_with(A, base()));
        assert!(!keys.contains(&SPACE), "else 側は読まない");
        assert!(!keys.contains(&BS), "1 行 if は条件が偽なら読まない");
        assert!(keys.contains(&LSHIFT), "and で両方真");
        assert!(!keys.contains(&RSHIFT), "and で片方偽");
        assert!(keys.contains(&LCONTROL), "elseif");
        assert!(
            !keys.contains(&RCONTROL),
            "読まない区間の define は効かない"
        );
        assert!(keys.contains(&LALT), "endif の後は読む");
        assert!(load_err("endif").message.contains("対応する if"));
        assert!(load_err("else").message.contains("対応する if"));
        assert!(load_err("if KBD109").message.contains("`(`"));
    }

    #[test]
    fn ime_modifiers() {
        let c = load_ok(
            "key A = B
             key IC-H = B
             key I-Space = B
             key IL-BackSpace = B
             key *IC-C-LShift = B
             key *RShift = B
             key ~IL-LControl = B
             key ~*RControl = B",
        );
        let g = global(&c);
        let d = LockSpec::assign_default();
        // 既定: 変換中でないことを要求、IME オンは問わない
        assert!(!d.is_dontcare(LockMod::ImeComp) && !d.is_on(LockMod::ImeComp));
        assert!(d.is_dontcare(LockMod::ImeLock));
        assert_eq!(only(g, A).locks, d);
        assert_eq!(only(g, H).locks, d.with(LockMod::ImeComp));
        assert_eq!(only(g, SPACE).locks, d.with(LockMod::ImeComp));
        assert_eq!(only(g, BS).locks, d.with(LockMod::ImeLock));
        // `*IC-` は変換中かを問わない。基本修飾は普通に読む
        let ls = only(g, LSHIFT);
        assert_eq!(ls.locks, LockSpec::any());
        assert_eq!(ls.mods, base().with(BasicMod::Control));
        // キー名の直前の `*` は鍵の条件も全部 dontcare
        assert_eq!(only(g, RSHIFT).locks, LockSpec::any());
        assert_eq!(only(g, LCONTROL).locks, d.without(LockMod::ImeLock));
        // `~*` はキー名の直前の `*` と同じで、全部 dontcare
        assert_eq!(only(g, RCONTROL).locks, LockSpec::any());
    }

    #[test]
    fn window_and_keymap2() {
        let c = load_ok(
            "window Edit /:Edit$/ : Global
              key A = H
             window Both ( /notepad/ && /メモ帳/ ) : Edit
             window Either ( /qq/ || /zz/ )
             keymap2 Sub : Global = &KeymapParent
             keymap KeymapDefault = &Default
             window Plain : Global
             keymap Global",
        );
        let by_name = |n: &str| c.keymaps.iter().position(|k| k.name == n).unwrap();
        let g = global(&c);
        assert_eq!(g.default_actions, vec![Action::OtherWindowClass]);
        assert!(g.window.as_ref().unwrap().matches("", ""));

        let edit = &c.keymaps[by_name("Edit")];
        let w = edit.window.as_ref().unwrap();
        assert!(w.matches("notepad.exe:Notepad:Edit", ""));
        assert!(w.matches("x:EDIT", "大文字小文字は区別しない"));
        assert!(!w.matches("x:Editor", ""));
        assert_eq!(edit.default_actions, vec![Action::KeymapParent]);
        assert_eq!(only(edit, A).actions, vec![Action::key_with(H, base())]);

        let both = c.keymaps[by_name("Both")].window.as_ref().unwrap();
        assert!(both.matches("notepad.exe:Edit", "無題 - メモ帳"));
        assert!(!both.matches("notepad.exe:Edit", ""));
        let either = c.keymaps[by_name("Either")].window.as_ref().unwrap();
        assert!(either.matches("qq", ""));
        assert!(either.matches("", "zz"));
        assert!(!either.matches("", ""));

        assert_eq!(
            c.keymaps[by_name("Sub")].default_actions,
            vec![Action::KeymapParent]
        );
        assert_eq!(
            c.keymaps[by_name("KeymapDefault")].default_actions,
            vec![Action::Default]
        );
        let plain = &c.keymaps[by_name("Plain")];
        assert!(plain.window.is_none(), "条件の無い window は普通の keymap");
        assert_eq!(plain.parent, Some(KeymapId(0)));

        // 後に定義した window ほど先。Global は最後
        assert_eq!(
            c.focus_keymaps("notepad.exe:Notepad:Edit", "無題 - メモ帳"),
            vec![
                KeymapId(by_name("Both")),
                KeymapId(by_name("Edit")),
                KeymapId(0)
            ]
        );
        assert_eq!(c.focus_keymaps("chrome.exe:Chrome", ""), vec![KeymapId(0)]);

        assert!(
            load_err("window Bad /(/ : Global")
                .message
                .contains("正規表現")
        );
        assert!(load_err("window Bad ( /a/ /b/ )").message.contains("`&&`"));
        assert!(
            load_err("keymap K = &Default\nkeymap K = &KeymapParent")
                .message
                .contains("既定動作")
        );
        assert!(
            load_err("window W /a/\nwindow W /b/")
                .message
                .contains("窓の条件")
        );
    }

    #[test]
    fn window_functions_and_arguments() {
        use crate::model::Target;
        let c = load_ok(
            "key A = &WindowMove(-16, 0)
             key B = &WindowMove(1, 2, MDI)
             key H = &WindowLower &WindowRaise(mdi) &WindowMaximize()
             key Space = &WindowClingToLeft &WindowClingToBottom(MDI)
             key BackSpace = &WindowSetAlpha(70) &WindowToggleTopMost &WindowRedraw
             key LShift = &MouseMove(0, -1) &WindowResizeTo(0, -100)
             key RShift = &WindowMoveTo(NE, 3, 4)",
        );
        let g = global(&c);
        let o = Target::Overlapped;
        assert_eq!(
            only(g, A).actions,
            vec![Action::Func(Func::WindowMove {
                dx: -16,
                dy: 0,
                target: o
            })]
        );
        assert_eq!(
            only(g, B).actions,
            vec![Action::Func(Func::WindowMove {
                dx: 1,
                dy: 2,
                target: Target::Mdi
            })]
        );
        assert_eq!(
            only(g, H).actions,
            vec![
                Action::Func(Func::WindowLower(o)),
                Action::Func(Func::WindowRaise(Target::Mdi)),
                Action::Func(Func::WindowMaximize(o)),
            ]
        );
        assert_eq!(
            only(g, SPACE).actions,
            vec![
                Action::Func(Func::WindowMoveTo {
                    gravity: Gravity::West,
                    dx: 0,
                    dy: 0,
                    target: o
                }),
                Action::Func(Func::WindowMoveTo {
                    gravity: Gravity::South,
                    dx: 0,
                    dy: 0,
                    target: Target::Mdi
                }),
            ]
        );
        assert_eq!(
            only(g, BS).actions,
            vec![
                Action::Func(Func::WindowSetAlpha(70)),
                Action::Func(Func::WindowToggleTopMost),
                Action::Func(Func::WindowRedraw),
            ]
        );
        assert_eq!(
            only(g, LSHIFT).actions,
            vec![
                Action::Func(Func::MouseMove { dx: 0, dy: -1 }),
                Action::Func(Func::WindowResizeTo {
                    width: 0,
                    height: -100,
                    target: o
                }),
            ]
        );
        assert_eq!(
            only(g, RSHIFT).actions,
            vec![Action::Func(Func::WindowMoveTo {
                gravity: Gravity::NorthEast,
                dx: 3,
                dy: 4,
                target: o
            })]
        );
        assert!(load_err("key A = &WindowMove").message.contains("引数"));
        assert!(load_err("key A = &WindowMove(1)").message.contains("引数"));
        assert!(
            load_err("key A = &WindowMove(1, 2, MDI, 4)")
                .message
                .contains("引数が多い")
        );
        assert!(load_err("key A = &WindowMove(a, 2)").message.contains("数"));
        assert!(
            load_err("key A = &WindowLower(Foo)")
                .message
                .contains("overlapped")
        );
        assert!(load_err("key A = &MouseMove(1, 2").message.contains("`)`"));
        assert!(load_err("key A = &MouseMove(1,, 2)").message.contains("空"));
    }

    #[test]
    fn later_assignment_wins() {
        let c = load_ok("key A = B\nkey A = H");
        let g = global(&c);
        let found = g
            .search(A, ModState::empty(), LockState::empty(), Edge::Down)
            .unwrap();
        assert_eq!(found.actions[0], Action::key_with(H, base()));
        assert_eq!(g.assignments.len(), 1);
    }

    #[test]
    fn include_and_encoding() {
        let dir = std::env::temp_dir().join(format!("newmayu-parser-{}", std::process::id()));
        let sub = dir.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(
            dir.join("top.mayu"),
            format!("{KEYS}\ninclude \"sub/inner.mayu\"\nkey H = B\n"),
        )
        .unwrap();
        std::fs::write(sub.join("inner.mayu"), "key A = B\n").unwrap();
        let c = load(&dir.join("top.mayu")).unwrap();
        let g = global(&c);
        assert_eq!(g.assignments.len(), 2);
        assert_eq!(only(g, A).actions.len(), 1);

        // include 先のエラーは include 先のファイル名と行で出る
        std::fs::write(sub.join("inner.mayu"), "key A = B\n\nkey Z = B\n").unwrap();
        let e = load(&dir.join("top.mayu")).unwrap_err();
        assert!(e.file.ends_with("inner.mayu"), "{e}");
        assert_eq!(e.line, 3);

        // BOM 付きはエラー
        std::fs::write(dir.join("bom.mayu"), b"\xEF\xBB\xBFkey A = B\n").unwrap();
        let e = load(&dir.join("bom.mayu")).unwrap_err();
        assert!(e.message.contains("BOM"), "{e}");
        // CP932 はエラー（行番号付き）
        std::fs::write(dir.join("sjis.mayu"), b"# ok\n# \x93\xfa\x96\x7b\x8c\xea\n").unwrap();
        let e = load(&dir.join("sjis.mayu")).unwrap_err();
        assert_eq!(e.line, 2);
        assert!(e.message.contains("UTF-8"), "{e}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn errors() {
        let e = load_err("def key Pause = E1-0x1d 0x45");
        assert!(e.message.contains("2 個以上"), "{e}");
        assert_eq!(e.line, KEYS.lines().count() + 1);
        assert!(load_err("key Z = A").message.contains("未定義"));
        assert!(
            load_err("key A = &WindowMonitor(1)")
                .message
                .contains("未対応")
        );
        assert!(load_err("key A = IL-B").message.contains("出力キー"));
        assert!(load_err("key A = IC-B").message.contains("出力キー"));
        assert!(load_err("key NL-A = B").message.contains("未対応"));
        assert!(load_err("mod !shift = A").message.contains("未対応"));
        assert!(load_err("key A = $Nothing").message.contains("未定義"));
        assert!(load_err("keymap X : Nowhere").message.contains("未定義"));
        assert!(load_err("def key A = 0x01").message.contains("定義済み"));
        assert!(load_err("event prefixed = A").message.contains("未対応"));
        assert!(load_err("key C- = B").message.contains("未対応"));
    }
}

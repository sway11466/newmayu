//! `.mayu` の構文解析と `model::Config` への変換。yamy の `SettingLoader` を写している。
//!
//! 読める文（最小サブセット）:
//!
//! ```text
//! include "file"
//! define SYMBOL
//! def key NAME... = [E0-|E1-]SCANCODE          # NAME を ( ) で囲む形も可。スキャンコードは 1 個
//! def mod (shift|alt|control|windows) = KEY...
//! def alias NAME = KEY
//! def sync = SCANCODE                          # 読むだけ（&Sync は未対応）
//! keymap NAME [: PARENT]                      # 同名の再宣言はそのキーマップに戻る
//! key MODKEY... = ACTION...                    # ACTION = 修飾付きキー名 | $keyseq名 | ( ACTION... ) | &Prefix(KEYMAP)
//! mod MODNAME (=|+=|-=) KEY...
//! keyseq $NAME = ACTION...                     # 使う側で展開する
//! ```
//!
//! 修飾の書き方は yamy の `load_MODIFIER` と同じ: `*` は直後の修飾（キー名の直前なら未指定の全部）を
//! dontcare、`~` は離し。未指定の基本修飾（S- A- C- W-）は離し、M0-〜M9- は dontcare。
//! `U-` `D-` は押し・離しの区別（両方または無指定なら両方）。
//!
//! これ以外（`if`/`else`/`endif`、`window`、`keymap2`、`event`、`def option`、`def subst`、
//! `&Prefix` 以外の `&関数`、`!`/`!!`/`!!!`、IL- IC- NL- 等の割当修飾、2 個以上のスキャンコード）は
//! エラーにする。
//!
//! ファイルは BOM 無し UTF-8 のみ。大文字小文字は区別しない。

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use super::lexer::{Line, Token, tokenize};
use crate::model::{
    Action, BasicMod, Config, EdgeSpec, Key, Keymap, KeymapId, ModSpec, ScanPrefix,
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
    "R-", "IL-", "IC-", "I-", "NL-", "CL-", "SL-", "KL-", "MAX-", "MIN-", "MMAX-", "MMIN-", "T-",
    "TS-", "L0-", "L1-", "L2-", "L3-", "L4-", "L5-", "L6-", "L7-", "L8-", "L9-",
];

/// 修飾の解析結果。
struct Modified {
    mods: ModSpec,
    edge: EdgeSpec,
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
            keymap: Keymap::new("Global", None),
            mod_ops: Default::default(),
        };
        let mut keymap_names = HashMap::new();
        keymap_names.insert("global".to_string(), 0);
        Loader {
            keys: Vec::new(),
            names: HashMap::new(),
            aliases: HashMap::new(),
            def_mods: Default::default(),
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
        let Some(Token::Name(head)) = cur.next() else {
            return self.err(format!("文の先頭が名前でない: {}", line.tokens[0]));
        };
        match lower(head).as_str() {
            "include" => self.load_include(&mut cur, dir, depth),
            "define" => self.load_define(&mut cur),
            "def" => self.load_def(&mut cur),
            "keymap" => self.load_keymap(&mut cur),
            "key" => self.load_key(&mut cur),
            "mod" => self.load_mod(&mut cur),
            "keyseq" => self.load_keyseq(&mut cur),
            "if" | "and" | "else" | "elseif" | "elsif" | "elif" | "or" | "endif" => {
                self.err(format!("`{head}` は未対応"))
            }
            "window" | "keymap2" | "event" => self.err(format!("`{head}` は未対応")),
            _ => self.err(format!("知らない文 `{head}`")),
        }
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

    /// `define SYMBOL`。シンボルは `if` でだけ使い、`if` はサブセット外なので読むだけ。
    fn load_define(&mut self, cur: &mut Cur) -> Result<()> {
        let Some(Token::Name(_)) = cur.next() else {
            return self.err("define の後はシンボル名");
        };
        self.expect_end(cur)
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

    /// `keymap NAME [: PARENT]`
    fn load_keymap(&mut self, cur: &mut Cur) -> Result<()> {
        let Some(t) = cur.next() else {
            return self.err("keymap の後は名前");
        };
        let name = self.name_of(t)?;
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
        if !cur.at_end() {
            return self.err("keymap の既定動作（`keymap NAME = ...`）は未対応");
        }
        let l = lower(&name);
        // 同名の再宣言はそのキーマップに戻る（yamy の `Keymaps::add` は既存を返す）。
        // yamy は 2 回目の親指定を黙って捨てるが、ここでは食い違えばエラーにする。
        if let Some(&idx) = self.keymap_names.get(&l) {
            if parent.is_some() && parent != self.keymaps[idx].keymap.parent {
                return self.err(format!(
                    "キーマップ `{name}` は定義済みで、親は変えられない"
                ));
            }
            self.current = idx;
            return Ok(());
        }
        let idx = self.keymaps.len();
        self.keymaps.push(KeymapBuild {
            keymap: Keymap::new(name, parent),
            mod_ops: Default::default(),
        });
        self.keymap_names.insert(l, idx);
        self.current = idx;
        Ok(())
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
            km.assign(key, m.mods, m.edge, actions.clone());
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

    /// 修飾の接頭辞列を読む。キー名の手前で止まる。yamy の `load_MODIFIER`。
    fn load_modifier(&mut self, cur: &mut Cur) -> Result<Modified> {
        // 既定: 基本修飾は離し、M0-9 は dontcare、U-/D- は無指定
        let mut mods = ModSpec::none();
        for m in BasicMod::ALL.iter().skip(4) {
            mods = mods.dontcare(*m);
        }
        let mut specified = [false; BasicMod::ALL.len()];
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
        Ok(Modified { mods, edge })
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
        let mut config = Config::new(resolved, KeymapId(0));
        for (i, keys) in self.def_mods.iter().enumerate() {
            config.def_mod(BasicMod::ALL[i], keys.iter().copied());
        }
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Assignment, Edge, ModState};

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
        let g = c.keymap(c.root);
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
        let g = c.keymap(c.root);
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
              key A = $capslock ( B H ) &Prefix(Sub)",
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
    fn later_assignment_wins() {
        let c = load_ok("key A = B\nkey A = H");
        let g = c.keymap(c.root);
        let found = g.search(A, ModState::empty(), Edge::Down).unwrap();
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
        let g = c.keymap(c.root);
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
        assert!(load_err("if ( KBD109 )").message.contains("未対応"));
        assert!(load_err("key A = &Default").message.contains("未対応"));
        assert!(load_err("key IL-A = B").message.contains("未対応"));
        assert!(load_err("mod !shift = A").message.contains("未対応"));
        assert!(load_err("key A = $Nothing").message.contains("未定義"));
        assert!(load_err("keymap X : Nowhere").message.contains("未定義"));
        assert!(load_err("def key A = 0x01").message.contains("定義済み"));
        assert!(
            load_err("window Foo /x/ : Global")
                .message
                .contains("未対応")
        );
        assert!(load_err("key C- = B").message.contains("未対応"));
    }
}

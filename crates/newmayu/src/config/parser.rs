//! `.mayu` の構文解析と `model::Config` への変換。yamy の `SettingLoader` を写している。
//!
//! 読める文（最小サブセット）:
//!
//! ```text
//! include "file"
//! define SYMBOL
//! if ( [!] SYMBOL ) [文] / and ( [!] SYMBOL ) / else [文] / elseif ( [!] SYMBOL ) / endif
//! def key NAME... = [E0-|E1-]SCANCODE...       # NAME を ( ) で囲む形も可。スキャンコードは 4 個まで
//! def mod (shift|alt|control|windows) = KEY...
//! def alias NAME = KEY
//! def sync = SCANCODE                          # &Sync で出す同期キー
//! def subst MODKEY... (=|=>) ACTION...         # キーマップで引く前に入力キーを置き換える。右辺は先頭のキーだけ使う
//! keymap NAME [: PARENT] [= ACTION...]        # 同名の再宣言はそのキーマップに戻る。= は割付の無いキーの既定動作
//! keymap2 NAME [: PARENT] [= ACTION...]       # keymap と同じ（yamy でも同じ）
//! window NAME [WINDOW] [: PARENT] [= ACTION...]
//!                                              # WINDOW = /クラス/ | ( /クラス/ && /タイトル/ ) | ( /クラス/ || /タイトル/ )
//! event (prefixed|before-key-down|after-key-up) = ACTION...
//!                                              # いまのキーマップの event。prefixed は遷移先のキーマップのものが動く
//! key MODKEY... = ACTION...                    # ACTION = 修飾付きキー名 | $keyseq名 | ( ACTION... ) | &Prefix(KEYMAP) | &Keymap(KEYMAP)
//!                                              #        | &Default | &KeymapParent | &OtherWindowClass | &KeymapWindow
//!                                              #        | &Window…([引数]) | &MouseMove(dx, dy)   （model::Func 参照）
//!                                              #        | &Ignore | &Undefined | &VK([E-][D-|U-]名前|番号)
//!                                              #        | &PostMessage(宛先, msg, wParam, lParam)
//!                                              #        | &Variable(mag, inc) | &Repeat(( ACTION... ) | $keyseq名 [, 上限])
//!                                              #        | &Sync | &ClipboardUpcaseWord | &ClipboardDowncaseWord
//!                                              #        | &HelpMessage(["タイトル" [, "本文"]]) | &HelpVariable("タイトル")
//!                                              #        | &ShellExecute(操作, ファイル, パラメータ, フォルダ, 窓の出し方)
//!                                              #        | &MayuDialog(Log, 窓の出し方) | &LogClear
//!                                              #        | &WindowIdentify | &InvestigateCommand | &LoadSetting(["名前"])
//!                                              # MODKEY の修飾には IL-（IME オン）IC-/I-（変換中）
//!                                              # NL- CL- SL- KL-（NumLock CapsLock ScrollLock カナ）も書ける
//! mod MODNAME (=|+=|-=) [!!]KEY...                # !! はワンショット（単独で押して離したらそのキー）
//! keyseq $NAME = ACTION...                     # 使う側で展開する。キーに IL- IC- なども書ける（def subst の右辺で効く）
//! ```
//!
//! 修飾の書き方は yamy の `load_MODIFIER` と同じ: `*` は直後の修飾（キー名の直前なら未指定の全部）を
//! dontcare、`~` は離し。未指定の基本修飾（S- A- C- W-）は離し、M0-〜M9- は dontcare。
//! `U-` `D-` は押し・離しの区別（両方または無指定なら両方）。
//! 未指定の `IC-` は「変換中でない」を要求し（yamy の既定）、`IL-` は dontcare。
//!
//! これ以外（`def option`、上記以外の `&関数`、`!`/`!!!`、`mod !!MODNAME`、R- MAX- 等の割当修飾）は
//! エラーにする。
//!
//! ファイルの文字コードは UTF-8（BOM の有無は問わない）か CP932。UTF-8 として正しければ UTF-8、
//! そうでなければ CP932 として読み、CP932 としても読めなければエラーにする。大文字小文字は区別しない。

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};

use super::lexer::{Line, Token, tokenize};
use super::vkeys::vk_by_name;
use crate::describe;
use crate::model::{
    Action, BasicMod, Config, EdgeSpec, EventKind, Func, Gravity, HelpText, Key, Keymap, KeymapId,
    LockMod, LockSpec, ModSpec, ScanCode, ScanPrefix, ShellExecute, ShowCommand, Subst, Target,
    ToWindow, WindowMatch, WindowOp,
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
/// `symbols` は `define` 済みとして扱うシンボル（yamy の設定の `-D` と起動引数の `-D`）。
pub fn load(path: &Path, symbols: &[String]) -> Result<Config> {
    let mut loader = Loader::new();
    loader.symbols.extend(symbols.iter().map(|s| lower(s)));
    loader.load_file(path, 0)?;
    loader.finish()
}

/// 文字列から読み込む（テスト用）。`include` は `dir` からの相対パス。
pub fn load_str(src: &str, name: &str, dir: &Path) -> Result<Config> {
    let mut loader = Loader::new();
    loader.load_source(src, name, dir, 0)?;
    loader.finish()
}

/// ファイルのバイト列を文字列にする。UTF-8 として正しければ UTF-8（BOM は剥がす）、
/// そうでなければ CP932 として復号する。どちらでもなければ、UTF-8 として読めなくなった行番号を返す。
fn decode(bytes: &[u8]) -> std::result::Result<Cow<'_, str>, usize> {
    let body = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    let utf8_err = match std::str::from_utf8(body) {
        Ok(s) => return Ok(Cow::Borrowed(s)),
        Err(e) => e,
    };
    let (s, had_errors) = encoding_rs::SHIFT_JIS.decode_without_bom_handling(body);
    if !had_errors {
        return Ok(s);
    }
    Err(body[..utf8_err.valid_up_to()]
        .iter()
        .filter(|b| **b == b'\n')
        .count()
        + 1)
}

/// `mod NAME (=|+=|-=) KEY` の 1 項。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ModOp {
    New,
    Add,
    Sub,
}

/// 構築中のキーマップ。`mod` 文は親の確定を待つので、ここでは演算のまま持つ。
/// 各項は（演算, キー, `!!` 付きか）。
struct KeymapBuild {
    keymap: Keymap,
    mod_ops: [Vec<(ModOp, Key, bool)>; BasicMod::ALL.len()],
}

struct Loader {
    /// `def key` で定義したキー。
    keys: Vec<Key>,
    /// `keys` と同じ並びの、`def key` の最初の名前（`&DescribeBindings` 用）。
    key_names: Vec<String>,
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
    /// `keyseq $名`（小文字化）→ 書かれた名前（`&DescribeBindings` 用）。
    keyseq_names: HashMap<String, String>,
    /// `def sync`。
    sync: Option<Key>,
    /// `def subst`。照合する順（後に書いたものが先）。
    substs: Vec<Subst>,
    keymaps: Vec<KeymapBuild>,
    /// キーマップ名（小文字化）→ `keymaps` の添字。
    keymap_names: HashMap<String, usize>,
    current: usize,
    /// エラー表示用。読んでいるファイルと行。
    file: String,
    line: usize,
}

/// `&WindowClingToLeft` 等の yamy の名前。
fn cling_name(fname: &str) -> Option<&'static str> {
    match lower(fname).as_str() {
        "windowclingtoleft" => Some("WindowClingToLeft"),
        "windowclingtoright" => Some("WindowClingToRight"),
        "windowclingtotop" => Some("WindowClingToTop"),
        "windowclingtobottom" => Some("WindowClingToBottom"),
        _ => None,
    }
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
    "R-", "MAX-", "MIN-", "MMAX-", "MMIN-", "T-", "TS-", "L0-", "L1-", "L2-", "L3-", "L4-", "L5-",
    "L6-", "L7-", "L8-", "L9-",
];

fn lock_mod_by_prefix(s: &str) -> Option<LockMod> {
    match s {
        "IL-" => Some(LockMod::ImeLock),
        "IC-" | "I-" => Some(LockMod::ImeComp),
        "NL-" => Some(LockMod::NumLock),
        "CL-" => Some(LockMod::CapsLock),
        "SL-" => Some(LockMod::ScrollLock),
        "KL-" => Some(LockMod::KanaLock),
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
        let v = self.i64(i)?;
        match i32::try_from(v) {
            Ok(v) => Ok(v),
            Err(_) => self
                .loader
                .err(format!("`&{}`: 数が大きすぎる", self.fname)),
        }
    }

    /// 空の引数は 0（yamy の `Token::getNumber`）。
    fn i64(&self, i: usize) -> Result<i64> {
        match self.args.get(i) {
            Some(Token::Number(v, _)) => Ok(*v),
            Some(Token::Name(s)) if s.is_empty() => Ok(0),
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

    fn to_window(&self, i: usize) -> Result<ToWindow> {
        let Some(t) = self.args.get(i) else {
            return self.loader.err(format!(
                "`&{}`: {} 番目の引数（宛先）が無い",
                self.fname,
                i + 1
            ));
        };
        let name = self.loader.name_of(t)?;
        Ok(match lower(&name).as_str() {
            "toitself" => ToWindow::Itself,
            "toparentwindow" => ToWindow::Parent,
            "tomainwindow" => ToWindow::Main,
            "tooverlappedwindow" => ToWindow::Overlapped,
            _ => {
                return self.loader.err(format!(
                    "`&{}`: 宛先は ToItself / ToParentWindow / ToMainWindow / ToOverlappedWindow: `{name}`",
                    self.fname
                ));
            }
        })
    }

    /// 文字列 `"…"`。引用符の無い名前もそのまま文字列にする（default.mayu の
    /// `&HelpMessage(Mayu, "…")`）。省略時は空。
    fn string(&self, i: usize) -> Result<String> {
        match self.args.get(i) {
            None => Ok(String::new()),
            Some(Token::Str(s)) | Some(Token::Name(s)) => Ok(s.clone()),
            Some(t) => self.loader.err(format!(
                "`&{}`: {} 番目の引数は文字列: `{t}`",
                self.fname,
                i + 1
            )),
        }
    }

    fn show_command(&self, i: usize) -> Result<ShowCommand> {
        let Some(t) = self.args.get(i) else {
            return self.loader.err(format!(
                "`&{}`: {} 番目の引数（窓の出し方）が無い",
                self.fname,
                i + 1
            ));
        };
        let name = self.loader.name_of(t)?;
        Ok(match lower(&name).as_str() {
            "hide" => ShowCommand::Hide,
            "maximize" => ShowCommand::Maximize,
            "minimize" => ShowCommand::Minimize,
            "restore" => ShowCommand::Restore,
            "show" => ShowCommand::Show,
            "showdefault" => ShowCommand::ShowDefault,
            "showmaximized" => ShowCommand::ShowMaximized,
            "showminimized" => ShowCommand::ShowMinimized,
            "showminnoactive" => ShowCommand::ShowMinNoActive,
            "showna" => ShowCommand::ShowNA,
            "shownoactivate" => ShowCommand::ShowNoActivate,
            "shownormal" => ShowCommand::ShowNormal,
            _ => {
                return self
                    .loader
                    .err(format!("`&{}`: 窓の出し方が不明: `{name}`", self.fname));
            }
        })
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
            key_names: Vec::new(),
            names: HashMap::new(),
            aliases: HashMap::new(),
            def_mods: Default::default(),
            symbols: HashSet::new(),
            can_read: Vec::new(),
            keyseqs: HashMap::new(),
            keyseq_names: HashMap::new(),
            sync: None,
            substs: Vec::new(),
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
        let src = match decode(&bytes) {
            Ok(s) => s,
            Err(line) => {
                self.file = name;
                self.line = line;
                return self.err("文字コードが判定できない（UTF-8 でも CP932 でもない）");
            }
        };
        let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        self.load_source(&src, &name, &dir, depth)
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
            "event" => self.load_event(cur),
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
                let sc = self.load_scan_code(cur)?;
                self.sync = Some(Key::new(sc.prefix, sc.code));
                self.expect_end(cur)
            }
            "subst" => self.load_def_subst(cur),
            "option" => self.err(format!("`def {what}` は未対応")),
            _ => self.err(format!("知らない定義 `def {what}`")),
        }
    }

    /// `def subst MODKEY... (=|=>) ACTION...`（yamy の `load_DEFINE_SUBSTITUTE`）。
    /// 右辺は keyseq として読み、先頭のキーだけを使う（`$名` の中も先頭）。残りは捨てる。
    fn load_def_subst(&mut self, cur: &mut Cur) -> Result<()> {
        let mut lhs = Vec::new();
        loop {
            let m = self.load_modifier(cur)?;
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
        let actions = self.load_actions(cur, false, true)?;
        let Some(&Action::Key {
            key: to_key,
            mods: to_mods,
            locks: to_locks,
            ..
        }) = actions.first()
        else {
            return self.err("`def subst` の右辺の先頭がキーでない");
        };
        for (key, m) in lhs {
            self.substs.insert(
                0,
                Subst {
                    key,
                    mods: m.mods,
                    edge: m.edge,
                    locks: m.locks,
                    to_key,
                    to_mods,
                    to_locks,
                },
            );
        }
        Ok(())
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
        // yamy の load_SCAN_CODES と同じく、行末か 4 個まで読む
        let mut codes = Vec::new();
        while !cur.at_end() && codes.len() < Key::MAX_SCAN_CODES {
            codes.push(self.load_scan_code(cur)?);
        }
        self.expect_end(cur)?;
        let Some(key) = Key::from_codes(&codes) else {
            return self.err("スキャンコードが無い");
        };
        let idx = self.keys.len();
        for n in &names {
            let l = lower(n);
            if self.names.contains_key(&l) || self.aliases.contains_key(&l) {
                return self.err(format!("キー名 `{n}` は定義済み"));
            }
            self.names.insert(l, idx);
        }
        self.keys.push(key);
        self.key_names.push(names[0].clone());
        Ok(())
    }

    /// `[E0-|E1-] NUMBER`
    fn load_scan_code(&mut self, cur: &mut Cur) -> Result<ScanCode> {
        let mut prefix = ScanPrefix::None;
        loop {
            match cur.next() {
                Some(Token::Number(n, text)) => {
                    let Ok(code) = u8::try_from(*n) else {
                        return self.err(format!("スキャンコード {text} が 0..=0xff の外"));
                    };
                    return Ok(ScanCode::new(prefix, code));
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
            default_actions = Some(self.load_actions_text(cur, false, false)?);
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
            if default_actions.is_some_and(|(a, _)| a != km.default_actions) {
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
        if let Some((a, text)) = default_actions {
            keymap.default_actions = a;
            keymap.default_text = text;
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

    /// `event NAME = ACTION...`。いまのキーマップに付く。
    fn load_event(&mut self, cur: &mut Cur) -> Result<()> {
        let Some(t) = cur.next() else {
            return self.err("event の後は prefixed / before-key-down / after-key-up");
        };
        let name = self.name_of(t)?;
        let Some(kind) = EventKind::by_name(&name) else {
            return self.err(format!(
                "知らない event `{name}`（prefixed / before-key-down / after-key-up）"
            ));
        };
        self.expect_sym(cur, "=")?;
        let (actions, text) = self.load_actions_text(cur, false, false)?;
        self.keymaps[self.current]
            .keymap
            .set_event_text(kind, actions, text);
        Ok(())
    }

    /// `&Repeat(( ACTION... ) | $keyseq名 [, 上限])`。上限の既定は 10（yamy と同じ）。
    /// 戻り値の 2 つ目は `&DescribeBindings` で出す形。
    fn load_repeat(&mut self, cur: &mut Cur) -> Result<(Action, String)> {
        self.expect_sym(cur, "(")?;
        let (actions, seq) = self.load_key_seq_arg(cur, "&Repeat の 1 番目の引数")?;
        let max = if cur.peek_sym(",") {
            cur.next();
            match cur.next() {
                Some(Token::Number(v, _)) => match i32::try_from(*v) {
                    Ok(v) => v,
                    Err(_) => return self.err("&Repeat: 上限が大きすぎる"),
                },
                _ => return self.err("&Repeat の 2 番目の引数は数（上限）"),
            }
        } else {
            10
        };
        self.expect_sym(cur, ")")?;
        Ok((
            Action::Repeat { actions, max },
            describe::repeat_text(&seq, max),
        ))
    }

    /// `&EmacsEditKillLinePred(( ACTION... ) | $keyseq名, ( ACTION... ) | $keyseq名)`。
    /// 戻り値の 2 つ目は `&DescribeBindings` で出す形。
    fn load_kill_line_pred(&mut self, cur: &mut Cur) -> Result<(Action, String)> {
        self.expect_sym(cur, "(")?;
        let (seq1, t1) = self.load_key_seq_arg(cur, "&EmacsEditKillLinePred の 1 番目の引数")?;
        self.expect_sym(cur, ",")?;
        let (seq2, t2) = self.load_key_seq_arg(cur, "&EmacsEditKillLinePred の 2 番目の引数")?;
        self.expect_sym(cur, ")")?;
        Ok((
            Action::EmacsEditKillLinePred { seq1, seq2 },
            describe::kill_line_pred_text(&t1, &t2),
        ))
    }

    /// 動作列の引数 `( ACTION... )` か `$keyseq名`（yamy の `load_ARGUMENT(const KeySeq **)`）。
    /// 戻り値の 2 つ目は `&DescribeBindings` で出す形（`$名`。`( … )` は yamy の無名の keyseq で `$`）。
    fn load_key_seq_arg(&mut self, cur: &mut Cur, what: &str) -> Result<(Vec<Action>, String)> {
        match cur.next() {
            Some(Token::Sym("(")) => Ok((self.load_actions(cur, true, false)?, "$".to_string())),
            Some(Token::Sym("$")) => {
                let Some(t) = cur.next() else {
                    return self.err("`$` の後は keyseq 名");
                };
                let name = self.name_of(t)?;
                match self.keyseqs.get(&lower(&name)) {
                    Some(seq) => Ok((seq.clone(), format!("${}", self.keyseq_name(&name)))),
                    None => self.err(format!("keyseq `${name}` は未定義")),
                }
            }
            _ => self.err(format!("{what}は `( 動作… )` か `$keyseq名`")),
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
        let (actions, text) = self.load_actions_text(cur, false, false)?;
        let km = &mut self.keymaps[self.current].keymap;
        for (key, m) in lhs {
            km.assign_text(key, m.mods, m.edge, m.locks, actions.clone(), text.clone());
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
        while let Some(mut t) = cur.next() {
            // `!!` はワンショット（yamy の AM_oneShot）。`!` `!!!` はまだ読まない
            let mut one_shot = false;
            match t {
                Token::Sym("!!") => {
                    one_shot = true;
                    let Some(next) = cur.next() else {
                        return self.err("`!!` の後はキー名");
                    };
                    t = next;
                }
                Token::Sym(s @ ("!" | "!!!")) => {
                    return self.err(format!("`{s}`（修飾キーの単独押し動作）は未対応"));
                }
                _ => {}
            }
            let key = self.key_by_name(t)?;
            let ops = &mut self.keymaps[self.current].mod_ops[m.index()];
            match op {
                ModOp::New => {
                    ops.clear();
                    ops.push((ModOp::New, key, one_shot));
                    op = ModOp::Add;
                }
                _ => {
                    if let Some(e) = ops.iter_mut().find(|(_, k, _)| *k == key) {
                        e.0 = op;
                        e.2 = one_shot;
                    } else {
                        ops.push((op, key, one_shot));
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
        let actions = self.load_actions(cur, false, true)?;
        self.keyseq_names.insert(lower(&name), name.clone());
        self.keyseqs.insert(lower(&name), actions);
        Ok(())
    }

    // ---- 動作列と修飾 ----------------------------------------------------

    /// `ACTION...`。`in_paren` なら `)` で止まる。
    /// `locks_ok` なら出力キーに IL- IC- などを書ける（`keyseq` と `def subst` の右辺。yamy と同じ）。
    /// そうでなければエラーにし、鍵の条件は全部 dontcare にする。
    fn load_actions(
        &mut self,
        cur: &mut Cur,
        in_paren: bool,
        locks_ok: bool,
    ) -> Result<Vec<Action>> {
        let mut out = Vec::new();
        self.load_actions_into(cur, in_paren, locks_ok, &mut out, &mut Vec::new())?;
        Ok(out)
    }

    /// [`Loader::load_actions`] と、`&DescribeBindings` で出す右辺（yamy の形）。
    fn load_actions_text(
        &mut self,
        cur: &mut Cur,
        in_paren: bool,
        locks_ok: bool,
    ) -> Result<(Vec<Action>, String)> {
        let mut out = Vec::new();
        let mut spans = Vec::new();
        self.load_actions_into(cur, in_paren, locks_ok, &mut out, &mut spans)?;
        // 動作ごとに書き、`spans` の範囲（`$名` など）はその文字列にする
        let mut items = Vec::new();
        let mut i = 0;
        for (start, end, text) in spans {
            while i < start {
                items.push(self.action_text(&out[i]));
                i += 1;
            }
            items.push(text);
            i = end;
        }
        while i < out.len() {
            items.push(self.action_text(&out[i]));
            i += 1;
        }
        Ok((out, describe::seq_text(&items)))
    }

    /// 1 つの動作の yamy の形。
    fn action_text(&self, a: &Action) -> String {
        let key_name = |k: Key| {
            self.keys
                .iter()
                .position(|x| *x == k)
                .map(|i| self.key_names[i].clone())
                .unwrap_or_else(|| format!("{k:?}"))
        };
        let keymap_name = |k: KeymapId| self.keymaps[k.0].keymap.name.clone();
        describe::action_text(a, &key_name, &keymap_name)
    }

    /// `keyseq` の書かれた名前。
    fn keyseq_name(&self, name: &str) -> String {
        self.keyseq_names
            .get(&lower(name))
            .cloned()
            .unwrap_or_else(|| name.to_string())
    }

    /// `out` に動作を足す。`spans` には、動作から書けない部分（`$名` など）の
    /// （`out` の始め, 終わり, 書き方）を足す。
    fn load_actions_into(
        &mut self,
        cur: &mut Cur,
        in_paren: bool,
        locks_ok: bool,
        out: &mut Vec<Action>,
        spans: &mut Vec<(usize, usize, String)>,
    ) -> Result<()> {
        while !cur.at_end() {
            let mut m = self.load_modifier(cur)?;
            if !locks_ok {
                if m.locks_specified {
                    return self.err("IL- IC- は出力キーには付けられない");
                }
                m.locks = LockSpec::any();
            }
            let Some(t) = cur.next() else {
                return self.err("修飾の後にキー名が無い");
            };
            match t {
                Token::Sym(")") if in_paren => return Ok(()),
                Token::Sym("(") => {
                    let start = out.len();
                    let inner = self.load_actions(cur, true, locks_ok)?;
                    out.extend(inner);
                    // yamy は無名の keyseq にする
                    spans.push((start, out.len(), "$".to_string()));
                }
                Token::Sym("$") => {
                    let Some(t) = cur.next() else {
                        return self.err("`$` の後は keyseq 名");
                    };
                    let name = self.name_of(t)?;
                    let Some(seq) = self.keyseqs.get(&lower(&name)) else {
                        return self.err(format!("keyseq `${name}` は未定義"));
                    };
                    let start = out.len();
                    out.extend(seq.iter().cloned());
                    spans.push((start, out.len(), format!("${}", self.keyseq_name(&name))));
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
                    if fname.eq_ignore_ascii_case("KeymapWindow") {
                        out.push(Action::KeymapWindow);
                        continue;
                    }
                    if fname.eq_ignore_ascii_case("Ignore") {
                        out.push(Action::Ignore);
                        continue;
                    }
                    if fname.eq_ignore_ascii_case("Sync") {
                        out.push(Action::Sync);
                        continue;
                    }
                    if fname.eq_ignore_ascii_case("LogClear") {
                        out.push(Action::LogClear);
                        continue;
                    }
                    if fname.eq_ignore_ascii_case("DescribeBindings") {
                        out.push(Action::DescribeBindings);
                        continue;
                    }
                    if fname.eq_ignore_ascii_case("EditNextModifier") {
                        out.push(self.load_edit_next_modifier(cur)?);
                        continue;
                    }
                    if fname.eq_ignore_ascii_case("InvestigateCommand") {
                        out.push(Action::InvestigateCommand);
                        continue;
                    }
                    if fname.eq_ignore_ascii_case("LoadSetting") {
                        let args = self.load_args(cur)?;
                        let a = Args {
                            fname: &fname,
                            args: &args,
                            loader: self,
                        };
                        a.no_more(1)?;
                        out.push(Action::LoadSetting(a.string(0)?));
                        continue;
                    }
                    if fname.eq_ignore_ascii_case("MayuDialog") {
                        let args = self.load_args(cur)?;
                        let a = Args {
                            fname: &fname,
                            args: &args,
                            loader: self,
                        };
                        a.no_more(2)?;
                        // yamy と同じく 2 個とも要る
                        let Some(t) = args.first() else {
                            return self.err("`&MayuDialog`: 1 番目の引数（ダイアログ）が無い");
                        };
                        let dialog = self.name_of(t)?;
                        match lower(&dialog).as_str() {
                            "log" => {}
                            "investigate" => {
                                return self.err("`&MayuDialog(Investigate, …)` は未対応");
                            }
                            _ => {
                                return self
                                    .err(format!("`&MayuDialog`: ダイアログが不明: `{dialog}`"));
                            }
                        }
                        out.push(Action::MayuDialog(a.show_command(1)?));
                        continue;
                    }
                    if fname.eq_ignore_ascii_case("VK") {
                        out.push(self.load_vk(cur)?);
                        continue;
                    }
                    if fname.eq_ignore_ascii_case("Variable") {
                        let args = self.load_args(cur)?;
                        let a = Args {
                            fname: &fname,
                            args: &args,
                            loader: self,
                        };
                        a.no_more(2)?;
                        out.push(Action::Variable {
                            mag: a.int(0)?,
                            inc: a.int(1)?,
                        });
                        continue;
                    }
                    if fname.eq_ignore_ascii_case("Repeat") {
                        let (a, text) = self.load_repeat(cur)?;
                        spans.push((out.len(), out.len() + 1, text));
                        out.push(a);
                        continue;
                    }
                    if fname.eq_ignore_ascii_case("EmacsEditKillLineFunc") {
                        out.push(Action::EmacsEditKillLineFunc);
                        continue;
                    }
                    if fname.eq_ignore_ascii_case("EmacsEditKillLinePred") {
                        let (a, text) = self.load_kill_line_pred(cur)?;
                        spans.push((out.len(), out.len() + 1, text));
                        out.push(a);
                        continue;
                    }
                    if fname.eq_ignore_ascii_case("HelpMessage") {
                        let args = self.load_args(cur)?;
                        let a = Args {
                            fname: &fname,
                            args: &args,
                            loader: self,
                        };
                        a.no_more(2)?;
                        let (title, body) = (a.string(0)?, a.string(1)?);
                        // yamy と同じく、どちらも空なら消す
                        let text = (!title.is_empty() || !body.is_empty())
                            .then_some(HelpText { title, body });
                        out.push(Action::HelpMessage(text));
                        continue;
                    }
                    if fname.eq_ignore_ascii_case("HelpVariable") {
                        let args = self.load_args(cur)?;
                        let a = Args {
                            fname: &fname,
                            args: &args,
                            loader: self,
                        };
                        a.no_more(1)?;
                        if args.is_empty() {
                            return self.err("`&HelpVariable`: 1 番目の引数（文字列）が無い");
                        }
                        out.push(Action::HelpVariable(a.string(0)?));
                        continue;
                    }
                    if fname.eq_ignore_ascii_case("ShellExecute") {
                        let args = self.load_args(cur)?;
                        let a = Args {
                            fname: &fname,
                            args: &args,
                            loader: self,
                        };
                        a.no_more(5)?;
                        // yamy と同じく 5 個とも要る。文字列は引用符なしの語も受ける
                        if args.len() < 4 {
                            return self.err(format!(
                                "`&ShellExecute`: {} 番目の引数（文字列）が無い",
                                args.len() + 1
                            ));
                        }
                        out.push(Action::ShellExecute(ShellExecute {
                            operation: a.string(0)?,
                            file: a.string(1)?,
                            parameters: a.string(2)?,
                            directory: a.string(3)?,
                            show: a.show_command(4)?,
                        }));
                        continue;
                    }
                    if fname.eq_ignore_ascii_case("Keymap") {
                        out.push(Action::Keymap(self.load_keymap_arg(cur, "Keymap")?));
                        continue;
                    }
                    if let Some(f) = self.load_func(&fname, cur)? {
                        // `&WindowClingToLeft` 等は `&WindowMoveTo` として持つので、名前で書く
                        if let Func::WindowMoveTo { target, .. } = f
                            && let Some(canon) = cling_name(&fname)
                        {
                            spans.push((
                                out.len(),
                                out.len() + 1,
                                describe::cling_text(canon, target),
                            ));
                        }
                        out.push(Action::Func(f));
                        continue;
                    }
                    if !fname.eq_ignore_ascii_case("Prefix") {
                        return self.err(format!("`&{fname}` は未対応"));
                    }
                    out.push(self.load_prefix(cur)?);
                }
                t => {
                    let key = self.key_by_name(t)?;
                    out.push(Action::Key {
                        key,
                        mods: m.mods,
                        edge: m.edge,
                        locks: m.locks,
                    });
                }
            }
        }
        if in_paren {
            return self.err("`)` が無い");
        }
        Ok(())
    }

    /// OS 側で実行する `&関数` とその引数。知らない名前なら `None`。
    /// 引数の `( … )` は省略でき、省略した引数は yamy と同じ既定値になる。
    /// `&Prefix(名前)` / `&Keymap(名前)` の `(名前)`。名前は定義済みのキーマップ。
    fn load_keymap_arg(&self, cur: &mut Cur, what: &str) -> Result<KeymapId> {
        self.expect_sym(cur, "(")?;
        let Some(t) = cur.next() else {
            return self.err(format!("&{what}( の後はキーマップ名"));
        };
        let kname = self.name_of(t)?;
        let Some(&idx) = self.keymap_names.get(&lower(&kname)) else {
            return self.err(format!("キーマップ `{kname}` は未定義"));
        };
        self.expect_sym(cur, ")")?;
        Ok(KeymapId(idx))
    }

    /// `&EditNextModifier(修飾)`。書かなかった修飾・鍵は dontcare（yamy の `load_ARGUMENT(Modifier)`）。
    fn load_edit_next_modifier(&self, cur: &mut Cur) -> Result<Action> {
        self.expect_sym(cur, "(")?;
        let mut mods = ModSpec::any();
        let mut locks = LockSpec::any();
        let mut flag = Flag::Press;
        loop {
            match cur.next() {
                Some(Token::Sym("*")) => flag = Flag::Dontcare,
                Some(Token::Sym("~")) => flag = Flag::Release,
                Some(Token::Sym(s)) if basic_mod_by_prefix(s).is_some() => {
                    let m = basic_mod_by_prefix(s).expect("上で確かめた");
                    mods = match flag {
                        Flag::Press => mods.with(m),
                        Flag::Release => mods.without(m),
                        Flag::Dontcare => mods.dontcare(m),
                    };
                    flag = Flag::Press;
                }
                Some(Token::Sym(s)) if lock_mod_by_prefix(s).is_some() => {
                    let m = lock_mod_by_prefix(s).expect("上で確かめた");
                    locks = match flag {
                        Flag::Press => locks.with(m),
                        Flag::Release => locks.without(m),
                        Flag::Dontcare => locks.dontcare(m),
                    };
                    flag = Flag::Press;
                }
                Some(Token::Sym(")")) if flag == Flag::Press => {
                    return Ok(Action::EditNextModifier(mods, locks));
                }
                Some(Token::Name(s)) if s.is_empty() && flag == Flag::Press => {
                    // `&EditNextModifier()` の空の引数
                    self.expect_sym(cur, ")")?;
                    return Ok(Action::EditNextModifier(mods, locks));
                }
                Some(t) => {
                    return self.err(format!("`&EditNextModifier`: 修飾が要る所に `{t}`"));
                }
                None => return self.err("`)` が無い"),
            }
        }
    }

    /// `&Prefix(キーマップ [, ignore_modifiers])`。ignore_modifiers の既定は true（yamy と同じ）。
    fn load_prefix(&self, cur: &mut Cur) -> Result<Action> {
        self.expect_sym(cur, "(")?;
        let Some(t) = cur.next() else {
            return self.err("&Prefix( の後はキーマップ名");
        };
        let kname = self.name_of(t)?;
        let Some(&idx) = self.keymap_names.get(&lower(&kname)) else {
            return self.err(format!("キーマップ `{kname}` は未定義"));
        };
        let mut ignore_modifiers = true;
        if cur.peek_sym(",") {
            cur.next();
            let Some(t) = cur.next() else {
                return self.err("`&Prefix`: 2 番目の引数は true か false");
            };
            let name = self.name_of(t)?;
            ignore_modifiers = match lower(&name).as_str() {
                "true" => true,
                "false" => false,
                _ => return self.err(format!("`&Prefix`: `{name}` は true か false")),
            };
        }
        self.expect_sym(cur, ")")?;
        Ok(Action::Prefix(KeymapId(idx), ignore_modifiers))
    }

    fn load_func(&self, fname: &str, cur: &mut Cur) -> Result<Option<Func>> {
        let lname = lower(fname);
        // 引数の最大数。知らない名前は None（引数を読まずに返す）
        let max = match lname.as_str() {
            "windowmove" | "windowresizeto" => 3,
            "windowmoveto" => 4,
            "mousemove" => 2,
            "mousewheel" => 1,
            "windowtoggletopmost"
            | "windowredraw"
            | "recenter"
            | "windowidentify"
            | "undefined"
            | "clipboardupcaseword"
            | "clipboarddowncaseword" => 0,
            "postmessage" => 4,
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
            "undefined" => Func::Undefined,
            "postmessage" => Func::PostMessage {
                to: a.to_window(0)?,
                message: a.int(1)? as u32,
                wparam: a.i64(2)? as u64,
                lparam: a.i64(3)?,
            },
            "windowsetalpha" => Func::WindowSetAlpha(a.int(0)?),
            "windowredraw" => Func::WindowRedraw,
            "recenter" => Func::Recenter,
            "windowidentify" => Func::WindowIdentify,
            "clipboardupcaseword" => Func::ClipboardUpcaseWord,
            "clipboarddowncaseword" => Func::ClipboardDowncaseWord,
            "windowresizeto" => Func::WindowResizeTo {
                width: a.int(0)?,
                height: a.int(1)?,
                target: a.target(2)?,
            },
            "mousemove" => Func::MouseMove {
                dx: a.int(0)?,
                dy: a.int(1)?,
            },
            "mousewheel" => Func::MouseWheel(a.int(0)?),
            _ => unreachable!("上の max の表と同じ名前の集合"),
        };
        Ok(Some(f))
    }

    /// `&VK( [E-] [D-|U-] 名前|番号 )`。yamy の `load_ARGUMENT(VKey*)`。
    fn load_vk(&self, cur: &mut Cur) -> Result<Action> {
        self.expect_sym(cur, "(")?;
        let mut extended = false;
        let mut down = false;
        let mut up = false;
        let vk = loop {
            match cur.next() {
                Some(Token::Sym("E-")) => extended = true,
                Some(Token::Sym("D-")) => down = true,
                Some(Token::Sym("U-")) => up = true,
                Some(Token::Number(v, s)) => match u8::try_from(*v) {
                    Ok(v) => break v,
                    Err(_) => return self.err(format!("`&VK`: 仮想キーコードは 0〜255: `{s}`")),
                },
                Some(t) => {
                    // `E-D-Return` のように名前に続く接頭辞は 1 つの名前として届くので、ここで剥がす
                    let mut name = self.name_of(t)?.as_str().to_string();
                    loop {
                        let head = name.get(..2).map(|h| h.to_ascii_uppercase());
                        match head.as_deref() {
                            Some("E-") => extended = true,
                            Some("D-") => down = true,
                            Some("U-") => up = true,
                            _ => break,
                        }
                        name = name[2..].to_string();
                    }
                    match vk_by_name(&name) {
                        Some(v) => break v,
                        None => return self.err(format!("`&VK`: 知らない仮想キー名 `{name}`")),
                    }
                }
                None => return self.err("`&VK(` の後は仮想キー名"),
            }
        };
        self.expect_sym(cur, ")")?;
        if !down && !up {
            down = true;
            up = true;
        }
        Ok(Action::Vk {
            vk,
            extended,
            down,
            up,
        })
    }

    /// `( 引数 , 引数 … )` を読む。`(` が無ければ空。
    /// 空の引数は字句解析が空の名前にしてあるので、`()` は空の引数 1 個になる（yamy と同じ）。
    fn load_args(&self, cur: &mut Cur) -> Result<Vec<Token>> {
        let mut args = Vec::new();
        if !cur.peek_sym("(") {
            return Ok(args);
        }
        cur.next();
        loop {
            match cur.next() {
                // 並んだ文字列は 1 つにつなぐ（`"…\n" \ 改行 "…"` で長い本文を書く）
                Some(Token::Str(s)) => {
                    let mut s = s.clone();
                    while let Some(Token::Str(more)) = cur.peek() {
                        s.push_str(more);
                        cur.next();
                    }
                    args.push(Token::Str(s));
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
                // （キー, `!!` 付きか）
                let mut keys: Vec<(Key, bool)> = match km.parent {
                    Some(p) => {
                        let parent = &resolved[p.0];
                        parent.mod_keys[i]
                            .iter()
                            .map(|k| (*k, parent.is_one_shot(BasicMod::ALL[i], *k)))
                            .collect()
                    }
                    None => self.def_mods[i].iter().map(|k| (*k, false)).collect(),
                };
                for (op, key, one_shot) in ops {
                    match op {
                        ModOp::New => {
                            keys.clear();
                            keys.push((*key, *one_shot));
                        }
                        ModOp::Add => keys.push((*key, *one_shot)),
                        ModOp::Sub => {
                            if let Some(pos) = keys.iter().position(|(k, _)| k == key) {
                                keys.remove(pos);
                            }
                        }
                    }
                }
                // 重なったキーは最初の位置に、後に書いた `!!` の有無で残す
                let mut dedup: Vec<(Key, bool)> = Vec::with_capacity(keys.len());
                for (k, one_shot) in keys {
                    match dedup.iter_mut().find(|(d, _)| *d == k) {
                        Some(d) => d.1 = one_shot,
                        None => dedup.push((k, one_shot)),
                    }
                }
                km.mod_keys[i] = dedup.iter().map(|(k, _)| *k).collect();
                km.one_shot_keys[i] = dedup.iter().filter(|(_, o)| *o).map(|(k, _)| *k).collect();
            }
            resolved.push(km);
        }
        let mut config = Config::new(resolved);
        for (i, keys) in self.def_mods.iter().enumerate() {
            config.def_mod(BasicMod::ALL[i], keys.iter().copied());
        }
        for (k, n) in self.keys.iter().zip(&self.key_names) {
            if !config.key_names.iter().any(|(c, _)| c == k) {
                config.key_names.push((*k, n.clone()));
            }
        }
        config.keys = self.keys;
        config.sync = self.sync;
        config.substs = self.substs;
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

    /// `keyseq` の中のキー。鍵の条件は割付の左辺と同じ既定（変換中でない）を持つ。
    fn in_keyseq(key: Key, mods: ModSpec) -> Action {
        Action::Key {
            key,
            mods,
            edge: EdgeSpec::Both,
            locks: LockSpec::assign_default(),
        }
    }

    fn keys_of(km: &Keymap) -> Vec<Key> {
        km.assignments.iter().map(|a| a.key).collect()
    }

    fn named<'a>(c: &'a Config, name: &str) -> &'a Keymap {
        c.keymaps.iter().find(|k| k.name == name).unwrap()
    }

    fn id_of(c: &Config, name: &str) -> KeymapId {
        KeymapId(c.keymaps.iter().position(|k| k.name == name).unwrap())
    }

    /// 一時フォルダにファイルを書いて、先頭のファイルを読ませる。`files` は（相対パス, 中身）。
    fn load_files(
        tag: &str,
        files: &[(&str, &[u8])],
        symbols: &[String],
    ) -> std::result::Result<Config, LoadError> {
        let dir = std::env::temp_dir().join(format!("newmayu-{tag}-{}", std::process::id()));
        for (name, bytes) in files {
            let path = dir.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }
        let r = load(&dir.join(files[0].0), symbols);
        std::fs::remove_dir_all(&dir).unwrap();
        r
    }

    // ---- def key / def mod / key ------------------------------------------

    const SIMPLE: &str = "key A = B\nkey bs = Space";

    #[test]
    fn def_mod_lists_the_modifier_keys() {
        // def mod shift = LShift RShift / def mod control = LControl RControl
        let c = load_ok(SIMPLE);
        assert_eq!(c.def_mods[BasicMod::Shift.index()], vec![LSHIFT, RSHIFT]);
        assert_eq!(
            c.def_mods[BasicMod::Control.index()],
            vec![LCONTROL, RCONTROL]
        );
    }

    #[test]
    fn first_keymap_is_global() {
        let c = load_ok(SIMPLE);
        assert_eq!(global(&c).name, "Global");
    }

    #[test]
    fn global_starts_from_def_mod() {
        let c = load_ok(SIMPLE);
        assert_eq!(
            global(&c).mod_keys[BasicMod::Shift.index()],
            vec![LSHIFT, RSHIFT]
        );
    }

    #[test]
    fn key_assigns_an_output_key() {
        // key A = B
        let c = load_ok(SIMPLE);
        let a = only(global(&c), A);
        assert_eq!(a.mods, base());
        assert_eq!(a.edge, EdgeSpec::Both);
        assert_eq!(a.actions, vec![Action::key_with(B, base())]);
    }

    #[test]
    fn second_name_of_def_key_names_the_key_ignoring_case() {
        // def key BackSpace BS = 0x0e / key bs = Space
        let c = load_ok(SIMPLE);
        assert_eq!(
            only(global(&c), BS).actions[0],
            Action::key_with(SPACE, base())
        );
    }

    // ---- 修飾 ---------------------------------------------------------------

    const MODIFIERS: &str = "key C-S-A = B
             key *H = B
             key ~S-*Space = B
             key D-BackSpace = B
             key U-LAlt = S-*B D-LShift U-LShift";

    #[test]
    fn c_and_s_prefixes_require_the_modifiers() {
        // key C-S-A = B
        let c = load_ok(MODIFIERS);
        assert_eq!(
            only(global(&c), A).mods,
            base().with(BasicMod::Control).with(BasicMod::Shift)
        );
    }

    #[test]
    fn star_before_the_key_name_makes_all_modifiers_dontcare() {
        // key *H = B
        let c = load_ok(MODIFIERS);
        assert_eq!(only(global(&c), H).mods, ModSpec::any());
    }

    #[test]
    fn tilde_requires_the_modifier_released() {
        // key ~S-*Space = B
        let c = load_ok(MODIFIERS);
        assert_eq!(
            only(global(&c), SPACE).mods,
            ModSpec::any().without(BasicMod::Shift)
        );
    }

    #[test]
    fn d_prefix_is_key_down_only() {
        // key D-BackSpace = B
        let c = load_ok(MODIFIERS);
        assert_eq!(only(global(&c), BS).edge, EdgeSpec::Down);
    }

    #[test]
    fn u_prefix_is_key_up_only() {
        // key U-LAlt = …
        let c = load_ok(MODIFIERS);
        assert_eq!(only(global(&c), LALT).edge, EdgeSpec::Up);
    }

    #[test]
    fn output_keys_take_modifier_and_edge_prefixes() {
        // key U-LAlt = S-*B D-LShift U-LShift
        let c = load_ok(MODIFIERS);
        assert_eq!(
            only(global(&c), LALT).actions,
            vec![
                Action::Key {
                    key: B,
                    mods: ModSpec::any().with(BasicMod::Shift),
                    edge: EdgeSpec::Both,
                    locks: LockSpec::any()
                },
                Action::Key {
                    key: LSHIFT,
                    mods: base(),
                    edge: EdgeSpec::Down,
                    locks: LockSpec::any()
                },
                Action::Key {
                    key: LSHIFT,
                    mods: base(),
                    edge: EdgeSpec::Up,
                    locks: LockSpec::any()
                },
            ]
        );
    }

    // ---- keyseq / keymap ----------------------------------------------------

    const KEYSEQ: &str = "define KBD109
             def sync = 0x7e
             keyseq $CapsLock = S-英数
             keymap Sub
              key B = A
             keymap Main
              key A = $capslock ( B H ) &Prefix(Sub)
              key C-B = &default";

    #[test]
    fn keymaps_are_numbered_in_definition_order() {
        let c = load_ok(KEYSEQ);
        assert_eq!(c.keymaps[2].name, "Main");
        assert_eq!(c.keymap(KeymapId(1)).name, "Sub");
    }

    #[test]
    fn keyseq_and_group_expand_in_place() {
        // key A = $capslock ( B H ) &Prefix(Sub)
        let c = load_ok(KEYSEQ);
        assert_eq!(
            only(&c.keymaps[2], A).actions,
            vec![
                in_keyseq(EISUU, base().with(BasicMod::Shift)),
                Action::key_with(B, base()),
                Action::key_with(H, base()),
                Action::Prefix(KeymapId(1), true),
            ]
        );
    }

    #[test]
    fn function_names_ignore_case() {
        // key C-B = &default
        let c = load_ok(KEYSEQ);
        let cb = only(&c.keymaps[2], B);
        assert!(cb.mods.is_pressed(BasicMod::Control));
        assert_eq!(cb.actions, vec![Action::Default]);
    }

    #[test]
    fn keymap_function_names_a_defined_keymap() {
        let c = load_ok(
            "keymap Sub
              key B = A
             keymap Main
              key A = &Keymap(sub)",
        );
        assert_eq!(
            only(&c.keymaps[2], A).actions,
            vec![Action::Keymap(KeymapId(1))]
        );
    }

    #[test]
    fn keymap_function_with_an_unknown_name_is_an_error() {
        assert!(
            load_err("key A = &Keymap(Nowhere)")
                .message
                .contains("未定義")
        );
    }

    // ---- mod ----------------------------------------------------------------

    const MOD_INHERIT: &str = "mod shift += E0RShift
             keymap Child : Global
              mod control -= LControl
              mod alt = 英数
             keymap Root
              mod shift = 英数";

    #[test]
    fn mod_plus_equal_adds_a_key() {
        // mod shift += E0RShift
        let c = load_ok(MOD_INHERIT);
        assert_eq!(
            c.keymaps[0].mod_keys[BasicMod::Shift.index()],
            vec![LSHIFT, RSHIFT, E0RSHIFT]
        );
    }

    #[test]
    fn child_keymap_inherits_the_modifier_keys() {
        // keymap Child : Global
        let c = load_ok(MOD_INHERIT);
        let child = &c.keymaps[1];
        assert_eq!(child.parent, Some(KeymapId(0)));
        assert_eq!(
            child.mod_keys[BasicMod::Shift.index()],
            vec![LSHIFT, RSHIFT, E0RSHIFT]
        );
    }

    #[test]
    fn mod_minus_equal_removes_a_key() {
        // mod control -= LControl
        let c = load_ok(MOD_INHERIT);
        assert_eq!(
            c.keymaps[1].mod_keys[BasicMod::Control.index()],
            vec![RCONTROL]
        );
    }

    #[test]
    fn mod_equal_replaces_the_keys() {
        // mod alt = 英数
        let c = load_ok(MOD_INHERIT);
        assert_eq!(c.keymaps[1].mod_keys[BasicMod::Alt.index()], vec![EISUU]);
    }

    #[test]
    fn keymap_without_a_parent_starts_from_def_mod() {
        // keymap Root / mod shift = 英数
        let c = load_ok(MOD_INHERIT);
        let root = &c.keymaps[2];
        assert_eq!(root.parent, None);
        assert_eq!(root.mod_keys[BasicMod::Shift.index()], vec![EISUU]);
        assert_eq!(
            root.mod_keys[BasicMod::Control.index()],
            vec![LCONTROL, RCONTROL]
        );
    }

    const ONE_SHOT: &str = "mod alt = !!LAlt
             mod control += !!英数
             mod control += 英数
             mod shift += !!E0RShift
             keymap Child : Global
              mod shift -= E0RShift";

    #[test]
    fn one_shot_keys_are_modifier_keys() {
        let c = load_ok(ONE_SHOT);
        let g = &c.keymaps[0];
        assert_eq!(g.mod_keys[BasicMod::Alt.index()], vec![LALT]);
        assert_eq!(
            g.mod_keys[BasicMod::Control.index()],
            vec![LCONTROL, RCONTROL, EISUU]
        );
    }

    #[test]
    fn double_bang_marks_a_one_shot_modifier() {
        // mod alt = !!LAlt / mod shift += !!E0RShift
        let c = load_ok(ONE_SHOT);
        let g = &c.keymaps[0];
        assert!(g.is_one_shot(BasicMod::Alt, LALT));
        assert!(g.is_one_shot(BasicMod::Shift, E0RSHIFT));
        assert!(!g.is_one_shot(BasicMod::Shift, LSHIFT));
    }

    #[test]
    fn rewriting_a_modifier_key_uses_the_last_form() {
        // mod control += !!英数 / mod control += 英数
        // 同じキーマップで書き直すと最後の書き方になる（yamy の addModifier）
        let c = load_ok(ONE_SHOT);
        assert!(!c.keymaps[0].is_one_shot(BasicMod::Control, EISUU));
    }

    #[test]
    fn child_keymap_inherits_one_shot() {
        let c = load_ok(ONE_SHOT);
        assert!(c.keymaps[1].is_one_shot(BasicMod::Alt, LALT));
    }

    #[test]
    fn removed_key_is_no_longer_one_shot() {
        // keymap Child : Global / mod shift -= E0RShift
        let c = load_ok(ONE_SHOT);
        assert!(!c.keymaps[1].is_one_shot(BasicMod::Shift, E0RSHIFT));
    }

    #[test]
    fn single_bang_modifier_is_not_supported() {
        assert!(load_err("mod alt = !LAlt").message.contains("未対応"));
    }

    #[test]
    fn triple_bang_modifier_is_not_supported() {
        assert!(load_err("mod alt = !!!LAlt").message.contains("未対応"));
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
    }

    #[test]
    fn reopening_a_keymap_with_another_parent_is_an_error() {
        assert!(
            load_err(
                "keymap Sub
keymap Sub : Global"
            )
            .message
            .contains("親は変えられない")
        );
    }

    // ---- if / else / endif --------------------------------------------------

    const IF_SRC: &str = "define KBD109
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
             key LAlt = B";

    /// `IF_SRC` を読んで割付が入ったキー。
    fn if_keys() -> Vec<Key> {
        keys_of(global(&load_ok(IF_SRC)))
    }

    #[test]
    fn if_reads_the_block_when_the_symbol_is_defined() {
        assert!(if_keys().contains(&A));
    }

    #[test]
    fn nested_else_is_read_when_the_nested_if_is_false() {
        let c = load_ok(IF_SRC);
        assert_eq!(only(global(&c), H).actions[0], Action::key_with(A, base()));
    }

    #[test]
    fn else_is_skipped_when_the_if_is_true() {
        assert!(!if_keys().contains(&SPACE), "else 側は読まない");
    }

    #[test]
    fn one_line_if_is_skipped_when_false() {
        assert!(!if_keys().contains(&BS), "1 行 if は条件が偽なら読まない");
    }

    #[test]
    fn and_is_true_when_both_are_true() {
        assert!(if_keys().contains(&LSHIFT), "and で両方真");
    }

    #[test]
    fn and_is_false_when_one_is_false() {
        assert!(!if_keys().contains(&RSHIFT), "and で片方偽");
    }

    #[test]
    fn elseif_is_read_when_the_if_is_false() {
        assert!(if_keys().contains(&LCONTROL), "elseif");
    }

    #[test]
    fn define_in_a_skipped_block_has_no_effect() {
        assert!(
            !if_keys().contains(&RCONTROL),
            "読まない区間の define は効かない"
        );
    }

    #[test]
    fn statements_after_endif_are_read() {
        assert!(if_keys().contains(&LALT), "endif の後は読む");
    }

    #[test]
    fn endif_without_if_is_an_error() {
        assert!(load_err("endif").message.contains("対応する if"));
    }

    #[test]
    fn else_without_if_is_an_error() {
        assert!(load_err("else").message.contains("対応する if"));
    }

    #[test]
    fn if_needs_a_parenthesis() {
        assert!(load_err("if KBD109").message.contains("`(`"));
    }

    // ---- IME・ロックの条件 ----------------------------------------------------

    const IME_SRC: &str = "key A = B
             key IC-H = B
             key I-Space = B
             key IL-BackSpace = B
             key *IC-C-LShift = B
             key *RShift = B
             key ~IL-LControl = B
             key ~*RControl = B
             key S-~NL-LAlt = B
             key CL-SL-KL-Eisuu = B";

    #[test]
    fn default_condition_requires_not_composing() {
        // key A = B
        // 既定: 変換中でないことを要求、IME オンは問わない
        let c = load_ok(IME_SRC);
        let d = LockSpec::assign_default();
        assert!(!d.is_dontcare(LockMod::ImeComp) && !d.is_on(LockMod::ImeComp));
        assert!(d.is_dontcare(LockMod::ImeLock));
        assert_eq!(only(global(&c), A).locks, d);
    }

    #[test]
    fn ic_prefix_requires_composing() {
        // key IC-H = B
        let c = load_ok(IME_SRC);
        assert_eq!(
            only(global(&c), H).locks,
            LockSpec::assign_default().with(LockMod::ImeComp)
        );
    }

    #[test]
    fn i_prefix_is_the_same_as_ic() {
        // key I-Space = B
        let c = load_ok(IME_SRC);
        assert_eq!(
            only(global(&c), SPACE).locks,
            LockSpec::assign_default().with(LockMod::ImeComp)
        );
    }

    #[test]
    fn il_prefix_requires_ime_on() {
        // key IL-BackSpace = B
        let c = load_ok(IME_SRC);
        assert_eq!(
            only(global(&c), BS).locks,
            LockSpec::assign_default().with(LockMod::ImeLock)
        );
    }

    #[test]
    fn star_ic_does_not_care_about_composing() {
        // key *IC-C-LShift = B
        // `*IC-` は変換中かを問わない。基本修飾は普通に読む
        let c = load_ok(IME_SRC);
        let ls = only(global(&c), LSHIFT);
        assert_eq!(ls.locks, LockSpec::any());
        assert_eq!(ls.mods, base().with(BasicMod::Control));
    }

    #[test]
    fn star_before_the_key_name_makes_all_locks_dontcare() {
        // key *RShift = B
        let c = load_ok(IME_SRC);
        assert_eq!(only(global(&c), RSHIFT).locks, LockSpec::any());
    }

    #[test]
    fn tilde_il_requires_ime_off() {
        // key ~IL-LControl = B
        let c = load_ok(IME_SRC);
        assert_eq!(
            only(global(&c), LCONTROL).locks,
            LockSpec::assign_default().without(LockMod::ImeLock)
        );
    }

    #[test]
    fn tilde_star_is_the_same_as_star() {
        // key ~*RControl = B
        // `~*` はキー名の直前の `*` と同じで、全部 dontcare
        let c = load_ok(IME_SRC);
        assert_eq!(only(global(&c), RCONTROL).locks, LockSpec::any());
    }

    #[test]
    fn tilde_nl_requires_num_lock_off() {
        // key S-~NL-LAlt = B
        let c = load_ok(IME_SRC);
        let la = only(global(&c), LALT);
        assert_eq!(
            la.locks,
            LockSpec::assign_default().without(LockMod::NumLock)
        );
        assert_eq!(la.mods, base().with(BasicMod::Shift));
    }

    #[test]
    fn cl_sl_kl_prefixes_require_the_locks() {
        // key CL-SL-KL-Eisuu = B
        let c = load_ok(IME_SRC);
        assert_eq!(
            only(global(&c), EISUU).locks,
            LockSpec::assign_default()
                .with(LockMod::CapsLock)
                .with(LockMod::ScrollLock)
                .with(LockMod::KanaLock)
        );
    }

    // ---- window / keymap2 ---------------------------------------------------

    const WINDOW_SRC: &str = "window Edit /:Edit$/ : Global
              key A = H
             window Both ( /notepad/ && /メモ帳/ ) : Edit
             window Either ( /qq/ || /zz/ )
             keymap2 Sub : Global = &KeymapParent
             keymap KeymapDefault = &Default
             window Plain : Global
             keymap Global";

    #[test]
    fn global_default_action_is_other_window_class() {
        let c = load_ok(WINDOW_SRC);
        assert_eq!(global(&c).default_actions, vec![Action::OtherWindowClass]);
    }

    #[test]
    fn global_matches_any_window() {
        let c = load_ok(WINDOW_SRC);
        assert!(global(&c).window.as_ref().unwrap().matches("", ""));
    }

    #[test]
    fn window_matches_the_class_regex() {
        // window Edit /:Edit$/ : Global
        let c = load_ok(WINDOW_SRC);
        let w = named(&c, "Edit").window.as_ref().unwrap();
        assert!(w.matches("notepad.exe:Notepad:Edit", ""));
        assert!(!w.matches("x:Editor", ""));
    }

    #[test]
    fn window_regex_ignores_case() {
        let c = load_ok(WINDOW_SRC);
        let w = named(&c, "Edit").window.as_ref().unwrap();
        assert!(w.matches("x:EDIT", "大文字小文字は区別しない"));
    }

    #[test]
    fn window_default_action_is_keymap_parent() {
        let c = load_ok(WINDOW_SRC);
        assert_eq!(
            named(&c, "Edit").default_actions,
            vec![Action::KeymapParent]
        );
    }

    #[test]
    fn key_after_window_goes_into_the_window_keymap() {
        // window Edit /:Edit$/ : Global / key A = H
        let c = load_ok(WINDOW_SRC);
        assert_eq!(
            only(named(&c, "Edit"), A).actions,
            vec![Action::key_with(H, base())]
        );
    }

    #[test]
    fn window_with_and_needs_both_class_and_title() {
        // window Both ( /notepad/ && /メモ帳/ ) : Edit
        let c = load_ok(WINDOW_SRC);
        let both = named(&c, "Both").window.as_ref().unwrap();
        assert!(both.matches("notepad.exe:Edit", "無題 - メモ帳"));
        assert!(!both.matches("notepad.exe:Edit", ""));
    }

    #[test]
    fn window_with_or_needs_class_or_title() {
        // window Either ( /qq/ || /zz/ )
        let c = load_ok(WINDOW_SRC);
        let either = named(&c, "Either").window.as_ref().unwrap();
        assert!(either.matches("qq", ""));
        assert!(either.matches("", "zz"));
        assert!(!either.matches("", ""));
    }

    #[test]
    fn keymap2_reads_its_default_action() {
        // keymap2 Sub : Global = &KeymapParent
        let c = load_ok(WINDOW_SRC);
        assert_eq!(named(&c, "Sub").default_actions, vec![Action::KeymapParent]);
    }

    #[test]
    fn keymap_reads_its_default_action() {
        // keymap KeymapDefault = &Default
        let c = load_ok(WINDOW_SRC);
        assert_eq!(
            named(&c, "KeymapDefault").default_actions,
            vec![Action::Default]
        );
    }

    #[test]
    fn window_without_a_condition_is_a_keymap() {
        // window Plain : Global
        let c = load_ok(WINDOW_SRC);
        let plain = named(&c, "Plain");
        assert!(plain.window.is_none(), "条件の無い window は普通の keymap");
        assert_eq!(plain.parent, Some(KeymapId(0)));
    }

    #[test]
    fn later_window_is_matched_first_and_global_is_last() {
        let c = load_ok(WINDOW_SRC);
        assert_eq!(
            c.focus_keymaps("notepad.exe:Notepad:Edit", "無題 - メモ帳"),
            vec![id_of(&c, "Both"), id_of(&c, "Edit"), KeymapId(0)]
        );
    }

    #[test]
    fn unmatched_window_uses_only_global() {
        let c = load_ok(WINDOW_SRC);
        assert_eq!(c.focus_keymaps("chrome.exe:Chrome", ""), vec![KeymapId(0)]);
    }

    #[test]
    fn bad_window_regex_is_an_error() {
        assert!(
            load_err("window Bad /(/ : Global")
                .message
                .contains("正規表現")
        );
    }

    #[test]
    fn window_condition_needs_an_operator() {
        assert!(load_err("window Bad ( /a/ /b/ )").message.contains("`&&`"));
    }

    #[test]
    fn keymap_default_action_cannot_be_changed() {
        assert!(
            load_err("keymap K = &Default\nkeymap K = &KeymapParent")
                .message
                .contains("既定動作")
        );
    }

    #[test]
    fn window_condition_cannot_be_changed() {
        assert!(
            load_err("window W /a/\nwindow W /b/")
                .message
                .contains("窓の条件")
        );
    }

    // ---- 窓の関数と引数 --------------------------------------------------------

    const WINFUNC_SRC: &str = "key A = &WindowMove(-16, 0)
             key B = &WindowMove(1, 2, MDI)
             key H = &WindowLower &WindowRaise(mdi) &WindowMaximize
             key Space = &WindowClingToLeft &WindowClingToBottom(MDI)
             key BackSpace = &WindowSetAlpha(70) &WindowToggleTopMost &WindowRedraw &Recenter
             key LShift = &MouseMove(0, -1) &WindowResizeTo(0, -100)
             key RShift = &WindowMoveTo(NE, 3, 4)
             key LControl = &Ignore &Undefined";

    #[test]
    fn window_move_reads_the_offsets() {
        // key A = &WindowMove(-16, 0)
        let c = load_ok(WINFUNC_SRC);
        assert_eq!(
            only(global(&c), A).actions,
            vec![Action::Func(Func::WindowMove {
                dx: -16,
                dy: 0,
                target: Target::Overlapped
            })]
        );
    }

    #[test]
    fn window_move_reads_the_target() {
        // key B = &WindowMove(1, 2, MDI)
        let c = load_ok(WINFUNC_SRC);
        assert_eq!(
            only(global(&c), B).actions,
            vec![Action::Func(Func::WindowMove {
                dx: 1,
                dy: 2,
                target: Target::Mdi
            })]
        );
    }

    #[test]
    fn omitted_target_is_the_overlapped_window() {
        // key H = &WindowLower &WindowRaise(mdi) &WindowMaximize
        let c = load_ok(WINFUNC_SRC);
        assert_eq!(
            only(global(&c), H).actions,
            vec![
                Action::Func(Func::WindowLower(Target::Overlapped)),
                Action::Func(Func::WindowRaise(Target::Mdi)),
                Action::Func(Func::WindowMaximize(Target::Overlapped)),
            ]
        );
    }

    #[test]
    fn window_cling_to_is_window_move_to_with_zero_offsets() {
        // key Space = &WindowClingToLeft &WindowClingToBottom(MDI)
        let c = load_ok(WINFUNC_SRC);
        assert_eq!(
            only(global(&c), SPACE).actions,
            vec![
                Action::Func(Func::WindowMoveTo {
                    gravity: Gravity::West,
                    dx: 0,
                    dy: 0,
                    target: Target::Overlapped
                }),
                Action::Func(Func::WindowMoveTo {
                    gravity: Gravity::South,
                    dx: 0,
                    dy: 0,
                    target: Target::Mdi
                }),
            ]
        );
    }

    #[test]
    fn functions_can_be_lined_up_without_parentheses() {
        // key BackSpace = &WindowSetAlpha(70) &WindowToggleTopMost &WindowRedraw &Recenter
        let c = load_ok(WINFUNC_SRC);
        assert_eq!(
            only(global(&c), BS).actions,
            vec![
                Action::Func(Func::WindowSetAlpha(70)),
                Action::Func(Func::WindowToggleTopMost),
                Action::Func(Func::WindowRedraw),
                Action::Func(Func::Recenter),
            ]
        );
    }

    #[test]
    fn number_arguments_can_be_zero_or_negative() {
        // key LShift = &MouseMove(0, -1) &WindowResizeTo(0, -100)
        let c = load_ok(WINFUNC_SRC);
        assert_eq!(
            only(global(&c), LSHIFT).actions,
            vec![
                Action::Func(Func::MouseMove { dx: 0, dy: -1 }),
                Action::Func(Func::WindowResizeTo {
                    width: 0,
                    height: -100,
                    target: Target::Overlapped
                }),
            ]
        );
    }

    #[test]
    fn window_move_to_reads_the_gravity() {
        // key RShift = &WindowMoveTo(NE, 3, 4)
        let c = load_ok(WINFUNC_SRC);
        assert_eq!(
            only(global(&c), RSHIFT).actions,
            vec![Action::Func(Func::WindowMoveTo {
                gravity: Gravity::NorthEast,
                dx: 3,
                dy: 4,
                target: Target::Overlapped
            })]
        );
    }

    #[test]
    fn ignore_and_undefined_are_read() {
        // key LControl = &Ignore &Undefined
        let c = load_ok(WINFUNC_SRC);
        assert_eq!(
            only(global(&c), LCONTROL).actions,
            vec![Action::Ignore, Action::Func(Func::Undefined)]
        );
    }

    #[test]
    fn undefined_takes_no_argument() {
        assert!(
            load_err("key A = &Undefined(1)")
                .message
                .contains("引数が多い")
        );
    }

    #[test]
    fn window_move_needs_arguments() {
        assert!(load_err("key A = &WindowMove").message.contains("引数"));
    }

    #[test]
    fn window_move_needs_two_numbers() {
        assert!(load_err("key A = &WindowMove(1)").message.contains("引数"));
    }

    #[test]
    fn window_move_takes_at_most_three_arguments() {
        assert!(
            load_err("key A = &WindowMove(1, 2, MDI, 4)")
                .message
                .contains("引数が多い")
        );
    }

    #[test]
    fn window_move_offset_must_be_a_number() {
        assert!(load_err("key A = &WindowMove(a, 2)").message.contains("数"));
    }

    #[test]
    fn unknown_target_is_an_error() {
        assert!(
            load_err("key A = &WindowLower(Foo)")
                .message
                .contains("overlapped")
        );
    }

    #[test]
    fn missing_close_parenthesis_is_an_error() {
        assert!(load_err("key A = &MouseMove(1, 2").message.contains("`)`"));
    }

    #[test]
    fn mouse_wheel_reads_the_delta() {
        // key A = &MouseWheel(-120)
        let c = load_ok("key A = &MouseWheel(-120)");
        assert_eq!(
            only(global(&c), A).actions,
            vec![Action::Func(Func::MouseWheel(-120))]
        );
    }

    #[test]
    fn mouse_wheel_takes_one_argument() {
        assert!(
            load_err("key A = &MouseWheel(1, 2)")
                .message
                .contains("引数が多い")
        );
    }

    #[test]
    fn empty_number_argument_is_zero() {
        // key A = &MouseMove(, 2)
        let c = load_ok("key A = &MouseMove(, 2)");
        assert_eq!(
            only(global(&c), A).actions,
            vec![Action::Func(Func::MouseMove { dx: 0, dy: 2 })]
        );
    }

    #[test]
    fn empty_argument_in_the_middle_is_counted() {
        assert!(
            load_err("key A = &MouseMove(1,, 2)")
                .message
                .contains("引数が多い")
        );
    }

    #[test]
    fn empty_parentheses_are_one_argument_for_window_redraw() {
        // yamy と同じく `()` は空の引数 1 個
        assert!(
            load_err("key A = &WindowRedraw()")
                .message
                .contains("引数が多い")
        );
    }

    #[test]
    fn empty_parentheses_are_one_argument_for_recenter() {
        assert!(
            load_err("key A = &Recenter()")
                .message
                .contains("引数が多い")
        );
    }

    #[test]
    fn empty_target_is_an_error() {
        assert!(
            load_err("key A = &WindowMaximize()")
                .message
                .contains("overlapped")
        );
    }

    #[test]
    fn keymap_window_is_read() {
        // key A = &KeymapWindow
        let c = load_ok("key A = &KeymapWindow");
        assert_eq!(only(global(&c), A).actions, vec![Action::KeymapWindow]);
    }

    #[test]
    fn clipboard_case_functions_are_read() {
        // key B = &ClipboardUpcaseWord &clipboarddowncaseword
        let c = load_ok("key B = &ClipboardUpcaseWord &clipboarddowncaseword");
        assert_eq!(
            only(global(&c), B).actions,
            vec![
                Action::Func(Func::ClipboardUpcaseWord),
                Action::Func(Func::ClipboardDowncaseWord)
            ]
        );
    }

    // ---- &HelpMessage / &HelpVariable ---------------------------------------

    const HELP_SRC: &str = concat!(
        "key A = &HelpMessage(\"EmacsEdit C-x-\", \"C-x u\\tundo\")\n",
        "key B = &HelpMessage &HelpMessage() &HelpMessage(\"\", \"\")\n",
        "key H = &HelpMessage(\"T\")\n",
        "key Space = &HelpVariable(\"繰り返し\")\n",
        // 引用符の無い名前も文字列
        "key LShift = &HelpMessage(Mayu, \"再読込完了\")\n",
        // 並んだ文字列は行継続をまたいでもつなぐ
        "key BS = &HelpMessage(\"KeyMap\", \\\n \" 1 : a\\n\" \\\n \" 2 : b\" \\\n )\n",
        // 空の引数は空の文字列
        "key RShift = &HelpMessage(\"KeyMap\", \\\n )\n",
        "key LControl = &HelpVariable()\n",
    );

    fn help(title: &str, body: &str) -> Action {
        Action::HelpMessage(Some(HelpText {
            title: title.into(),
            body: body.into(),
        }))
    }

    #[test]
    fn help_message_reads_the_title_and_the_body() {
        // key A = &HelpMessage("EmacsEdit C-x-", "C-x u\tundo")
        let c = load_ok(HELP_SRC);
        assert_eq!(
            only(global(&c), A).actions,
            vec![help("EmacsEdit C-x-", "C-x u\tundo")]
        );
    }

    #[test]
    fn help_message_without_text_hides() {
        // key B = &HelpMessage &HelpMessage() &HelpMessage("", "")
        let c = load_ok(HELP_SRC);
        assert_eq!(
            only(global(&c), B).actions,
            vec![Action::HelpMessage(None); 3]
        );
    }

    #[test]
    fn help_message_with_only_a_title_has_an_empty_body() {
        // key H = &HelpMessage("T")
        let c = load_ok(HELP_SRC);
        assert_eq!(only(global(&c), H).actions, vec![help("T", "")]);
    }

    #[test]
    fn help_variable_reads_the_title() {
        // key Space = &HelpVariable("繰り返し")
        let c = load_ok(HELP_SRC);
        assert_eq!(
            only(global(&c), SPACE).actions,
            vec![Action::HelpVariable("繰り返し".into())]
        );
    }

    #[test]
    fn unquoted_name_is_a_string() {
        // key LShift = &HelpMessage(Mayu, "再読込完了")
        let c = load_ok(HELP_SRC);
        assert_eq!(
            only(global(&c), LSHIFT).actions,
            vec![help("Mayu", "再読込完了")]
        );
    }

    #[test]
    fn adjacent_strings_are_joined_across_continuation_lines() {
        let c = load_ok(HELP_SRC);
        assert_eq!(
            only(global(&c), BS).actions,
            vec![help("KeyMap", " 1 : a\n 2 : b")]
        );
    }

    #[test]
    fn empty_string_argument_is_an_empty_string() {
        let c = load_ok(HELP_SRC);
        assert_eq!(only(global(&c), RSHIFT).actions, vec![help("KeyMap", "")]);
    }

    #[test]
    fn help_variable_with_empty_parentheses_has_an_empty_title() {
        // key LControl = &HelpVariable()
        let c = load_ok(HELP_SRC);
        assert_eq!(
            only(global(&c), LCONTROL).actions,
            vec![Action::HelpVariable(String::new())]
        );
    }

    #[test]
    fn help_message_title_must_be_a_string() {
        load_err("key A = &HelpMessage(1)");
    }

    #[test]
    fn help_message_takes_at_most_two_arguments() {
        load_err("key A = &HelpMessage(\"a\", \"b\", \"c\")");
    }

    #[test]
    fn help_variable_needs_parentheses() {
        load_err("key A = &HelpVariable");
    }

    #[test]
    fn help_variable_takes_one_argument() {
        load_err("key A = &HelpVariable(\"a\", \"b\")");
    }

    // ---- &ShellExecute ------------------------------------------------------

    #[test]
    fn shell_execute_reads_five_arguments() {
        let c = load_ok("key A = &ShellExecute(open, \"notepad.exe\", \"a b\", , showMaximized)");
        assert_eq!(
            only(global(&c), A).actions,
            vec![Action::ShellExecute(ShellExecute {
                operation: "open".into(),
                file: "notepad.exe".into(),
                parameters: "a b".into(),
                directory: String::new(),
                show: ShowCommand::ShowMaximized,
            })]
        );
    }

    #[test]
    fn shell_execute_needs_a_show_command() {
        assert!(
            load_err("key A = &ShellExecute(open, \"a\", , )")
                .message
                .contains("窓の出し方")
        );
    }

    #[test]
    fn shell_execute_needs_the_third_argument() {
        assert!(
            load_err("key A = &ShellExecute(open, \"a\")")
                .message
                .contains("3 番目")
        );
    }

    #[test]
    fn shell_execute_with_an_unknown_show_command_is_an_error() {
        assert!(
            load_err("key A = &ShellExecute(open, \"a\", , , Big)")
                .message
                .contains("窓の出し方が不明")
        );
    }

    #[test]
    fn shell_execute_does_not_read_argument_substitution() {
        load_err("key A = &ShellExecute(open, $Clipboard, , , ShowNormal)");
    }

    #[test]
    fn shell_execute_takes_at_most_five_arguments() {
        load_err("key A = &ShellExecute(open, \"a\", , , ShowNormal, 1)");
    }

    // ---- &DescribeBindings の文字列 -------------------------------------------

    const DESCRIBE_SRC: &str = "keyseq $Seq = C-BS\n\
             keymap Sub = &Ignore\n\
             keymap Global\n\
             key A = *BS $Seq ( H Space ) &WindowClingToLeft &Repeat($Seq) &DescribeBindings\n\
             key B = &EmacsEditKillLinePred(( H ), $Seq) &Prefix(Sub)\n\
             event prefixed = &Sync";

    #[test]
    fn describe_text_of_an_assignment_is_made_while_loading() {
        let c = load_ok(DESCRIBE_SRC);
        assert_eq!(
            only(global(&c), A).text,
            "BackSpace $Seq $ &WindowClingToLeft(overlapped)  &Repeat($Seq, 10)  &DescribeBindings "
        );
    }

    #[test]
    fn describe_text_names_the_sequence_arguments() {
        let c = load_ok(DESCRIBE_SRC);
        assert_eq!(
            only(global(&c), B).text,
            "&EmacsEditKillLinePred($, $Seq)  &Prefix(Sub, true)  "
        );
    }

    #[test]
    fn describe_text_of_an_event_is_made_while_loading() {
        let c = load_ok(DESCRIBE_SRC);
        assert_eq!(
            global(&c).event_texts[EventKind::Prefixed.index()],
            "&Sync "
        );
    }

    #[test]
    fn describe_text_of_a_default_action_is_made_while_loading() {
        let c = load_ok(DESCRIBE_SRC);
        assert_eq!(c.keymaps[1].default_text, "&Ignore ");
        assert_eq!(global(&c).default_text, "&OtherWindowClass ");
    }

    #[test]
    fn key_name_is_the_first_name_of_def_key() {
        let c = load_ok(DESCRIBE_SRC);
        assert_eq!(c.key_name(BS), "BackSpace");
    }

    #[test]
    fn describe_bindings_is_read() {
        let c = load_ok(DESCRIBE_SRC);
        assert_eq!(
            only(global(&c), A).actions.last(),
            Some(&Action::DescribeBindings)
        );
    }

    // ---- 調査・設定・ログ --------------------------------------------------------

    const INVESTIGATE_SRC: &str = "key A = &WindowIdentify &MayuDialog(Log, SHOW)\n\
             key B = &InvestigateCommand\n\
             key H = &LoadSetting &LoadSetting(\"109 Emacs\")";

    #[test]
    fn window_identify_is_read() {
        // key A = &WindowIdentify &MayuDialog(Log, SHOW)
        let c = load_ok(INVESTIGATE_SRC);
        assert_eq!(
            only(global(&c), A).actions,
            vec![
                Action::Func(Func::WindowIdentify),
                Action::MayuDialog(ShowCommand::Show)
            ]
        );
    }

    #[test]
    fn investigate_command_is_read() {
        // key B = &InvestigateCommand
        let c = load_ok(INVESTIGATE_SRC);
        assert_eq!(
            only(global(&c), B).actions,
            vec![Action::InvestigateCommand]
        );
    }

    #[test]
    fn load_setting_reads_an_optional_name() {
        // key H = &LoadSetting &LoadSetting("109 Emacs")
        let c = load_ok(INVESTIGATE_SRC);
        assert_eq!(
            only(global(&c), H).actions,
            vec![
                Action::LoadSetting(String::new()),
                Action::LoadSetting("109 Emacs".into())
            ]
        );
    }

    #[test]
    fn load_setting_takes_at_most_one_argument() {
        assert!(
            load_err("key A = &LoadSetting(a, b)")
                .message
                .contains("引数が多い")
        );
    }

    #[test]
    fn window_identify_takes_no_argument() {
        assert!(
            load_err("key A = &WindowIdentify()")
                .message
                .contains("引数が多い")
        );
    }

    const DIALOG_SRC: &str = "key A = &MayuDialog(Log, showNormal)\n\
             key B = &mayudialog(LOG, Hide)\n\
             key H = &LogClear";

    #[test]
    fn mayu_dialog_reads_the_show_command() {
        // key A = &MayuDialog(Log, showNormal)
        let c = load_ok(DIALOG_SRC);
        assert_eq!(
            only(global(&c), A).actions,
            vec![Action::MayuDialog(ShowCommand::ShowNormal)]
        );
    }

    #[test]
    fn mayu_dialog_ignores_case() {
        // key B = &mayudialog(LOG, Hide)
        let c = load_ok(DIALOG_SRC);
        assert_eq!(
            only(global(&c), B).actions,
            vec![Action::MayuDialog(ShowCommand::Hide)]
        );
    }

    #[test]
    fn log_clear_is_read() {
        // key H = &LogClear
        let c = load_ok(DIALOG_SRC);
        assert_eq!(only(global(&c), H).actions, vec![Action::LogClear]);
    }

    #[test]
    fn mayu_dialog_investigate_is_not_supported() {
        assert!(
            load_err("key A = &MayuDialog(Investigate, Show)")
                .message
                .contains("未対応")
        );
    }

    #[test]
    fn mayu_dialog_with_an_unknown_dialog_is_an_error() {
        assert!(
            load_err("key A = &MayuDialog(Foo, Show)")
                .message
                .contains("ダイアログが不明")
        );
    }

    #[test]
    fn mayu_dialog_needs_a_show_command() {
        assert!(
            load_err("key A = &MayuDialog(Log)")
                .message
                .contains("窓の出し方")
        );
    }

    #[test]
    fn mayu_dialog_needs_a_dialog_name() {
        load_err("key A = &MayuDialog()");
    }

    #[test]
    fn mayu_dialog_takes_at_most_two_arguments() {
        load_err("key A = &MayuDialog(Log, Show, 1)");
    }

    // ---- &EmacsEditKillLine… / &Sync / &EditNextModifier / &Prefix ------------

    #[test]
    fn emacs_edit_kill_line_reads_two_sequences() {
        let c = load_ok(
            "keyseq $Del = BS
             key A = &EmacsEditKillLineFunc &EmacsEditKillLinePred($Del, (Space H))",
        );
        assert_eq!(
            only(global(&c), A).actions,
            vec![
                Action::EmacsEditKillLineFunc,
                Action::EmacsEditKillLinePred {
                    seq1: vec![in_keyseq(BS, base())],
                    seq2: vec![Action::key_with(SPACE, base()), Action::key_with(H, base())],
                },
            ]
        );
    }

    #[test]
    fn emacs_edit_kill_line_pred_needs_two_sequences() {
        assert!(
            load_err("key A = &EmacsEditKillLinePred((BS))")
                .message
                .contains("`,`")
        );
    }

    #[test]
    fn emacs_edit_kill_line_pred_argument_must_be_a_sequence() {
        assert!(
            load_err("key A = &EmacsEditKillLinePred(BS, (H))")
                .message
                .contains("1 番目の引数")
        );
    }

    #[test]
    fn def_sync_reads_the_scan_code() {
        // def sync = E0-0x7e
        let c = load_ok("def sync = E0-0x7e\nkey A = &Sync &WindowMaximize");
        assert_eq!(c.sync, Some(Key::e0(0x7e)));
    }

    #[test]
    fn sync_is_read() {
        // key A = &Sync &WindowMaximize
        let c = load_ok("def sync = E0-0x7e\nkey A = &Sync &WindowMaximize");
        assert_eq!(only(global(&c), A).actions[0], Action::Sync);
    }

    #[test]
    fn sync_key_is_none_without_def_sync() {
        let c = load_ok("key A = &Sync");
        assert_eq!(c.sync, None);
    }

    const EDIT_NEXT_SRC: &str = "key A = &EditNextModifier(M-)
             key B = &EditNextModifier(~S-C-IC-)
             key H = &EditNextModifier()";

    #[test]
    fn edit_next_modifier_reads_a_modifier() {
        // key A = &EditNextModifier(M-)
        let c = load_ok(EDIT_NEXT_SRC);
        assert_eq!(
            only(global(&c), A).actions,
            vec![Action::EditNextModifier(
                ModSpec::any().with(BasicMod::Alt),
                LockSpec::any()
            )]
        );
    }

    #[test]
    fn edit_next_modifier_reads_release_and_lock_prefixes() {
        // key B = &EditNextModifier(~S-C-IC-)
        let c = load_ok(EDIT_NEXT_SRC);
        assert_eq!(
            only(global(&c), B).actions,
            vec![Action::EditNextModifier(
                ModSpec::any()
                    .without(BasicMod::Shift)
                    .with(BasicMod::Control),
                LockSpec::any().with(LockMod::ImeComp)
            )]
        );
    }

    #[test]
    fn edit_next_modifier_without_a_modifier_changes_nothing() {
        // key H = &EditNextModifier()
        let c = load_ok(EDIT_NEXT_SRC);
        assert_eq!(
            only(global(&c), H).actions,
            vec![Action::EditNextModifier(ModSpec::any(), LockSpec::any())]
        );
    }

    #[test]
    fn describe_text_of_edit_next_modifier_uses_a_for_alt() {
        let c = load_ok(EDIT_NEXT_SRC);
        assert_eq!(only(global(&c), A).text, "&EditNextModifier(A-)  ");
    }

    #[test]
    fn edit_next_modifier_argument_must_be_a_modifier() {
        assert!(
            load_err("key A = &EditNextModifier(Foo)")
                .to_string()
                .contains("修飾が要る所")
        );
    }

    const PREFIX_SRC: &str = "keymap Sub
             key A = &Prefix(Sub, False)
             key B = &Prefix(Sub, true)
             key H = &Prefix(Sub)";

    #[test]
    fn prefix_reads_false_ignoring_case() {
        // key A = &Prefix(Sub, False)
        let c = load_ok(PREFIX_SRC);
        assert_eq!(
            only(&c.keymaps[1], A).actions,
            vec![Action::Prefix(KeymapId(1), false)]
        );
    }

    #[test]
    fn prefix_reads_true() {
        // key B = &Prefix(Sub, true)
        let c = load_ok(PREFIX_SRC);
        assert_eq!(
            only(&c.keymaps[1], B).actions,
            vec![Action::Prefix(KeymapId(1), true)]
        );
    }

    #[test]
    fn prefix_ignore_modifiers_defaults_to_true() {
        // key H = &Prefix(Sub)
        let c = load_ok(PREFIX_SRC);
        assert_eq!(
            only(&c.keymaps[1], H).actions,
            vec![Action::Prefix(KeymapId(1), true)]
        );
    }

    #[test]
    fn prefix_ignore_modifiers_must_be_true_or_false() {
        assert!(
            load_err(
                "keymap Sub
key A = &Prefix(Sub, yes)"
            )
            .to_string()
            .contains("true か false")
        );
    }

    // ---- event / &Variable / &Repeat ----------------------------------------

    const EVENT_SRC: &str = "keyseq $seq = Space
             keymap Sub
             event prefixed = H
             event Before-Key-Down = B
             key A = &Variable(10, 3) &Prefix(Sub)
             key B = &Repeat((H), 100) &Variable(0, 0)
             key H = &Repeat($seq)";

    #[test]
    fn event_prefixed_is_read() {
        // event prefixed = H
        let c = load_ok(EVENT_SRC);
        assert_eq!(
            c.keymaps[1].event(EventKind::Prefixed),
            Some(&[Action::key_with(H, base())][..])
        );
    }

    #[test]
    fn event_name_ignores_case() {
        // event Before-Key-Down = B
        let c = load_ok(EVENT_SRC);
        assert_eq!(
            c.keymaps[1].event(EventKind::BeforeKeyDown),
            Some(&[Action::key_with(B, base())][..])
        );
    }

    #[test]
    fn event_that_is_not_written_is_none() {
        let c = load_ok(EVENT_SRC);
        assert_eq!(c.keymaps[1].event(EventKind::AfterKeyUp), None);
        assert_eq!(global(&c).event(EventKind::Prefixed), None);
    }

    #[test]
    fn variable_reads_mag_and_inc() {
        // key A = &Variable(10, 3) &Prefix(Sub)
        let c = load_ok(EVENT_SRC);
        assert_eq!(
            only(&c.keymaps[1], A).actions,
            vec![
                Action::Variable { mag: 10, inc: 3 },
                Action::Prefix(KeymapId(1), true)
            ]
        );
    }

    #[test]
    fn repeat_reads_the_sequence_and_the_max() {
        // key B = &Repeat((H), 100) &Variable(0, 0)
        let c = load_ok(EVENT_SRC);
        assert_eq!(
            only(&c.keymaps[1], B).actions,
            vec![
                Action::Repeat {
                    actions: vec![Action::key_with(H, base())],
                    max: 100
                },
                Action::Variable { mag: 0, inc: 0 }
            ]
        );
    }

    #[test]
    fn repeat_max_defaults_to_10() {
        // key H = &Repeat($seq)
        let c = load_ok(EVENT_SRC);
        assert_eq!(
            only(&c.keymaps[1], H).actions,
            vec![Action::Repeat {
                actions: vec![in_keyseq(SPACE, base())],
                max: 10
            }]
        );
    }

    #[test]
    fn unknown_event_is_an_error() {
        assert!(
            load_err("event foo = A")
                .to_string()
                .contains("知らない event")
        );
    }

    #[test]
    fn repeat_argument_must_be_a_sequence() {
        assert!(
            load_err("key A = &Repeat(A)")
                .to_string()
                .contains("&Repeat の 1 番目")
        );
    }

    #[test]
    fn variable_needs_two_arguments() {
        assert!(
            load_err("key A = &Variable(1)")
                .to_string()
                .contains("&Variable")
        );
    }

    // ---- &VK / &PostMessage -------------------------------------------------

    const VK_SRC: &str = "key A = &VK(RButton)
             key B = &VK(E-D-Return) &VK(U-0x41)
             key H = &PostMessage(ToItself, 277, 2, 0)
             key Space = &PostMessage(toMainWindow, 273, 41525, -1)";

    #[test]
    fn vk_reads_a_virtual_key_name() {
        // key A = &VK(RButton)
        let c = load_ok(VK_SRC);
        assert_eq!(
            only(global(&c), A).actions,
            vec![Action::Vk {
                vk: 0x02,
                extended: false,
                down: true,
                up: true
            }]
        );
    }

    #[test]
    fn vk_reads_e_d_u_prefixes_and_a_number() {
        // key B = &VK(E-D-Return) &VK(U-0x41)
        let c = load_ok(VK_SRC);
        assert_eq!(
            only(global(&c), B).actions,
            vec![
                Action::Vk {
                    vk: 0x0d,
                    extended: true,
                    down: true,
                    up: false
                },
                Action::Vk {
                    vk: 0x41,
                    extended: false,
                    down: false,
                    up: true
                },
            ]
        );
    }

    #[test]
    fn post_message_reads_its_arguments() {
        // key H = &PostMessage(ToItself, 277, 2, 0)
        let c = load_ok(VK_SRC);
        assert_eq!(
            only(global(&c), H).actions,
            vec![Action::Func(Func::PostMessage {
                to: ToWindow::Itself,
                message: 277,
                wparam: 2,
                lparam: 0
            })]
        );
    }

    #[test]
    fn post_message_target_ignores_case() {
        // key Space = &PostMessage(toMainWindow, 273, 41525, -1)
        let c = load_ok(VK_SRC);
        assert_eq!(
            only(global(&c), SPACE).actions,
            vec![Action::Func(Func::PostMessage {
                to: ToWindow::Main,
                message: 273,
                wparam: 41525,
                lparam: -1
            })]
        );
    }

    #[test]
    fn vk_with_an_unknown_name_is_an_error() {
        assert!(
            load_err("key A = &VK(Nothing)")
                .message
                .contains("仮想キー名")
        );
    }

    #[test]
    fn vk_number_must_be_0_to_255() {
        assert!(load_err("key A = &VK(300)").message.contains("0〜255"));
    }

    #[test]
    fn vk_needs_parentheses() {
        assert!(load_err("key A = &VK").message.contains("`(`"));
    }

    #[test]
    fn post_message_needs_four_arguments() {
        assert!(
            load_err("key A = &PostMessage(ToItself, 1, 2)")
                .message
                .contains("無い")
        );
    }

    #[test]
    fn post_message_with_an_unknown_target_is_an_error() {
        assert!(
            load_err("key A = &PostMessage(Nowhere, 1, 2, 3)")
                .message
                .contains("宛先")
        );
    }

    #[test]
    fn later_assignment_wins() {
        // key A = B / key A = H
        let c = load_ok("key A = B\nkey A = H");
        let g = global(&c);
        let found = g
            .search(A, ModState::empty(), LockState::empty(), Edge::Down)
            .unwrap();
        assert_eq!(found.actions[0], Action::key_with(H, base()));
        assert_eq!(g.assignments.len(), 1);
    }

    // ---- ファイルを読ませる（Medium） --------------------------------------------

    #[test]
    fn include_reads_the_file_relative_to_the_including_file() {
        let top = format!("{KEYS}\ninclude \"sub/inner.mayu\"\nkey H = B\n");
        let c = load_files(
            "include",
            &[
                ("top.mayu", top.as_bytes()),
                ("sub/inner.mayu", b"key A = B\n"),
            ],
            &[],
        )
        .unwrap();
        let g = global(&c);
        assert_eq!(g.assignments.len(), 2);
        assert_eq!(only(g, A).actions.len(), 1);
    }

    #[test]
    fn error_in_an_included_file_names_that_file_and_line() {
        let top = format!("{KEYS}\ninclude \"sub/inner.mayu\"\nkey H = B\n");
        let e = load_files(
            "include-error",
            &[
                ("top.mayu", top.as_bytes()),
                ("sub/inner.mayu", b"key A = B\n\nkey Z = B\n"),
            ],
            &[],
        )
        .unwrap_err();
        assert!(e.file.ends_with("inner.mayu"), "{e}");
        assert_eq!(e.line, 3);
    }

    #[test]
    fn symbol_given_to_load_is_defined_ignoring_case() {
        // if ( use109 ) / key A = B / endif
        let src = format!("{KEYS}\nif ( use109 )\nkey A = B\nendif\n");
        let c = load_files(
            "symbols",
            &[("s.mayu", src.as_bytes())],
            &["USE109".to_string()],
        )
        .unwrap();
        assert_eq!(global(&c).assignments.len(), 1);
    }

    #[test]
    fn symbol_not_given_to_load_is_undefined() {
        let src = format!("{KEYS}\nif ( use109 )\nkey A = B\nendif\n");
        let c = load_files("no-symbols", &[("s.mayu", src.as_bytes())], &[]).unwrap();
        assert_eq!(global(&c).assignments.len(), 0);
    }

    /// `key 無変換 = B`（無変換 = 0x7b）が読めたか。
    fn muhenkan_is_b(c: &Config) -> bool {
        let a = only(global(c), Key::plain(0x7b));
        matches!(a.actions.as_slice(), [Action::Key { key, .. }] if *key == B)
    }

    #[test]
    fn utf8_file_is_read() {
        let src = format!("{KEYS}\ndef key 無変換 = 0x7b\nkey 無変換 = B\n");
        let c = load_files("utf8", &[("utf8.mayu", src.as_bytes())], &[]).unwrap();
        assert!(muhenkan_is_b(&c));
    }

    #[test]
    fn utf8_file_with_bom_is_read() {
        let src = format!("{KEYS}\ndef key 無変換 = 0x7b\nkey 無変換 = B\n");
        let bytes = [b"\xEF\xBB\xBF".as_slice(), src.as_bytes()].concat();
        let c = load_files("bom", &[("bom.mayu", &bytes)], &[]).unwrap();
        assert!(muhenkan_is_b(&c));
    }

    #[test]
    fn cp932_file_is_read() {
        // "無変換".encode("shift_jis") = 96 b3 95 cf 8a b7
        let bytes = [
            format!("{KEYS}\ndef key ").as_bytes(),
            b"\x96\xb3\x95\xcf\x8a\xb7",
            b" = 0x7b\nkey ",
            b"\x96\xb3\x95\xcf\x8a\xb7",
            b" = B\n",
        ]
        .concat();
        let c = load_files("cp932", &[("cp932.mayu", &bytes)], &[]).unwrap();
        assert!(muhenkan_is_b(&c));
    }

    #[test]
    fn file_in_neither_encoding_is_an_error_with_the_line() {
        // どちらでもない: エラー（UTF-8 として読めなくなった行番号付き）
        let e =
            load_files("bad-encoding", &[("bad.mayu", b"# ok\n# \xfe\xff\n")], &[]).unwrap_err();
        assert_eq!(e.line, 2);
        assert!(e.message.contains("文字コード"), "{e}");
    }

    // ---- def key のスキャンコード ----------------------------------------------

    #[test]
    fn def_key_reads_multiple_scan_codes() {
        // def key Pause = E1-0x1d 0x45 / key Pause = A
        let c = load_ok("def key Pause = E1-0x1d 0x45\nkey Pause = A");
        let pause = Key::from_codes(&[
            ScanCode::new(ScanPrefix::E1, 0x1d),
            ScanCode::new(ScanPrefix::None, 0x45),
        ])
        .unwrap();
        assert_eq!(
            only(global(&c), pause).actions,
            vec![Action::key_with(A, base())]
        );
        assert!(c.keys.contains(&pause));
        assert!(c.keys.contains(&A));
    }

    #[test]
    fn def_key_reads_up_to_four_scan_codes() {
        // yamy と同じく最大 4 個
        load_ok("def key Foo = 0x01 0x02 0x03 0x04");
    }

    #[test]
    fn def_key_with_five_scan_codes_is_an_error() {
        load_err("def key Foo = 0x01 0x02 0x03 0x04 0x05");
    }

    // ---- エラー -------------------------------------------------------------

    #[test]
    fn undefined_key_is_an_error_with_the_line() {
        let e = load_err("key Z = A");
        assert!(e.message.contains("未定義"));
        assert_eq!(e.line, KEYS.lines().count() + 1);
    }

    #[test]
    fn window_monitor_is_not_supported() {
        assert!(
            load_err("key A = &WindowMonitor(1)")
                .message
                .contains("未対応")
        );
    }

    #[test]
    fn il_prefix_on_an_output_key_is_an_error() {
        assert!(load_err("key A = IL-B").message.contains("出力キー"));
    }

    #[test]
    fn ic_prefix_on_an_output_key_is_an_error() {
        assert!(load_err("key A = IC-B").message.contains("出力キー"));
    }

    #[test]
    fn max_prefix_is_not_supported() {
        assert!(load_err("key MAX-A = B").message.contains("未対応"));
    }

    #[test]
    fn bang_before_a_modifier_name_is_not_supported() {
        assert!(load_err("mod !shift = A").message.contains("未対応"));
    }

    #[test]
    fn undefined_keyseq_is_an_error() {
        assert!(load_err("key A = $Nothing").message.contains("未定義"));
    }

    #[test]
    fn undefined_parent_keymap_is_an_error() {
        assert!(load_err("keymap X : Nowhere").message.contains("未定義"));
    }

    #[test]
    fn defining_a_key_name_twice_is_an_error() {
        assert!(load_err("def key A = 0x01").message.contains("定義済み"));
    }

    #[test]
    fn changing_the_default_modifiers_is_not_supported() {
        assert!(load_err("key C- = B").message.contains("未対応"));
    }

    #[test]
    fn def_subst_right_side_must_start_with_a_key() {
        assert!(
            load_err("def subst A = &Default")
                .message
                .contains("キーでない")
        );
    }

    // ---- def subst ----------------------------------------------------------

    const SUBST_SRC: &str = "keyseq $SB = S-*B A
             def subst S-*A = $SB
             def subst H Space => IC-BS
             def subst D-LAlt = B
             def subst *H = A";

    #[test]
    fn each_left_key_of_def_subst_makes_a_substitution() {
        let c = load_ok(SUBST_SRC);
        assert_eq!(c.substs.len(), 5);
    }

    #[test]
    fn later_def_subst_is_matched_first() {
        // def subst *H = A（最後に書いた）
        let c = load_ok(SUBST_SRC);
        let s = &c.substs;
        assert_eq!((s[0].key, s[0].to_key), (H, A));
    }

    #[test]
    fn star_in_def_subst_makes_all_conditions_dontcare() {
        // def subst *H = A
        let c = load_ok(SUBST_SRC);
        let s = &c.substs;
        assert_eq!(s[0].mods, ModSpec::any());
        assert_eq!(s[0].locks, LockSpec::any());
    }

    #[test]
    fn d_prefix_in_def_subst_is_key_down_only() {
        // def subst D-LAlt = B
        let c = load_ok(SUBST_SRC);
        assert_eq!(c.substs[1].edge, EdgeSpec::Down);
    }

    #[test]
    fn left_keys_of_def_subst_share_the_replacement() {
        // def subst H Space => IC-BS
        let c = load_ok(SUBST_SRC);
        let s = &c.substs;
        assert_eq!((s[2].key, s[3].key), (SPACE, H));
        assert_eq!(s[2].to_key, BS);
        assert_eq!(s[3].to_key, BS);
    }

    #[test]
    fn left_side_of_def_subst_has_the_default_conditions() {
        // def subst H Space => IC-BS
        let c = load_ok(SUBST_SRC);
        let s = &c.substs;
        assert_eq!(s[2].mods, base());
        assert_eq!(s[2].locks, LockSpec::assign_default());
        assert_eq!(s[3].mods, base());
        assert_eq!(s[3].locks, LockSpec::assign_default());
    }

    #[test]
    fn right_side_of_def_subst_takes_ic() {
        // def subst H Space => IC-BS
        let c = load_ok(SUBST_SRC);
        let s = &c.substs;
        assert_eq!(
            s[2].to_locks,
            LockSpec::assign_default().with(LockMod::ImeComp)
        );
        assert_eq!(
            s[3].to_locks,
            LockSpec::assign_default().with(LockMod::ImeComp)
        );
    }

    #[test]
    fn def_subst_takes_the_first_key_of_a_keyseq() {
        // keyseq $SB = S-*B A / def subst S-*A = $SB
        let c = load_ok(SUBST_SRC);
        let s = &c.substs;
        assert_eq!(s[4].key, A);
        assert_eq!(s[4].mods, ModSpec::any().with(BasicMod::Shift));
        assert_eq!(s[4].to_key, B);
        assert_eq!(s[4].to_mods, ModSpec::any().with(BasicMod::Shift));
        assert_eq!(s[4].to_locks, LockSpec::any());
    }

    #[test]
    fn keyseq_takes_il_and_ic() {
        // keyseq にも IL- IC- を書ける（出力には効かない）
        load_ok("keyseq $X = IL-A\nkey B = $X");
    }

    #[test]
    fn def_alias_names_a_key() {
        // def alias Esc = H
        // key esc = B
        let c = load_ok("def alias Esc = H\nkey esc = B");
        assert_eq!(
            only(global(&c), H).actions,
            vec![Action::key_with(B, base())]
        );
    }

    #[test]
    fn def_alias_cannot_point_to_an_alias() {
        // def alias X = H
        // def alias Y = X   （yamy の searchKeyByNonAliasName）
        let e = load_err("def alias X = H\ndef alias Y = X");
        assert!(e.message.contains("別名は使えない"), "{e}");
    }

    #[test]
    fn mod_mod0_assigns_the_key() {
        // mod mod0 = 英数
        let c = load_ok("mod mod0 = 英数");
        assert_eq!(global(&c).mod_keys[BasicMod::Mod0.index()], vec![EISUU]);
    }

    #[test]
    fn m0_prefix_requires_mod0() {
        // key M0-A = B
        let c = load_ok("mod mod0 = 英数\nkey M0-A = B");
        assert_eq!(only(global(&c), A).mods, base().with(BasicMod::Mod0));
    }

    #[test]
    fn elsif_is_else_if() {
        let c = load_ok(
            "define KBD109
             if ( KBD104 )
               key A = B
             elsif ( KBD109 )
               key H = B
             endif",
        );
        let keys: Vec<Key> = global(&c).assignments.iter().map(|a| a.key).collect();
        assert_eq!(keys, vec![H]);
    }

    #[test]
    fn elif_is_else_if() {
        let c = load_ok(
            "define KBD109
             if ( KBD104 )
               key A = B
             elif ( KBD109 )
               key H = B
             endif",
        );
        let keys: Vec<Key> = global(&c).assignments.iter().map(|a| a.key).collect();
        assert_eq!(keys, vec![H]);
    }

    #[test]
    fn or_is_else_if() {
        let c = load_ok(
            "define KBD109
             if ( KBD104 )
               key A = B
             or ( KBD109 )
               key H = B
             endif",
        );
        let keys: Vec<Key> = global(&c).assignments.iter().map(|a| a.key).collect();
        assert_eq!(keys, vec![H]);
    }

    #[test]
    fn keymap2_default_action_is_keymap_parent() {
        // keymap2 Sub : Global
        // yamy の load_KEYMAP_DEFINITION は keymap2 でも &KeymapParent を入れる
        let c = load_ok("keymap2 Sub : Global");
        assert_eq!(c.keymaps[1].default_actions, vec![Action::KeymapParent]);
    }

    #[test]
    fn window_minimize_takes_a_target() {
        // key A = &WindowMinimize(MDI)
        let c = load_ok("key A = &WindowMinimize(MDI)");
        assert_eq!(
            only(global(&c), A).actions,
            vec![Action::Func(Func::WindowMinimize(Target::Mdi))]
        );
    }

    #[test]
    fn window_h_maximize_takes_a_target() {
        // key A = &WindowHMaximize(MDI)
        let c = load_ok("key A = &WindowHMaximize(MDI)");
        assert_eq!(
            only(global(&c), A).actions,
            vec![Action::Func(Func::WindowHMaximize(Target::Mdi))]
        );
    }

    #[test]
    fn window_v_maximize_takes_a_target() {
        // key A = &WindowVMaximize(MDI)
        let c = load_ok("key A = &WindowVMaximize(MDI)");
        assert_eq!(
            only(global(&c), A).actions,
            vec![Action::Func(Func::WindowVMaximize(Target::Mdi))]
        );
    }

    #[test]
    fn window_move_visibly_takes_a_target() {
        // key A = &WindowMoveVisibly(MDI)
        let c = load_ok("key A = &WindowMoveVisibly(MDI)");
        assert_eq!(
            only(global(&c), A).actions,
            vec![Action::Func(Func::WindowMoveVisibly(Target::Mdi))]
        );
    }

    #[test]
    fn window_close_takes_a_target() {
        // key A = &WindowClose(MDI)
        let c = load_ok("key A = &WindowClose(MDI)");
        assert_eq!(
            only(global(&c), A).actions,
            vec![Action::Func(Func::WindowClose(Target::Mdi))]
        );
    }

    #[test]
    fn window_cling_to_right_is_move_to_east() {
        // key A = &WindowClingToRight
        let c = load_ok("key A = &WindowClingToRight");
        assert_eq!(
            only(global(&c), A).actions,
            vec![Action::Func(Func::WindowMoveTo {
                gravity: Gravity::East,
                dx: 0,
                dy: 0,
                target: Target::Overlapped
            })]
        );
    }

    #[test]
    fn window_cling_to_top_is_move_to_north() {
        // key A = &WindowClingToTop
        let c = load_ok("key A = &WindowClingToTop");
        assert_eq!(
            only(global(&c), A).actions,
            vec![Action::Func(Func::WindowMoveTo {
                gravity: Gravity::North,
                dx: 0,
                dy: 0,
                target: Target::Overlapped
            })]
        );
    }
}

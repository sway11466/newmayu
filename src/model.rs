//! キーマップのデータ構造。
//!
//! `.mayu` を読み込んだ結果はこのモジュールの型に落ちる。エンジンはこの型だけを見る。
//! 修飾の表現は yamy に倣い「押下」「dontcare」の 2 本のビット列で持つ（照合が 1 行で済む）。
//! yamy が一つの列挙に混ぜていた基本修飾（C- S- A- W- M0-9）とキーシーケンス修飾（D- U-）は
//! 型を分ける。割当修飾のうち鍵の状態で決まるもの（IL- IC-）は `LockMod` として別に持つ。
//! `window` 文の窓の条件は `WindowMatch`。

use regex::{Regex, RegexBuilder};

/// スキャンコードの前置バイト。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ScanPrefix {
    None,
    E0,
    /// yamy ではマウスボタン等の疑似キーにも使う。
    E1,
}

/// スキャンコード 1 個。前置バイトとコードの組。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ScanCode {
    pub prefix: ScanPrefix,
    pub code: u8,
}

impl ScanCode {
    pub const fn new(prefix: ScanPrefix, code: u8) -> Self {
        ScanCode { prefix, code }
    }
}

/// 物理キー。スキャンコードの列で同一性を取る。たいていは 1 個、Pause は
/// `E1-0x1d 0x45` の 2 個。最大 4 個（yamy の `Key::MAX_SCAN_CODES_SIZE`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Key {
    /// 先頭 `len` 個が使われる。残りは `ScanCode::new(ScanPrefix::None, 0)` で埋めて、
    /// 導出した比較が列の比較と一致するようにする。
    codes: [ScanCode; Key::MAX_SCAN_CODES],
    len: u8,
}

impl Key {
    pub const MAX_SCAN_CODES: usize = 4;

    const EMPTY: ScanCode = ScanCode::new(ScanPrefix::None, 0);

    /// スキャンコード 1 個のキー。
    pub const fn new(prefix: ScanPrefix, code: u8) -> Self {
        Key {
            codes: [
                ScanCode::new(prefix, code),
                Key::EMPTY,
                Key::EMPTY,
                Key::EMPTY,
            ],
            len: 1,
        }
    }
    pub const fn plain(code: u8) -> Self {
        Key::new(ScanPrefix::None, code)
    }
    pub const fn e0(code: u8) -> Self {
        Key::new(ScanPrefix::E0, code)
    }

    /// スキャンコードの列のキー。1〜4 個でなければ `None`。
    pub fn from_codes(codes: &[ScanCode]) -> Option<Self> {
        if codes.is_empty() || codes.len() > Key::MAX_SCAN_CODES {
            return None;
        }
        let mut key = Key {
            codes: [Key::EMPTY; Key::MAX_SCAN_CODES],
            len: codes.len() as u8,
        };
        key.codes[..codes.len()].copy_from_slice(codes);
        Some(key)
    }

    pub fn codes(&self) -> &[ScanCode] {
        &self.codes[..self.len as usize]
    }

    /// 後ろに `other` のスキャンコードを足したキー。4 個を超えるなら `None`。
    pub fn concat(&self, other: Key) -> Option<Key> {
        let mut codes = self.codes().to_vec();
        codes.extend_from_slice(other.codes());
        Key::from_codes(&codes)
    }

    /// `prefix` のスキャンコードの列がこのキーの先頭部分か（yamy の `Key::isPrefixScanCode`）。
    pub fn starts_with(&self, prefix: Key) -> bool {
        self.codes().starts_with(prefix.codes())
    }
}

/// 基本修飾。`key C-S-A = ...` の `C-` `S-` と、ユーザー定義の `M0-`〜`M9-`。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BasicMod {
    Shift,
    Alt,
    Control,
    Windows,
    Mod0,
    Mod1,
    Mod2,
    Mod3,
    Mod4,
    Mod5,
    Mod6,
    Mod7,
    Mod8,
    Mod9,
}

impl BasicMod {
    pub const ALL: [BasicMod; 14] = [
        BasicMod::Shift,
        BasicMod::Alt,
        BasicMod::Control,
        BasicMod::Windows,
        BasicMod::Mod0,
        BasicMod::Mod1,
        BasicMod::Mod2,
        BasicMod::Mod3,
        BasicMod::Mod4,
        BasicMod::Mod5,
        BasicMod::Mod6,
        BasicMod::Mod7,
        BasicMod::Mod8,
        BasicMod::Mod9,
    ];

    pub const fn index(self) -> usize {
        self as usize
    }

    const fn bit(self) -> u16 {
        1 << self.index()
    }
}

/// 基本修飾の集合。「いま押されている修飾」を表す。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ModState(u16);

impl ModState {
    pub const fn empty() -> Self {
        ModState(0)
    }
    pub fn with(mut self, m: BasicMod) -> Self {
        self.set(m, true);
        self
    }
    pub fn set(&mut self, m: BasicMod, on: bool) {
        if on {
            self.0 |= m.bit();
        } else {
            self.0 &= !m.bit();
        }
    }
    pub const fn contains(self, m: BasicMod) -> bool {
        self.0 & m.bit() != 0
    }
}

/// 基本修飾の条件。割付の左辺（`C-S-A`）と、出力キーの修飾（`*LControl`）の両方に使う。
///
/// - `pressed` : 押されているべき修飾
/// - `dontcare`: 問わない修飾（`*` は全修飾を dontcare にする）
///
/// `dontcare` でも `pressed` でもないビットは「離されているべき」。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ModSpec {
    pressed: u16,
    dontcare: u16,
}

impl ModSpec {
    /// 修飾なし（全修飾が離されていること）。
    pub const fn none() -> Self {
        ModSpec {
            pressed: 0,
            dontcare: 0,
        }
    }
    /// `*`: 全修飾を問わない。
    pub const fn any() -> Self {
        ModSpec {
            pressed: 0,
            dontcare: (1 << BasicMod::ALL.len()) - 1,
        }
    }
    /// `C-` のように押下を要求する。
    pub fn with(mut self, m: BasicMod) -> Self {
        self.pressed |= m.bit();
        self.dontcare &= !m.bit();
        self
    }
    /// `~C-` のように非押下を要求する。
    pub fn without(mut self, m: BasicMod) -> Self {
        self.pressed &= !m.bit();
        self.dontcare &= !m.bit();
        self
    }
    /// この修飾を問わない。
    pub fn dontcare(mut self, m: BasicMod) -> Self {
        self.dontcare |= m.bit();
        self
    }

    pub const fn is_dontcare(self, m: BasicMod) -> bool {
        self.dontcare & m.bit() != 0
    }
    pub const fn is_pressed(self, m: BasicMod) -> bool {
        self.pressed & m.bit() != 0
    }

    /// 現在の修飾状態がこの条件を満たすか（yamy の `Modifier::doesMatch`）。
    pub const fn matches(self, state: ModState) -> bool {
        (self.pressed | self.dontcare) == (state.0 | self.dontcare)
    }

    /// care の修飾をこの条件の値に、dontcare の修飾を `state` の値にした状態。
    /// `def subst` の置き換え先の修飾を入力に重ねるときに使う。
    pub const fn apply(self, state: ModState) -> ModState {
        ModState((state.0 & self.dontcare) | (self.pressed & !self.dontcare))
    }

    /// 修飾状態をそのまま条件にする（全修飾を care）。
    pub const fn from_state(state: ModState) -> ModSpec {
        ModSpec {
            pressed: state.0,
            dontcare: 0,
        }
    }

    /// 自分が dontcare で `other` が care のビットを `other` の値で埋める（yamy の `Modifier::add`）。
    /// 出力キーの修飾を決めるときに使う。`*LControl` なら現在の修飾がそのまま乗る。
    /// 両方 dontcare のビットは dontcare のまま残る。
    pub const fn add(self, other: ModSpec) -> ModSpec {
        let fill = self.dontcare & !other.dontcare;
        ModSpec {
            pressed: self.pressed | (other.pressed & fill),
            dontcare: self.dontcare & !fill,
        }
    }
}

/// キーの押し／離し。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Edge {
    Down,
    Up,
}

impl Edge {
    pub const fn is_down(self) -> bool {
        matches!(self, Edge::Down)
    }
}

/// キーシーケンス修飾（`D-` `U-`）。指定が無ければ押し・離しの両方に対応する。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum EdgeSpec {
    #[default]
    Both,
    Down,
    Up,
}

impl EdgeSpec {
    pub const fn matches(self, edge: Edge) -> bool {
        matches!(
            (self, edge),
            (EdgeSpec::Both, _) | (EdgeSpec::Down, Edge::Down) | (EdgeSpec::Up, Edge::Up)
        )
    }
}

/// 鍵の状態で決まる割当修飾。割付の左辺にだけ書ける（出力キーには付かない）。
/// yamy の `m_currentLock` に当たる。いまは IME の 2 つ。NL- CL- MAX- などもここに増える。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LockMod {
    /// `IL-`: IME がオン。
    ImeLock,
    /// `IC-` `I-`: IME で変換中（未確定文字がある）。
    ImeComp,
    /// `NL-`: NumLock。
    NumLock,
    /// `CL-`: CapsLock。
    CapsLock,
    /// `SL-`: ScrollLock。
    ScrollLock,
    /// `KL-`: カナロック。
    KanaLock,
}

impl LockMod {
    pub const ALL: [LockMod; 6] = [
        LockMod::ImeLock,
        LockMod::ImeComp,
        LockMod::NumLock,
        LockMod::CapsLock,
        LockMod::ScrollLock,
        LockMod::KanaLock,
    ];

    pub const fn index(self) -> usize {
        self as usize
    }

    const fn bit(self) -> u8 {
        1 << self.index()
    }
}

/// 鍵の現在の状態。エンジンが外（フック層）から受け取る。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct LockState(u8);

impl LockState {
    pub const fn empty() -> Self {
        LockState(0)
    }
    pub fn set(&mut self, m: LockMod, on: bool) {
        if on {
            self.0 |= m.bit();
        } else {
            self.0 &= !m.bit();
        }
    }
    pub const fn contains(self, m: LockMod) -> bool {
        self.0 & m.bit() != 0
    }
}

/// 鍵の条件。`ModSpec` と同じく「オン」「dontcare」の 2 本のビット列。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct LockSpec {
    on: u8,
    dontcare: u8,
}

impl LockSpec {
    /// 全部オフであること。
    pub const fn none() -> Self {
        LockSpec { on: 0, dontcare: 0 }
    }
    /// 全部問わない。
    pub const fn any() -> Self {
        LockSpec {
            on: 0,
            dontcare: (1 << LockMod::ALL.len()) - 1,
        }
    }
    /// yamy の割付の既定: IME 変換中でないことを要求し、他は問わない。
    /// `key A = B` は変換中には効かず、効かせるには `*IC-` を付ける。
    pub const fn assign_default() -> Self {
        LockSpec::any().without(LockMod::ImeComp)
    }
    /// `IC-` のようにオンを要求する。
    pub const fn with(mut self, m: LockMod) -> Self {
        self.on |= m.bit();
        self.dontcare &= !m.bit();
        self
    }
    /// `~IC-` のようにオフを要求する。
    pub const fn without(mut self, m: LockMod) -> Self {
        self.on &= !m.bit();
        self.dontcare &= !m.bit();
        self
    }
    /// この鍵を問わない。
    pub const fn dontcare(mut self, m: LockMod) -> Self {
        self.dontcare |= m.bit();
        self
    }

    pub const fn is_dontcare(self, m: LockMod) -> bool {
        self.dontcare & m.bit() != 0
    }
    pub const fn is_on(self, m: LockMod) -> bool {
        self.on & m.bit() != 0
    }

    pub const fn matches(self, state: LockState) -> bool {
        (self.on | self.dontcare) == (state.0 | self.dontcare)
    }

    /// care の鍵をこの条件の値に、dontcare の鍵を `state` の値にした状態（`ModSpec::apply` と同じ）。
    pub const fn apply(self, state: LockState) -> LockState {
        LockState((state.0 & self.dontcare) | (self.on & !self.dontcare))
    }
}

/// キーマップの識別子（`Config::keymaps` の添字）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct KeymapId(pub usize);

/// `( /クラス/ && /タイトル/ )` か `( /クラス/ || /タイトル/ )` か。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowOp {
    And,
    Or,
}

/// `window` 文の窓の条件（yamy の `Keymap::doesSameWindow`）。
///
/// 照合する相手は、フック DLL がフォーカス窓から親をたどって作る 2 本の文字列:
/// クラス名の列 `exe名:クラス:…:クラス` と、タイトルの列（同じ順）。
/// 正規表現は大文字小文字を区別せず、文字列のどこかに一致すればよい。
#[derive(Clone, Debug)]
pub struct WindowMatch {
    class: Regex,
    title: Regex,
    class_src: String,
    title_src: String,
    op: WindowOp,
}

impl WindowMatch {
    /// `window NAME /クラス/` は `( /クラス/ && /.*/ )` と同じ。
    /// 正規表現が読めなければその理由を返す。
    pub fn new(class: &str, title: Option<&str>, op: WindowOp) -> Result<WindowMatch, String> {
        let class_src = if class.is_empty() { ".*" } else { class }.to_string();
        let title_src = match title {
            Some(t) if !t.is_empty() => t.to_string(),
            _ => ".*".to_string(),
        };
        let build = |src: &str| {
            RegexBuilder::new(src)
                .case_insensitive(true)
                .build()
                .map_err(|e| format!("/{src}/: {e}"))
        };
        Ok(WindowMatch {
            class: build(&class_src)?,
            title: build(&title_src)?,
            class_src,
            title_src,
            op,
        })
    }

    /// 全部の窓に一致する（`Global` 用）。
    pub fn any() -> WindowMatch {
        WindowMatch::new("", None, WindowOp::Or).expect(".* は正しい正規表現")
    }

    pub fn matches(&self, class: &str, title: &str) -> bool {
        match self.op {
            WindowOp::And => self.class.is_match(class) && self.title.is_match(title),
            WindowOp::Or => self.class.is_match(class) || self.title.is_match(title),
        }
    }

    pub fn class_src(&self) -> &str {
        &self.class_src
    }
    pub fn title_src(&self) -> &str {
        &self.title_src
    }
    pub fn op(&self) -> WindowOp {
        self.op
    }
}

impl PartialEq for WindowMatch {
    fn eq(&self, other: &Self) -> bool {
        self.class_src == other.class_src
            && self.title_src == other.title_src
            && self.op == other.op
    }
}
impl Eq for WindowMatch {}

/// 割付の右辺に並ぶ動作。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// キーを出力する。`mods` の dontcare は現在の修飾で埋まる（`*LControl`）。
    /// `edge` が `Both` なら押し・離しの両方を出す。
    /// `locks` は出力には効かない。`keyseq` の中にだけ書け（yamy と同じ）、
    /// `def subst` の置き換え先として使われたときに効く。それ以外では全部 dontcare。
    Key {
        key: Key,
        mods: ModSpec,
        edge: EdgeSpec,
        locks: LockSpec,
    },
    /// `&Prefix(キーマップ)`: 次の 1 キーだけ指定キーマップで解決する。
    Prefix(KeymapId),
    /// `&Keymap(キーマップ)`: いまのキーを指定キーマップで引き直す。
    Keymap(KeymapId),
    /// `&Default`: 押された物理キーをそのまま出す（割付が無いときと同じ）。
    Default,
    /// `&KeymapParent`: 親キーマップで引き直す。親が無ければ `&Default`。
    /// `keymap` / `window` の既定動作。
    KeymapParent,
    /// `&OtherWindowClass`: いまの窓に一致する次の `window` キーマップで引き直す。
    /// 次が無ければ `&Default`。`Global` の既定動作。
    OtherWindowClass,
    /// `&KeymapWindow`: いまの窓に一致する最初の `window` キーマップで引き直す
    /// （`&Prefix` 先のキーマップから、窓の割付をそのまま使うとき）。
    KeymapWindow,
    /// OS 側で実行する関数（窓の操作・マウス移動）。押したときだけ動く。
    Func(Func),
    /// `&Ignore`: 何もしない（キーを出さない）。
    Ignore,
    /// `&VK([E-][D-|U-]名前|番号)`: 仮想キーコードで出す（スキャンコードではなく）。
    /// `down` なら押したときに押しを、`up` なら離したときに離しを出す。どちらも無指定なら両方。
    /// マウスボタン（LButton 等）はマウス入力として出す。
    Vk {
        vk: u8,
        extended: bool,
        down: bool,
        up: bool,
    },
    /// `&Variable(mag, inc)`: エンジンの変数を `変数 * mag + inc` にする。押したときだけ。
    /// `&Repeat` の回数になる（emacsedit.mayu の C-u 数字）。
    Variable { mag: i32, inc: i32 },
    /// `&Repeat(( 動作… ) [, 上限])`: 動作列を変数の値の回数（上限まで）くり返す。
    /// 押したときは最後の 1 回だけ押したままにし、離したときにそれを離す。上限の既定は 10。
    Repeat { actions: Vec<Action>, max: i32 },
    /// `&EmacsEditKillLineFunc`: 前回の kill-line の結果がクリップボードに無ければ溜めた分を捨て、
    /// クリップボードを空にする。押したときだけ。
    EmacsEditKillLineFunc,
    /// `&EmacsEditKillLinePred(動作列1, 動作列2)`: 切り取った後のクリップボードを溜めた分に足し、
    /// 空なら（Edit の行末）改行を足して動作列1、`…\r\n` なら（IE のテキストエリア）動作列2 を出す。
    /// 押したときだけ。
    EmacsEditKillLinePred {
        seq1: Vec<Action>,
        seq2: Vec<Action>,
    },
    /// `&Sync`: ここまでに出したキーがフォーカス窓で処理されるのを待つ。押したときだけ。
    /// コンソール窓では待たない（yamy と同じ）。
    Sync,
    /// `&HelpMessage([タイトル, 本文])`: 案内の窓を出す。`None`（引数なし）なら消す。押したときだけ。
    HelpMessage(Option<HelpText>),
    /// `&HelpVariable(タイトル)`: `&Variable` の値を本文にして案内の窓を出す。押したときだけ。
    HelpVariable(String),
    /// `&ShellExecute(操作, ファイル, パラメータ, フォルダ, 窓の出し方)`: プログラムやファイルを開く。押したときだけ。
    ShellExecute(ShellExecute),
    /// `&MayuDialog(Log, 窓の出し方)`: ログダイアログを出す・隠す。押したときだけ。
    /// yamy の `Investigate`（調査ダイアログ）は newmayu に無いので読み込みエラー。
    MayuDialog(ShowCommand),
    /// `&LogClear`: ログを消す（ログダイアログの「ログのクリア」と同じ）。押したときだけ。
    LogClear,
    /// `&InvestigateCommand`: 窓に届く `WM_COMMAND` / `WM_SYSCOMMAND` をログに出すかを切り替える。押したときだけ。
    InvestigateCommand,
    /// `&LoadSetting([名前])`: 設定を読み直す。名前があれば、設定の一覧でその名前の設定を選んでから。
    /// 空なら名前なし。押したときだけ。
    LoadSetting(String),
    /// `&DescribeBindings`: いまの窓で効いている割付をログに出す。押したときだけ。
    DescribeBindings,
}

/// `&ShellExecute` の引数。空の文字列は yamy と同じく、操作なら `open`、それ以外なら NULL として渡す。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellExecute {
    pub operation: String,
    pub file: String,
    pub parameters: String,
    pub directory: String,
    pub show: ShowCommand,
}

/// `&ShellExecute` / `&MayuDialog` の窓の出し方（yamy の `ShowCommandType`。Win32 の `SW_*`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShowCommand {
    Hide,
    Maximize,
    Minimize,
    Restore,
    Show,
    ShowDefault,
    ShowMaximized,
    ShowMinimized,
    ShowMinNoActive,
    ShowNA,
    ShowNoActivate,
    ShowNormal,
}

/// 案内の窓に出す 2 段の文字列。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HelpText {
    pub title: String,
    pub body: String,
}

/// `event 名 = 動作…` の名前。yamy の `Event`。キーマップごとに持つ。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EventKind {
    /// `prefixed`: `&Prefix` で遷移した直後。遷移先のキーマップのものが動く。
    Prefixed,
    /// `before-key-down`: 物理キーを押したとき、その割付の前。
    BeforeKeyDown,
    /// `after-key-up`: 物理キーを離したとき、その割付の後。
    AfterKeyUp,
}

impl EventKind {
    pub const ALL: [EventKind; 3] = [
        EventKind::Prefixed,
        EventKind::BeforeKeyDown,
        EventKind::AfterKeyUp,
    ];

    pub const fn index(self) -> usize {
        self as usize
    }

    /// `.mayu` に書く名前（大文字小文字は区別しない）。
    pub fn by_name(name: &str) -> Option<EventKind> {
        EventKind::ALL
            .into_iter()
            .find(|k| k.name().eq_ignore_ascii_case(name))
    }

    pub const fn name(self) -> &'static str {
        match self {
            EventKind::Prefixed => "prefixed",
            EventKind::BeforeKeyDown => "before-key-down",
            EventKind::AfterKeyUp => "after-key-up",
        }
    }
}

/// `&PostMessage` の宛先（yamy の `ToWindowType`）。フォーカス窓から見た相対。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToWindow {
    /// `ToItself`: フォーカス窓そのもの。
    Itself,
    /// `ToParentWindow`: その親。
    Parent,
    /// `ToMainWindow`: 親をたどった根。
    Main,
    /// `ToOverlappedWindow`: 子窓でなくなるまでたどった窓。
    Overlapped,
}

/// 窓の関数の対象（yamy の `TargetWindowType`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// 最上位の窓（既定）。
    Overlapped,
    /// MDI 子窓。無ければ最上位の窓。
    Mdi,
}

/// 窓を寄せる向き（yamy の `GravityType`）。`&WindowClingToLeft` は `West`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gravity {
    Center,
    North,
    East,
    West,
    South,
    NorthWest,
    NorthEast,
    SouthWest,
    SouthEast,
}

impl Gravity {
    pub const fn north(self) -> bool {
        matches!(
            self,
            Gravity::North | Gravity::NorthWest | Gravity::NorthEast
        )
    }
    pub const fn south(self) -> bool {
        matches!(
            self,
            Gravity::South | Gravity::SouthWest | Gravity::SouthEast
        )
    }
    pub const fn east(self) -> bool {
        matches!(
            self,
            Gravity::East | Gravity::NorthEast | Gravity::SouthEast
        )
    }
    pub const fn west(self) -> bool {
        matches!(
            self,
            Gravity::West | Gravity::NorthWest | Gravity::SouthWest
        )
    }
}

/// OS 側で実行する `&関数`。エンジンは `Output::Func` に乗せて返すだけで、実行は `window` モジュール。
/// 引数の既定値は yamy と同じ（対象は `Overlapped`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Func {
    /// `&WindowRaise([対象])`: Z 順の先頭へ。
    WindowRaise(Target),
    /// `&WindowLower([対象])`: Z 順の末尾へ。
    WindowLower(Target),
    /// `&WindowMinimize([対象])`: 最小化と復元のトグル。
    WindowMinimize(Target),
    /// `&WindowMaximize([対象])`: 最大化と復元のトグル。
    WindowMaximize(Target),
    /// `&WindowHMaximize([対象])`: 横だけ最大化のトグル。
    WindowHMaximize(Target),
    /// `&WindowVMaximize([対象])`: 縦だけ最大化のトグル。
    WindowVMaximize(Target),
    /// `&WindowMove(dx, dy [, 対象])`: 相対移動。
    WindowMove { dx: i32, dy: i32, target: Target },
    /// `&WindowMoveTo(向き, dx, dy [, 対象])`: 作業領域の端に寄せてから dx, dy だけずらす。
    /// `&WindowClingToLeft` 等はこれの `(West, 0, 0)` 等。
    WindowMoveTo {
        gravity: Gravity,
        dx: i32,
        dy: i32,
        target: Target,
    },
    /// `&WindowMoveVisibly([対象])`: 作業領域からはみ出た分を戻す。
    WindowMoveVisibly(Target),
    /// `&WindowClose([対象])`: 閉じる。
    WindowClose(Target),
    /// `&WindowToggleTopMost`: 最前面の切替。
    WindowToggleTopMost,
    /// `&WindowSetAlpha(n)`: 半透明化のトグル（n は 0〜100 の不透明度）。n < 0 で全部戻す。
    WindowSetAlpha(i32),
    /// `&WindowRedraw`: 再描画。
    WindowRedraw,
    /// `&Recenter`: Edit / RichEdit でキャレット行を縦の中央に。
    Recenter,
    /// `&WindowIdentify`: フォーカス窓のクラス名・タイトル・位置と大きさをログに出す。
    WindowIdentify,
    /// `&WindowResizeTo(幅, 高さ [, 対象])`: 0 はそのまま、負なら作業領域からの差。
    WindowResizeTo {
        width: i32,
        height: i32,
        target: Target,
    },
    /// `&MouseMove(dx, dy)`: マウスカーソルの相対移動。
    MouseMove { dx: i32, dy: i32 },
    /// `&MouseWheel(delta)`: ホイールを回す（120 で奥へ 1 単位、-120 で手前へ 1 単位）。
    MouseWheel(i32),
    /// `&Undefined`: ビープ音を鳴らす（割付の無いキーだと知らせる）。
    Undefined,
    /// `&PostMessage(宛先, メッセージ, wParam, lParam)`: 窓にメッセージを送る（PostMessage）。
    PostMessage {
        to: ToWindow,
        message: u32,
        wparam: u64,
        lparam: i64,
    },
    /// `&ClipboardUpcaseWord`: クリップボードの文字列を大文字にする。
    ClipboardUpcaseWord,
    /// `&ClipboardDowncaseWord`: クリップボードの文字列を小文字にする。
    ClipboardDowncaseWord,
}

impl Action {
    pub const fn key(key: Key) -> Self {
        Action::Key {
            key,
            mods: ModSpec::none(),
            edge: EdgeSpec::Both,
            locks: LockSpec::any(),
        }
    }
    pub const fn key_with(key: Key, mods: ModSpec) -> Self {
        Action::Key {
            key,
            mods,
            edge: EdgeSpec::Both,
            locks: LockSpec::any(),
        }
    }
}

/// `def subst 修飾-キー = 修飾-キー` の 1 つ（yamy の `Keyboard::Substitute`）。
///
/// 入力が左辺（`key` `mods` `edge` `locks`）に合えば、キーマップで引く前にキーを `to_key` に、
/// 修飾と鍵の状態を `to_mods` `to_locks` で決め打ちする（dontcare のものは入力のまま）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Subst {
    pub key: Key,
    pub mods: ModSpec,
    pub edge: EdgeSpec,
    pub locks: LockSpec,
    pub to_key: Key,
    pub to_mods: ModSpec,
    pub to_locks: LockSpec,
}

/// `key 修飾-キー = 動作…` の 1 行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Assignment {
    pub key: Key,
    pub mods: ModSpec,
    pub edge: EdgeSpec,
    pub locks: LockSpec,
    pub actions: Vec<Action>,
    /// `&DescribeBindings` で出す右辺（yamy の形。[`crate::describe::seq_text`]）。読み込み時に作る。
    pub text: String,
}

/// `keymap 名 : 親` / `window 名 /re/ : 親` の 1 つ。
///
/// `mod_keys` は読み込み時に親から継承・加減算を済ませた結果（yamy の `Keymap::adjustModifier`）。
/// エンジンは現在のキーマップの `mod_keys` だけを見る。`one_shot_keys` は `mod_keys` のうち
/// `!!` を付けたもの（yamy の `AM_oneShot`）。
///
/// `window` が `Some` のキーマップは、前景窓が条件に合うとき自動で選ばれる。
/// `None` のキーマップ（`keymap`）は親や `&Prefix` 経由でしか使われない。
/// `default_actions` は割付の無いキーに使う動作列（yamy の default keyseq）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keymap {
    pub name: String,
    pub parent: Option<KeymapId>,
    pub window: Option<WindowMatch>,
    pub default_actions: Vec<Action>,
    pub mod_keys: [Vec<Key>; BasicMod::ALL.len()],
    pub one_shot_keys: [Vec<Key>; BasicMod::ALL.len()],
    pub assignments: Vec<Assignment>,
    /// `event 名 = 動作…`。
    pub events: [Option<Vec<Action>>; EventKind::ALL.len()],
    /// `&DescribeBindings` で出す既定動作（yamy の形）。
    pub default_text: String,
    /// `&DescribeBindings` で出す `event` の右辺（yamy の形）。
    pub event_texts: [String; EventKind::ALL.len()],
}

impl Keymap {
    /// `keymap 名 : 親`。既定動作は `&KeymapParent`。
    pub fn new(name: impl Into<String>, parent: Option<KeymapId>) -> Self {
        Keymap {
            name: name.into(),
            parent,
            window: None,
            default_actions: vec![Action::KeymapParent],
            mod_keys: Default::default(),
            one_shot_keys: Default::default(),
            assignments: Vec::new(),
            events: Default::default(),
            default_text: "&KeymapParent ".to_string(),
            event_texts: Default::default(),
        }
    }

    /// yamy が最初から作る `Global`: 全部の窓に一致し、既定動作は `&OtherWindowClass`。
    pub fn global() -> Self {
        Keymap {
            window: Some(WindowMatch::any()),
            default_actions: vec![Action::OtherWindowClass],
            default_text: "&OtherWindowClass ".to_string(),
            ..Keymap::new("Global", None)
        }
    }

    /// `mod control += 英数`
    pub fn add_mod_key(&mut self, m: BasicMod, key: Key) {
        let keys = &mut self.mod_keys[m.index()];
        if !keys.contains(&key) {
            keys.push(key);
        }
    }

    /// `mod win = !!LWin`
    pub fn add_one_shot_mod_key(&mut self, m: BasicMod, key: Key) {
        self.add_mod_key(m, key);
        let keys = &mut self.one_shot_keys[m.index()];
        if !keys.contains(&key) {
            keys.push(key);
        }
    }

    /// `key 修飾-キー = 動作…`
    ///
    /// 後に書いた割付が先に照合される（yamy の `addAssignment` は先頭に挿入する）。
    /// キー・修飾・押し離し・鍵の条件が同じ割付があれば置き換える。
    pub fn assign(
        &mut self,
        key: Key,
        mods: ModSpec,
        edge: EdgeSpec,
        locks: LockSpec,
        actions: Vec<Action>,
    ) {
        self.assign_text(key, mods, edge, locks, actions, String::new());
    }

    /// [`Keymap::assign`] に、`&DescribeBindings` で出す右辺 `text` を付けたもの。
    pub fn assign_text(
        &mut self,
        key: Key,
        mods: ModSpec,
        edge: EdgeSpec,
        locks: LockSpec,
        actions: Vec<Action>,
        text: String,
    ) {
        if let Some(a) = self
            .assignments
            .iter_mut()
            .find(|a| a.key == key && a.mods == mods && a.edge == edge && a.locks == locks)
        {
            a.actions = actions;
            a.text = text;
            return;
        }
        self.assignments.insert(
            0,
            Assignment {
                key,
                mods,
                edge,
                locks,
                actions,
                text,
            },
        );
    }

    /// `event 名 = 動作…`。同じ名前は置き換える。
    pub fn set_event(&mut self, kind: EventKind, actions: Vec<Action>) {
        self.set_event_text(kind, actions, String::new());
    }

    /// [`Keymap::set_event`] に、`&DescribeBindings` で出す右辺 `text` を付けたもの。
    pub fn set_event_text(&mut self, kind: EventKind, actions: Vec<Action>, text: String) {
        self.events[kind.index()] = Some(actions);
        self.event_texts[kind.index()] = text;
    }

    pub fn event(&self, kind: EventKind) -> Option<&[Action]> {
        self.events[kind.index()].as_deref()
    }

    /// このキーが修飾キーとして登録されていれば、その修飾を返す。
    pub fn modifier_of(&self, key: Key) -> Option<BasicMod> {
        BasicMod::ALL
            .into_iter()
            .find(|m| self.mod_keys[m.index()].contains(&key))
    }

    /// このキーが修飾 `m` の `!!` 付きの修飾キーか。
    pub fn is_one_shot(&self, m: BasicMod, key: Key) -> bool {
        self.one_shot_keys[m.index()].contains(&key)
    }

    /// このキーマップ内だけで割付を探す（親は見ない。yamy の `Keymap::searchAssignment`）。
    /// 同じ組に複数当たるときは先に書かれたものを返す。
    pub fn search(
        &self,
        key: Key,
        mods: ModState,
        locks: LockState,
        edge: Edge,
    ) -> Option<&Assignment> {
        self.assignments.iter().find(|a| {
            a.key == key && a.mods.matches(mods) && a.locks.matches(locks) && a.edge.matches(edge)
        })
    }
}

/// 読み込んだ設定の全体。`keymaps` は定義順。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub keymaps: Vec<Keymap>,
    /// `def key` で定義したキー。複数スキャンコードのキーを入力から組み立てるときに照合する。
    pub keys: Vec<Key>,
    /// `def mod Control = LControl RControl`。出力側で修飾を押す／離すときに使う物理キー。
    /// Shift / Alt / Control / Windows のみ。Mod0〜9 は出力されない。
    pub def_mods: [Vec<Key>; BasicMod::ALL.len()],
    /// `def sync = SCANCODE`。`&Sync` で出す同期キー。無ければ `&Sync` は何もしない。
    pub sync: Option<Key>,
    /// `def subst`。キーマップに関係なく全体で 1 つ。照合する順（後に書いたものが先）に並ぶ。
    pub substs: Vec<Subst>,
    /// `def key` の最初の名前（yamy の `Key::getName`）。`&DescribeBindings` で使う。
    pub key_names: Vec<(Key, String)>,
}

impl Config {
    pub fn new(keymaps: Vec<Keymap>) -> Self {
        Config {
            keymaps,
            keys: Vec::new(),
            def_mods: Default::default(),
            sync: None,
            substs: Vec::new(),
            key_names: Vec::new(),
        }
    }

    /// キーの名前（`def key` の最初の名前）。定義の無いキーはスキャンコードで書く。
    pub fn key_name(&self, key: Key) -> String {
        match self.key_names.iter().find(|(k, _)| *k == key) {
            Some((_, n)) => n.clone(),
            None => format!("{key:?}"),
        }
    }

    /// `key` が定義済みのキーではなく、定義済みのキーの先頭部分になっている
    /// （続きのスキャンコードを待つ）か。yamy の `searchKey` が外れて `searchPrefixKey` が当たる場合。
    pub fn waits_for_more(&self, key: Key) -> bool {
        !self.keys.contains(&key) && self.keys.iter().any(|k| k.starts_with(key))
    }

    /// 入力に合う `def subst` を探す（yamy の `Keyboard::searchSubstitute`）。
    pub fn substitute(
        &self,
        key: Key,
        mods: ModState,
        locks: LockState,
        edge: Edge,
    ) -> Option<&Subst> {
        self.substs.iter().find(|s| {
            s.key == key && s.mods.matches(mods) && s.locks.matches(locks) && s.edge.matches(edge)
        })
    }

    /// 前景窓に一致する `window` キーマップを、照合する順に並べる。
    /// yamy は後に定義したものほど先に照合する（`Global` は最初に作られるので最後）。
    pub fn focus_keymaps(&self, class: &str, title: &str) -> Vec<KeymapId> {
        self.keymaps
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, k)| k.window.as_ref().is_some_and(|w| w.matches(class, title)))
            .map(|(i, _)| KeymapId(i))
            .collect()
    }

    pub fn def_mod(&mut self, m: BasicMod, keys: impl IntoIterator<Item = Key>) {
        self.def_mods[m.index()] = keys.into_iter().collect();
    }

    pub fn keymap(&self, id: KeymapId) -> &Keymap {
        &self.keymaps[id.0]
    }
}

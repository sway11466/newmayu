//! キーマップのデータ構造。
//!
//! `.mayu` を読み込んだ結果はこのモジュールの型に落ちる。エンジンはこの型だけを見る。
//! 修飾の表現は yamy に倣い「押下」「dontcare」の 2 本のビット列で持つ（照合が 1 行で済む）。
//! yamy が一つの列挙に混ぜていた基本修飾（C- S- A- W- M0-9）とキーシーケンス修飾（D- U-）は
//! 型を分ける。割当修飾のうち鍵の状態で決まるもの（IL- IC-）は `LockMod` として別に持つ。

/// スキャンコードの前置バイト。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScanPrefix {
    None,
    E0,
    /// yamy ではマウスボタン等の疑似キーにも使う。
    E1,
}

/// 物理キー。前置バイトとスキャンコードの組で同一性を取る。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    pub prefix: ScanPrefix,
    pub code: u8,
}

impl Key {
    pub const fn new(prefix: ScanPrefix, code: u8) -> Self {
        Key { prefix, code }
    }
    pub const fn plain(code: u8) -> Self {
        Key::new(ScanPrefix::None, code)
    }
    pub const fn e0(code: u8) -> Self {
        Key::new(ScanPrefix::E0, code)
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
}

impl LockMod {
    pub const ALL: [LockMod; 2] = [LockMod::ImeLock, LockMod::ImeComp];

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
}

/// キーマップの識別子（`Config::keymaps` の添字）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct KeymapId(pub usize);

/// 割付の右辺に並ぶ動作。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// キーを出力する。`mods` の dontcare は現在の修飾で埋まる（`*LControl`）。
    /// `edge` が `Both` なら押し・離しの両方を出す。
    Key {
        key: Key,
        mods: ModSpec,
        edge: EdgeSpec,
    },
    /// `&Prefix(キーマップ)`: 次の 1 キーだけ指定キーマップで解決する。
    Prefix(KeymapId),
    /// `&Default`: 押された物理キーをそのまま出す（割付が無いときと同じ）。
    Default,
}

impl Action {
    pub const fn key(key: Key) -> Self {
        Action::Key {
            key,
            mods: ModSpec::none(),
            edge: EdgeSpec::Both,
        }
    }
    pub const fn key_with(key: Key, mods: ModSpec) -> Self {
        Action::Key {
            key,
            mods,
            edge: EdgeSpec::Both,
        }
    }
}

/// `key 修飾-キー = 動作…` の 1 行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Assignment {
    pub key: Key,
    pub mods: ModSpec,
    pub edge: EdgeSpec,
    pub locks: LockSpec,
    pub actions: Vec<Action>,
}

/// `keymap 名 : 親` の 1 つ。
///
/// `mod_keys` は読み込み時に親から継承・加減算を済ませた結果（yamy の `Keymap::adjustModifier`）。
/// エンジンは現在のキーマップの `mod_keys` だけを見る。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keymap {
    pub name: String,
    pub parent: Option<KeymapId>,
    pub mod_keys: [Vec<Key>; BasicMod::ALL.len()],
    pub assignments: Vec<Assignment>,
}

impl Keymap {
    pub fn new(name: impl Into<String>, parent: Option<KeymapId>) -> Self {
        Keymap {
            name: name.into(),
            parent,
            mod_keys: Default::default(),
            assignments: Vec::new(),
        }
    }

    /// `mod control += 英数`
    pub fn add_mod_key(&mut self, m: BasicMod, key: Key) {
        let keys = &mut self.mod_keys[m.index()];
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
        if let Some(a) = self
            .assignments
            .iter_mut()
            .find(|a| a.key == key && a.mods == mods && a.edge == edge && a.locks == locks)
        {
            a.actions = actions;
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
            },
        );
    }

    /// このキーが修飾キーとして登録されていれば、その修飾を返す。
    pub fn modifier_of(&self, key: Key) -> Option<BasicMod> {
        BasicMod::ALL
            .into_iter()
            .find(|m| self.mod_keys[m.index()].contains(&key))
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

/// 読み込んだ設定の全体。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub keymaps: Vec<Keymap>,
    /// 前景窓から選ばれるキーマップ。窓別の選択はまだ無く、常にこれ。
    pub root: KeymapId,
    /// `def mod Control = LControl RControl`。出力側で修飾を押す／離すときに使う物理キー。
    /// Shift / Alt / Control / Windows のみ。Mod0〜9 は出力されない。
    pub def_mods: [Vec<Key>; BasicMod::ALL.len()],
}

impl Config {
    pub fn new(keymaps: Vec<Keymap>, root: KeymapId) -> Self {
        Config {
            keymaps,
            root,
            def_mods: Default::default(),
        }
    }

    pub fn def_mod(&mut self, m: BasicMod, keys: impl IntoIterator<Item = Key>) {
        self.def_mods[m.index()] = keys.into_iter().collect();
    }

    pub fn keymap(&self, id: KeymapId) -> &Keymap {
        &self.keymaps[id.0]
    }
}

//! キー入力を受けて出力を決める状態機械。
//!
//! OS には依存しない。フック側はキー 1 つごとに [`Engine::feed`] を呼び、
//! 返った [`Output`] を `SendInput` に流す（物理入力そのものは常に抑制する前提）。
//! yamy の `Engine::keyboardHandler` 以降の流れのうち、基本修飾・`def subst`・キーマップ木・
//! `key` 割付・キー出力・`&Prefix`・窓によるキーマップ選択を写している。
//!
//! 窓の情報（クラス名の列・タイトルの列）はフック DLL から届き、[`Engine::set_focus`] で入る。
//! 一致する `window` キーマップの列（yamy の `FocusOfThread::m_keymaps`）を持ち、
//! 先頭から順に引く。割付が無いキーはそのキーマップの既定動作に落ちる
//! （普通は `&KeymapParent` で親へ、`Global` は `&OtherWindowClass` で列の次へ）。
//!
//! 押したときに引いた動作列はキーごとに覚え、離しはそれを使って出す（yamy と違う点）。
//! 押している間に修飾や鍵の状態（IME 変換中など）が変わって別の割付に当たっても、
//! 押したときに出したキーが離されずに残らないようにするため。
//! `D-` `U-` 付きの割付は yamy と同じく押しと離しで別々に引く。
//! `def key` に無いキーは yamy と同じく捨てる。
//! `def subst` で引くキーが押しと離しで変わったとき（`def subst D-A = B` など）も yamy と同じく引き直す。

use std::collections::{HashMap, HashSet};

use crate::model::{
    Action, BasicMod, Config, Edge, EdgeSpec, EventKind, Func, HelpText, Key, KeymapId, LockMod,
    LockState, ModSpec, ModState,
};

/// フックから届く 1 イベント。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Input {
    pub key: Key,
    pub edge: Edge,
}

impl Input {
    pub const fn down(key: Key) -> Self {
        Input {
            key,
            edge: Edge::Down,
        }
    }
    pub const fn up(key: Key) -> Self {
        Input {
            key,
            edge: Edge::Up,
        }
    }
}

/// エンジンが OS に出す 1 イベント。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Output {
    Press(Key),
    Release(Key),
    /// 窓の操作など、OS 側で実行する関数。`hwnd` はそのときのフォーカス窓。
    Func {
        hwnd: u64,
        func: Func,
    },
    /// `&VK`: 仮想キーコードで押す／離す。
    Vk {
        vk: u8,
        extended: bool,
        up: bool,
    },
    /// `&Sync`: ここまでの出力がフォーカス窓で処理されるまで、以後の出力を待たせる。
    /// 実際に待つのは本体側（同期キーを出し、フック DLL からの通知を待つ）。
    Sync,
    /// `&HelpMessage` / `&HelpVariable`: 案内の窓を出す。`None` なら消す。
    /// `hwnd` はそのときのフォーカス窓（その窓がある画面に出す）。
    Help {
        hwnd: u64,
        text: Option<HelpText>,
    },
}

/// キーマップ木の探索が戻らないときの上限（yamy と同じ 64）。
const MAX_KEYMAP_RECURSION: usize = 64;

/// 1 キーの処理中に持ち回る値（yamy の `Engine::Current`）。
#[derive(Clone, Copy)]
struct Current {
    /// 物理キー。押したときの動作列をこのキーで覚える。
    input: Key,
    /// 割付を引くキー。`&Default` が出す。`def subst` があれば置き換えた後のキー、無ければ物理キー。
    key: Key,
    /// 入力時の修飾の状態（`def subst` で置き換えた後）。割付の照合に使う。
    state: ModState,
    /// 入力時の鍵の状態（`def subst` で置き換えた後）。割付の照合に使う。
    locks: LockState,
    /// 出力キーの修飾を埋める元。自分が修飾キーならその修飾は dontcare。
    event_mods: ModSpec,
    /// 窓に一致するキーマップ列のうち、いま何番目を引いているか（`&OtherWindowClass` が進める）。
    focus_idx: usize,
}

/// 割付の動作列のうち、どの部分を出すか。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Part {
    /// 全動作を押して離す。
    All,
    /// 最後の動作だけ押したままにする（先行する動作は押して離す）。
    Down,
    /// 最後の動作を離す。
    Up,
}

/// 押したときに [`Part::Down`] で出した動作列。離しはこれを [`Part::Up`] で出す。
struct Held {
    actions: Vec<Action>,
    /// 動作列を引いたキーマップ（`&KeymapParent` の起点）。
    keymap_id: KeymapId,
    /// 押したときの持ち回り値。
    cur: Current,
}

pub struct Engine {
    config: Config,
    /// 前景窓に一致する `window` キーマップの列（照合順）。先頭が通常の解決の起点。
    focus: Vec<KeymapId>,
    /// フォーカスを持つ窓。同じ窓の通知を繰り返し受けても引き直さないために覚える。
    focus_hwnd: u64,
    /// フォーカス窓がコンソール（`ConsoleWindowClass`）。`&Sync` を出さない。
    focus_is_console: bool,
    /// いまキーを解決するキーマップ。通常は `focus` の先頭、`&Prefix` 後は遷移先。
    current: KeymapId,
    /// `&Prefix` を受けて次の 1 キーを待っている。
    is_prefix: bool,
    /// 物理的に押されているキー。
    pressed: HashSet<Key>,
    /// 押したときに出した動作列（離しで使う）。
    held: HashMap<Key, Held>,
    /// IME などの鍵の状態。外から `set_ime` で入る。分からないうちは全部オフ（yamy と同じ）。
    locks: LockState,
    /// OS に押したと伝えてまだ離していないキー。
    win32_pressed: HashSet<Key>,
    /// `&Variable` が変え、`&Repeat` が回数に使う値（yamy の `m_variable`）。
    variable: i32,
    /// 複数スキャンコードのキーの組み立て途中（yamy の `keyboardHandler` の `key`）。
    /// 押し・離しは最初のスキャンコードのもの。
    pending: Option<Input>,
    /// `!!` の修飾キーを押したときの修飾（yamy の `m_oneShotKey`）。他のキーを押したら消す。
    one_shot: Option<(ModState, ModSpec)>,
    out: Vec<Output>,
}

impl Engine {
    pub fn new(config: Config) -> Self {
        // 窓の情報が届くまでは、空の名前に一致するキーマップ（Global）を使う（yamy の global focus）
        let focus = config.focus_keymaps("", "");
        let current = focus.first().copied().unwrap_or(KeymapId(0));
        Engine {
            config,
            focus,
            focus_hwnd: 0,
            focus_is_console: false,
            current,
            is_prefix: false,
            variable: 0,
            pending: None,
            one_shot: None,
            pressed: HashSet::new(),
            held: HashMap::new(),
            locks: LockState::empty(),
            win32_pressed: HashSet::new(),
            out: Vec::new(),
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// 鍵の状態を 1 つ入れる（IME のオン・変換中、NumLock 等）。
    pub fn set_lock(&mut self, m: LockMod, on: bool) {
        self.locks.set(m, on);
    }

    /// IME の状態を入れる。`on` は IME がオン（`IL-`）、`composing` は変換中（`IC-`）。
    pub fn set_ime(&mut self, on: bool, composing: bool) {
        self.set_lock(LockMod::ImeLock, on);
        self.set_lock(LockMod::ImeComp, composing);
    }

    /// フォーカスが移った。`class` `title` はフック DLL が作る列（`model::WindowMatch` 参照）。
    /// 一致する `window` キーマップを引き直し、`&Prefix` の途中なら捨てる。
    pub fn set_focus(&mut self, hwnd: u64, class: &str, title: &str) {
        self.focus_is_console = class
            .split(':')
            .any(|c| c.eq_ignore_ascii_case("ConsoleWindowClass"));
        let focus = self.config.focus_keymaps(class, title);
        if hwnd == self.focus_hwnd && focus == self.focus {
            return;
        }
        self.focus = focus;
        self.focus_hwnd = hwnd;
        self.is_prefix = false;
        self.current = self.root();
    }

    /// 通常の解決の起点（窓に一致するキーマップ列の先頭）。
    fn root(&self) -> KeymapId {
        self.focus.first().copied().unwrap_or(self.current)
    }

    /// いま窓に一致しているキーマップ列。
    pub fn focus(&self) -> &[KeymapId] {
        &self.focus
    }

    /// 1 イベントを処理し、OS に出すイベント列を返す。
    pub fn feed(&mut self, input: Input) -> Vec<Output> {
        // 複数スキャンコードのキー: 届いたスキャンコードを貯め、定義の先頭部分に一致するうちは
        // 何も出さずに次を待つ（yamy の searchKey / searchPrefixKey）
        let input = match self.pending.take() {
            Some(p) => Input {
                // 待つのは定義（4 個まで）の先頭部分のときだけなので、足しても 4 個を超えない
                key: p
                    .key
                    .concat(input.key)
                    .expect("組み立て途中のキーは定義より短い"),
                edge: p.edge,
            },
            None => input,
        };
        if self.config.waits_for_more(input.key) {
            self.pending = Some(input);
            return Vec::new();
        }
        // def key に無いキーは捨て、押しているキーにも数えない（yamy の undefined key）
        if !self.config.keys.contains(&input.key) {
            self.release_all_if_idle();
            return std::mem::take(&mut self.out);
        }

        let Input { key, edge } = input;
        match edge {
            Edge::Down => {
                self.pressed.insert(key);
            }
            Edge::Up => {
                self.pressed.remove(&key);
            }
        }

        let keymap = self.config.keymap(self.current);
        let state = self.current_mods();
        // このキー自身が修飾キーなら、その修飾は問わない（yamy の fixModifierKey）。
        // 照合には効かず、出力キーの修飾を埋めるときに「触らない」印として効く。
        let own_mod = keymap.modifier_of(key);
        let mut event_mods = ModSpec::from_state(state);
        if let Some(m) = own_mod {
            event_mods = event_mods.dontcare(m);
        }

        if own_mod.is_some_and(|m| keymap.is_one_shot(m, key)) {
            // `!!` の修飾キー（yamy の AM_oneShot）: 押しでは何も出さない。他のキーを押さずに離したら、
            // 押したときの修飾でこのキーを押して離す。&Prefix の後は yamy と同じく何も出さない
            if !self.is_prefix {
                match edge {
                    Edge::Down => self.one_shot = Some((state, event_mods)),
                    Edge::Up => {
                        if let Some((state, event_mods)) = self.one_shot.take() {
                            self.begin_generating(key, Edge::Down, state, event_mods);
                            self.begin_generating(key, Edge::Up, state, event_mods);
                        }
                    }
                }
            }
        } else {
            if edge == Edge::Down {
                self.one_shot = None;
            }
            self.generate_with_prefix(key, edge, own_mod, state, event_mods);
        }

        self.release_all_if_idle();
        std::mem::take(&mut self.out)
    }

    /// 物理キーが全部離れたら、OS 側に残した修飾とキーも全部離す。
    fn release_all_if_idle(&mut self) {
        if !self.pressed.is_empty() {
            return;
        }
        self.generate_modifier_events(ModSpec::none());
        let mut rest: Vec<Key> = self.win32_pressed.drain().collect();
        rest.sort();
        for k in rest {
            self.out.push(Output::Release(k));
        }
        self.one_shot = None;
    }

    /// `&Prefix` の前後処理をして 1 キーを出す。
    fn generate_with_prefix(
        &mut self,
        key: Key,
        edge: Edge,
        own_mod: Option<BasicMod>,
        state: ModState,
        event_mods: ModSpec,
    ) {
        // &Prefix の前後処理（yamy の beginGeneratingKeyboardEvents）。
        //   (1) D-prefix  : 現在のキーマップで解決し &Prefix(X) が走る
        //   (2) U-prefix  : root で解決し、終わったら X に戻す
        //   (3) D-next    : X で解決し、終わったら root に戻す
        //   (4) U-next    : root で解決
        let saved = self.current;
        if own_mod.is_none() && self.is_prefix {
            match edge {
                Edge::Down => self.is_prefix = false,
                Edge::Up => self.current = self.root(),
            }
        }

        self.begin_generating(key, edge, state, event_mods);

        if own_mod.is_none() {
            if !self.is_prefix {
                self.current = self.root();
            } else if edge == Edge::Up {
                self.current = saved;
            }
        }
    }

    /// いまのキーマップで 1 キーを出す（`def subst`・event・押したときの動作列の記録を含む）。
    fn begin_generating(&mut self, key: Key, edge: Edge, state: ModState, event_mods: ModSpec) {
        let keymap_id = self.current;
        let mut cur = Current {
            input: key,
            key,
            state,
            locks: self.locks,
            event_mods,
            focus_idx: 0,
        };
        // def subst: キーマップで引く前にキーを置き換える（yamy の beginGeneratingKeyboardEvents）。
        // 置き換え先で書いた修飾・鍵はその値に、dontcare のものは入力のままにする。
        if let Some(s) = self.config.substitute(key, state, self.locks, edge) {
            cur.key = s.to_key;
            cur.state = s.to_mods.apply(state);
            cur.locks = s.to_locks.apply(self.locks);
            cur.event_mods = s.to_mods.add(event_mods);
        }
        // 離しは、押したときに覚えた動作列があればそれを使う。
        // ただし def subst で引くキーが押したときと変わったら、yamy と同じく引き直す
        let held = match edge {
            Edge::Down => None,
            Edge::Up => self.held.remove(&key).filter(|h| h.cur.key == cur.key),
        };
        if edge == Edge::Down {
            self.generate_event(EventKind::BeforeKeyDown, keymap_id, cur, 0);
        }
        match held {
            Some(h) => self.generate_key_seq_events(&h.actions, Part::Up, h.keymap_id, h.cur, 0),
            None => self.generate_keyboard_events(keymap_id, cur, edge, 0),
        }
        if edge == Edge::Up {
            self.generate_event(EventKind::AfterKeyUp, keymap_id, cur, 0);
        }
    }

    /// 現在のキーマップの修飾キー登録と物理押下状態から、いま押されている修飾を求める。
    fn current_mods(&self) -> ModState {
        let keymap = self.config.keymap(self.current);
        let mut state = ModState::empty();
        for m in BasicMod::ALL {
            let on = keymap.mod_keys[m.index()]
                .iter()
                .any(|k| self.pressed.contains(k));
            state.set(m, on);
        }
        state
    }

    /// キーマップで割付を探して動作を出す。無ければそのキーマップの既定動作を出す
    /// （yamy の `generateKeyboardEvents(Current)`）。
    fn generate_keyboard_events(
        &mut self,
        keymap_id: KeymapId,
        cur: Current,
        edge: Edge,
        depth: usize,
    ) {
        if depth >= MAX_KEYMAP_RECURSION {
            return;
        }
        let keymap = self.config.keymap(keymap_id);
        let part_of = |spec: EdgeSpec| match spec {
            EdgeSpec::Both => match edge {
                Edge::Down => Part::Down,
                Edge::Up => Part::Up,
            },
            EdgeSpec::Down | EdgeSpec::Up => Part::All,
        };
        let (actions, part) = match keymap.search(cur.key, cur.state, cur.locks, edge) {
            Some(a) => (a.actions.clone(), part_of(a.edge)),
            None => (keymap.default_actions.clone(), part_of(EdgeSpec::Both)),
        };
        // 離しで使う動作列を覚える。`&KeymapParent` 等で下に進んだ先が最終的に当たった動作列なので、
        // 内側の呼び出しが外側の記録を上書きする。`D-` `U-` 付きの割付に当たったら覚えない
        // （離しは改めて引く）。
        match part {
            Part::Down => {
                self.held.insert(
                    cur.input,
                    Held {
                        actions: actions.clone(),
                        keymap_id,
                        cur,
                    },
                );
            }
            Part::All => {
                self.held.remove(&cur.input);
            }
            Part::Up => {}
        }
        self.generate_key_seq_events(&actions, part, keymap_id, cur, depth);
    }

    /// 割付の無いキー、または `&Default` の動作: 物理キーをそのまま通す。
    /// 修飾は入力時の状態（自分が修飾キーなら dontcare）に揃える。
    fn generate_default(&mut self, key: Key, event_mods: ModSpec, edge: Edge) {
        match edge {
            Edge::Down => {
                self.generate_modifier_events(event_mods);
                self.press(key);
            }
            Edge::Up => self.release(key),
        }
    }

    /// `keymap_id` は動作列を引いたキーマップ（`&KeymapParent` が親をたどる起点）。
    fn generate_key_seq_events(
        &mut self,
        actions: &[Action],
        part: Part,
        keymap_id: KeymapId,
        cur: Current,
        depth: usize,
    ) {
        let Some((last, head)) = actions.split_last() else {
            return;
        };
        if part == Part::Up {
            self.generate_action_events(last, Edge::Up, keymap_id, cur, depth);
            return;
        }
        for a in head {
            self.generate_action_events(a, Edge::Down, keymap_id, cur, depth);
            self.generate_action_events(a, Edge::Up, keymap_id, cur, depth);
        }
        self.generate_action_events(last, Edge::Down, keymap_id, cur, depth);
        if part == Part::All {
            self.generate_action_events(last, Edge::Up, keymap_id, cur, depth);
        }
    }

    fn generate_action_events(
        &mut self,
        action: &Action,
        edge: Edge,
        keymap_id: KeymapId,
        cur: Current,
        depth: usize,
    ) {
        let Current {
            key, event_mods, ..
        } = cur;
        match action {
            Action::Key {
                key,
                mods,
                edge: spec,
                ..
            } => {
                if !spec.matches(edge) {
                    return;
                }
                match edge {
                    Edge::Down => {
                        let m = mods.add(event_mods);
                        self.generate_modifier_events(m);
                        self.press(*key);
                    }
                    Edge::Up => self.release(*key),
                }
            }
            Action::Prefix(target) => {
                if edge == Edge::Down {
                    self.current = *target;
                    self.is_prefix = true;
                    self.generate_event(EventKind::Prefixed, *target, cur, depth + 1);
                }
            }
            Action::Default => self.generate_default(key, event_mods, edge),
            Action::KeymapParent => match self.config.keymap(keymap_id).parent {
                Some(parent) => self.generate_keyboard_events(parent, cur, edge, depth + 1),
                None => self.generate_default(key, event_mods, edge),
            },
            Action::Ignore => {}
            Action::Sync => {
                if edge == Edge::Down && !self.focus_is_console {
                    self.out.push(Output::Sync);
                }
            }
            Action::Variable { mag, inc } => {
                if edge == Edge::Down {
                    self.variable = self.variable.wrapping_mul(*mag).wrapping_add(*inc);
                }
            }
            Action::Repeat { actions, max } => match edge {
                // yamy の funcRepeat: 回数 - 1 回は押して離し、最後の 1 回は押したまま
                Edge::Down => {
                    let end = self.variable.min(*max);
                    for _ in 1..end {
                        self.generate_key_seq_events(actions, Part::All, keymap_id, cur, depth + 1);
                    }
                    if end > 0 {
                        self.generate_key_seq_events(
                            actions,
                            Part::Down,
                            keymap_id,
                            cur,
                            depth + 1,
                        );
                    }
                }
                Edge::Up => {
                    self.generate_key_seq_events(actions, Part::Up, keymap_id, cur, depth + 1);
                }
            },
            Action::Vk {
                vk,
                extended,
                down,
                up,
            } => {
                let emit = match edge {
                    Edge::Down => *down,
                    Edge::Up => *up,
                };
                if emit {
                    self.out.push(Output::Vk {
                        vk: *vk,
                        extended: *extended,
                        up: edge == Edge::Up,
                    });
                }
            }
            Action::Func(f) => {
                if edge == Edge::Down {
                    self.out.push(Output::Func {
                        hwnd: self.focus_hwnd,
                        func: *f,
                    });
                }
            }
            Action::HelpMessage(text) => {
                if edge == Edge::Down {
                    self.out.push(Output::Help {
                        hwnd: self.focus_hwnd,
                        text: text.clone(),
                    });
                }
            }
            Action::HelpVariable(title) => {
                if edge == Edge::Down {
                    self.out.push(Output::Help {
                        hwnd: self.focus_hwnd,
                        text: Some(HelpText {
                            title: title.clone(),
                            body: self.variable.to_string(),
                        }),
                    });
                }
            }
            Action::KeymapWindow => {
                let root = self.root();
                let cur = Current {
                    focus_idx: 0,
                    ..cur
                };
                self.generate_keyboard_events(root, cur, edge, depth + 1)
            }
            Action::OtherWindowClass => {
                let next = cur.focus_idx + 1;
                match self.focus.get(next) {
                    Some(&k) => {
                        let cur = Current {
                            focus_idx: next,
                            ..cur
                        };
                        self.generate_keyboard_events(k, cur, edge, depth + 1)
                    }
                    None => self.generate_default(key, event_mods, edge),
                }
            }
        }
    }

    /// `event` の動作列を全部押して離す（yamy の `generateEvents`）。そのキーマップだけを見る。
    fn generate_event(&mut self, kind: EventKind, keymap_id: KeymapId, cur: Current, depth: usize) {
        let Some(actions) = self.config.keymap(keymap_id).event(kind) else {
            return;
        };
        let actions = actions.to_vec();
        self.generate_key_seq_events(&actions, Part::All, keymap_id, cur, depth);
    }

    /// 出力キーに必要な修飾を OS 側で揃える。dontcare の修飾は触らない。
    fn generate_modifier_events(&mut self, mods: ModSpec) {
        for m in BasicMod::ALL {
            if mods.is_dontcare(m) {
                continue;
            }
            let keys = self.config.def_mods[m.index()].clone();
            if mods.is_pressed(m) {
                let none_pressed = !keys.iter().any(|k| self.win32_pressed.contains(k));
                if none_pressed && let Some(first) = keys.first() {
                    self.press(*first);
                }
            } else {
                for k in keys {
                    if self.win32_pressed.contains(&k) {
                        self.release(k);
                    }
                }
            }
        }
    }

    /// 押下は毎回出す（OS のオートリピートに相当する入力をそのまま通すため）。
    fn press(&mut self, key: Key) {
        self.win32_pressed.insert(key);
        self.out.push(Output::Press(key));
    }

    /// 離しは OS 側で押されているときだけ出す。
    fn release(&mut self, key: Key) {
        if self.win32_pressed.remove(&key) {
            self.out.push(Output::Release(key));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        Action, EventKind, Keymap, LockSpec, ScanCode, ScanPrefix, Subst, WindowMatch, WindowOp,
    };

    // 109.mayu のスキャンコード
    const A: Key = Key::plain(0x1e);
    const B: Key = Key::plain(0x30);
    const F: Key = Key::plain(0x21);
    const H: Key = Key::plain(0x23);
    const X: Key = Key::plain(0x2d);
    const BACK_SPACE: Key = Key::plain(0x0e);
    const CAPS_LOCK: Key = Key::plain(0x3a);
    const L_CONTROL: Key = Key::plain(0x1d);
    const R_CONTROL: Key = Key::e0(0x1d);
    const RIGHT: Key = Key::e0(0x4d);

    const GLOBAL: KeymapId = KeymapId(0);

    /// テストで使うキーの `def key` と `def mod Control = LControl RControl`、それを継承した Global キーマップ。
    fn base() -> (Config, Keymap) {
        let mut config = Config::new(Vec::new());
        config.keys = vec![
            A, B, F, H, X, BACK_SPACE, CAPS_LOCK, L_CONTROL, R_CONTROL, RIGHT, L_WIN,
        ];
        config.def_mod(BasicMod::Control, [L_CONTROL, R_CONTROL]);
        let mut global = Keymap::global();
        global.add_mod_key(BasicMod::Control, L_CONTROL);
        global.add_mod_key(BasicMod::Control, R_CONTROL);
        (config, global)
    }

    fn engine(config: Config, keymaps: Vec<Keymap>) -> Engine {
        Engine::new(Config { keymaps, ..config })
    }

    #[test]
    fn simple_remap_a_to_b() {
        let (config, mut global) = base();
        // key A = B
        global.assign(
            A,
            ModSpec::none(),
            EdgeSpec::Both,
            LockSpec::any(),
            vec![Action::key(B)],
        );
        let mut e = engine(config, vec![global]);

        assert_eq!(e.feed(Input::down(A)), vec![Output::Press(B)]);
        assert_eq!(e.feed(Input::up(A)), vec![Output::Release(B)]);
    }

    /// `def key Pause = E1-0x1d 0x45`
    fn pause() -> Key {
        Key::from_codes(&[
            ScanCode::new(ScanPrefix::E1, 0x1d),
            ScanCode::new(ScanPrefix::None, 0x45),
        ])
        .unwrap()
    }

    #[test]
    fn multi_scan_code_key_waits_for_the_rest() {
        let (mut config, mut global) = base();
        config.keys = vec![A, L_CONTROL, pause()];
        // key Pause = B
        global.assign(
            pause(),
            ModSpec::none(),
            EdgeSpec::Both,
            LockSpec::any(),
            vec![Action::key(B)],
        );
        let mut e = engine(config, vec![global]);
        let e1_1d = Key::new(ScanPrefix::E1, 0x1d);
        let k45 = Key::plain(0x45);

        // 定義の先頭部分に一致するうちは何も出さずに待ち、揃ったら 1 キーとして引く
        assert_eq!(e.feed(Input::down(e1_1d)), vec![]);
        assert_eq!(e.feed(Input::down(k45)), vec![Output::Press(B)]);
        assert_eq!(e.feed(Input::up(e1_1d)), vec![]);
        assert_eq!(e.feed(Input::up(k45)), vec![Output::Release(B)]);
        // 先頭部分でないキーは待たない
        assert_eq!(
            e.feed(Input::down(L_CONTROL)),
            vec![Output::Press(L_CONTROL)]
        );
        assert_eq!(
            e.feed(Input::up(L_CONTROL)),
            vec![Output::Release(L_CONTROL)]
        );
        // def key に無いキーは捨てる（yamy と同じ）
        assert_eq!(e.feed(Input::down(k45)), vec![]);
        assert_eq!(e.feed(Input::up(k45)), vec![]);
    }

    #[test]
    fn undefined_key_is_dropped() {
        let (config, global) = base();
        let mut e = engine(config, vec![global]);
        let undefined = Key::plain(0x45);

        assert_eq!(e.feed(Input::down(undefined)), vec![]);
        assert_eq!(e.feed(Input::up(undefined)), vec![]);
        // 押しているキーの数にも数えない
        assert_eq!(
            e.feed(Input::down(L_CONTROL)),
            vec![Output::Press(L_CONTROL)]
        );
        assert_eq!(e.feed(Input::down(undefined)), vec![]);
        assert_eq!(
            e.feed(Input::up(L_CONTROL)),
            vec![Output::Release(L_CONTROL)]
        );
        assert_eq!(e.feed(Input::up(undefined)), vec![]);
    }

    const L_WIN: Key = Key::e0(0x5b);

    /// `def mod Windows = LWin` と `mod win = !!LWin`。
    fn one_shot_win() -> (Config, Keymap) {
        let (mut config, mut global) = base();
        config.def_mod(BasicMod::Windows, [L_WIN]);
        global.add_one_shot_mod_key(BasicMod::Windows, L_WIN);
        (config, global)
    }

    #[test]
    fn one_shot_modifier_alone_acts_as_the_key_on_release() {
        let (config, global) = one_shot_win();
        let mut e = engine(config, vec![global]);

        // 押しでは何も出さず、離しで押して離す（オートリピートの押しも何も出さない）
        assert_eq!(e.feed(Input::down(L_WIN)), vec![]);
        assert_eq!(e.feed(Input::down(L_WIN)), vec![]);
        assert_eq!(
            e.feed(Input::up(L_WIN)),
            vec![Output::Press(L_WIN), Output::Release(L_WIN)]
        );
    }

    #[test]
    fn one_shot_modifier_uses_its_assignment() {
        let (config, mut global) = one_shot_win();
        // key W-LWin = A（引くときは自分の修飾も押されている。yamy と同じ）
        global.assign(
            L_WIN,
            ModSpec::none().with(BasicMod::Windows),
            EdgeSpec::Both,
            LockSpec::any(),
            vec![Action::key(A)],
        );
        // key W-C-LWin = B
        global.assign(
            L_WIN,
            ModSpec::none()
                .with(BasicMod::Windows)
                .with(BasicMod::Control),
            EdgeSpec::Both,
            LockSpec::any(),
            vec![Action::key(B)],
        );
        let mut e = engine(config, vec![global]);

        assert_eq!(e.feed(Input::down(L_WIN)), vec![]);
        assert_eq!(
            e.feed(Input::up(L_WIN)),
            vec![Output::Press(A), Output::Release(A)]
        );
        // 引くときの修飾は押したときのもの（離す前に Control を離しても C- で引く）
        assert_eq!(
            e.feed(Input::down(L_CONTROL)),
            vec![Output::Press(L_CONTROL)]
        );
        assert_eq!(e.feed(Input::down(L_WIN)), vec![]);
        assert_eq!(
            e.feed(Input::up(L_CONTROL)),
            vec![Output::Release(L_CONTROL)]
        );
        assert_eq!(
            e.feed(Input::up(L_WIN)),
            vec![Output::Press(B), Output::Release(B)]
        );
    }

    #[test]
    fn one_shot_modifier_with_other_key_acts_as_modifier() {
        let (config, global) = one_shot_win();
        let mut e = engine(config, vec![global]);

        // 押している間は修飾として効き、他のキーを挟んだら離しで何も出さない
        assert_eq!(e.feed(Input::down(L_WIN)), vec![]);
        assert_eq!(
            e.feed(Input::down(A)),
            vec![Output::Press(L_WIN), Output::Press(A)]
        );
        assert_eq!(e.feed(Input::up(A)), vec![Output::Release(A)]);
        assert_eq!(e.feed(Input::up(L_WIN)), vec![Output::Release(L_WIN)]);
        // 挟んだ記録は次に持ち越さない
        assert_eq!(e.feed(Input::down(L_WIN)), vec![]);
        assert_eq!(
            e.feed(Input::up(L_WIN)),
            vec![Output::Press(L_WIN), Output::Release(L_WIN)]
        );
        // def key に無いキーは挟んでも取り消さない（yamy と同じ）
        let undefined = Key::plain(0x45);
        assert_eq!(e.feed(Input::down(L_WIN)), vec![]);
        assert_eq!(e.feed(Input::down(undefined)), vec![]);
        assert_eq!(e.feed(Input::up(undefined)), vec![]);
        assert_eq!(
            e.feed(Input::up(L_WIN)),
            vec![Output::Press(L_WIN), Output::Release(L_WIN)]
        );
    }

    #[test]
    fn unassigned_key_passes_through() {
        let (config, global) = base();
        let mut e = engine(config, vec![global]);

        assert_eq!(e.feed(Input::down(A)), vec![Output::Press(A)]);
        assert_eq!(e.feed(Input::up(A)), vec![Output::Release(A)]);
    }

    #[test]
    fn control_h_to_backspace() {
        let (config, mut global) = base();
        // key C-H = BackSpace
        global.assign(
            H,
            ModSpec::none().with(BasicMod::Control),
            EdgeSpec::Both,
            LockSpec::any(),
            vec![Action::key(BACK_SPACE)],
        );
        let mut e = engine(config, vec![global]);

        // 修飾キー自身は素通し
        assert_eq!(
            e.feed(Input::down(L_CONTROL)),
            vec![Output::Press(L_CONTROL)]
        );
        // BackSpace は修飾なしで出すので、OS 側の Control を先に離す
        assert_eq!(
            e.feed(Input::down(H)),
            vec![Output::Release(L_CONTROL), Output::Press(BACK_SPACE)]
        );
        assert_eq!(e.feed(Input::up(H)), vec![Output::Release(BACK_SPACE)]);
        // Control は既に離れているので何も出ない
        assert_eq!(e.feed(Input::up(L_CONTROL)), vec![]);

        // 修飾の無い H は素通し
        assert_eq!(e.feed(Input::down(H)), vec![Output::Press(H)]);
        assert_eq!(e.feed(Input::up(H)), vec![Output::Release(H)]);
    }

    #[test]
    fn capslock_as_control() {
        let (config, mut global) = base();
        // mod control += CapsLock
        global.add_mod_key(BasicMod::Control, CAPS_LOCK);
        // key *CapsLock = *LControl
        global.assign(
            CAPS_LOCK,
            ModSpec::any(),
            EdgeSpec::Both,
            LockSpec::any(),
            vec![Action::key_with(L_CONTROL, ModSpec::any())],
        );
        // key C-H = BackSpace
        global.assign(
            H,
            ModSpec::none().with(BasicMod::Control),
            EdgeSpec::Both,
            LockSpec::any(),
            vec![Action::key(BACK_SPACE)],
        );
        let mut e = engine(config, vec![global]);

        assert_eq!(
            e.feed(Input::down(CAPS_LOCK)),
            vec![Output::Press(L_CONTROL)]
        );
        assert_eq!(
            e.feed(Input::down(H)),
            vec![Output::Release(L_CONTROL), Output::Press(BACK_SPACE)]
        );
        assert_eq!(e.feed(Input::up(H)), vec![Output::Release(BACK_SPACE)]);
        assert_eq!(e.feed(Input::up(CAPS_LOCK)), vec![]);

        // CapsLock 単押しは LControl の押し離しになる
        assert_eq!(
            e.feed(Input::down(CAPS_LOCK)),
            vec![Output::Press(L_CONTROL)]
        );
        assert_eq!(
            e.feed(Input::up(CAPS_LOCK)),
            vec![Output::Release(L_CONTROL)]
        );
    }

    #[test]
    fn prefix_switches_keymap_for_next_key() {
        let (config, mut global) = base();
        let cx = KeymapId(1);
        // keymap Global: key C-X = &Prefix(EmacsC-X)
        global.assign(
            X,
            ModSpec::none().with(BasicMod::Control),
            EdgeSpec::Both,
            LockSpec::any(),
            vec![Action::Prefix(cx)],
        );
        // keymap EmacsC-X : Global: key F = Right
        let mut emacs_cx = Keymap::new("EmacsC-X", Some(GLOBAL));
        emacs_cx.add_mod_key(BasicMod::Control, L_CONTROL);
        emacs_cx.add_mod_key(BasicMod::Control, R_CONTROL);
        emacs_cx.assign(
            F,
            ModSpec::none(),
            EdgeSpec::Both,
            LockSpec::any(),
            vec![Action::key(RIGHT)],
        );
        let mut e = engine(config, vec![global, emacs_cx]);

        assert_eq!(
            e.feed(Input::down(L_CONTROL)),
            vec![Output::Press(L_CONTROL)]
        );
        // C-x: 遷移だけで出力なし
        assert_eq!(e.feed(Input::down(X)), vec![]);
        assert_eq!(e.feed(Input::up(X)), vec![]);
        assert_eq!(e.current, cx);
        assert!(e.is_prefix);
        // 修飾キーを離しても前置状態は続く
        assert_eq!(
            e.feed(Input::up(L_CONTROL)),
            vec![Output::Release(L_CONTROL)]
        );
        assert_eq!(e.current, cx);
        // 次の 1 キーは EmacsC-X で解決される
        assert_eq!(e.feed(Input::down(F)), vec![Output::Press(RIGHT)]);
        assert_eq!(e.current, GLOBAL);
        assert!(!e.is_prefix);
        // F の離しは Global で素通し扱いになり Right は残るが、全キーが離れた時点で解放される
        assert_eq!(e.feed(Input::up(F)), vec![Output::Release(RIGHT)]);

        // 前置が解けた後の F は素通し
        assert_eq!(e.feed(Input::down(F)), vec![Output::Press(F)]);
        assert_eq!(e.feed(Input::up(F)), vec![Output::Release(F)]);
    }

    #[test]
    fn default_action_passes_the_physical_key_through() {
        let (mut config, mut global) = base();
        // 子キーマップで C-A を &Default にすると、親の A→B を通らず A がそのまま出る
        global.assign(
            A,
            ModSpec::none(),
            EdgeSpec::Both,
            LockSpec::any(),
            vec![Action::key(B)],
        );
        global.assign(
            A,
            ModSpec::none().with(BasicMod::Control),
            EdgeSpec::Both,
            LockSpec::any(),
            vec![Action::Default],
        );
        config.def_mod(BasicMod::Control, [L_CONTROL]);
        let mut e = engine(config, vec![global]);
        assert_eq!(
            e.feed(Input::down(L_CONTROL)),
            vec![Output::Press(L_CONTROL)]
        );
        assert_eq!(e.feed(Input::down(A)), vec![Output::Press(A)]);
        assert_eq!(e.feed(Input::up(A)), vec![Output::Release(A)]);
        assert_eq!(
            e.feed(Input::up(L_CONTROL)),
            vec![Output::Release(L_CONTROL)]
        );
        // 修飾なしの A は割付どおり B
        assert_eq!(e.feed(Input::down(A)), vec![Output::Press(B)]);
    }

    #[test]
    fn ime_state_selects_assignment() {
        let (config, mut global) = base();
        // key A = B        （既定: 変換中でないとき）
        // key IC-A = X     （変換中）
        // key IL-F = Right （IME オン）
        let d = LockSpec::assign_default();
        global.assign(A, ModSpec::none(), EdgeSpec::Both, d, vec![Action::key(B)]);
        global.assign(
            A,
            ModSpec::none(),
            EdgeSpec::Both,
            d.with(LockMod::ImeComp),
            vec![Action::key(X)],
        );
        global.assign(
            F,
            ModSpec::none(),
            EdgeSpec::Both,
            d.with(LockMod::ImeLock),
            vec![Action::key(RIGHT)],
        );
        let mut e = engine(config, vec![global]);

        // 状態が分からないうちは IME オフ・変換中でない扱い
        assert_eq!(e.feed(Input::down(A)), vec![Output::Press(B)]);
        assert_eq!(e.feed(Input::up(A)), vec![Output::Release(B)]);
        assert_eq!(e.feed(Input::down(F)), vec![Output::Press(F)]);
        assert_eq!(e.feed(Input::up(F)), vec![Output::Release(F)]);

        // IME オン・変換中でない
        e.set_ime(true, false);
        assert_eq!(e.feed(Input::down(A)), vec![Output::Press(B)]);
        assert_eq!(e.feed(Input::up(A)), vec![Output::Release(B)]);
        assert_eq!(e.feed(Input::down(F)), vec![Output::Press(RIGHT)]);
        assert_eq!(e.feed(Input::up(F)), vec![Output::Release(RIGHT)]);

        // 変換中: 既定の割付は効かず、IC- の割付が効く
        e.set_ime(true, true);
        assert_eq!(e.feed(Input::down(A)), vec![Output::Press(X)]);
        assert_eq!(e.feed(Input::up(A)), vec![Output::Release(X)]);
    }

    #[test]
    fn window_keymaps_are_tried_in_order() {
        let (config, mut global) = base();
        let any = LockSpec::any();
        // keymap Global:                 key A = B
        // keymap EmacsEdit : Global:     key F = Right
        // window Edit /:Edit$/ : EmacsEdit:  key H = BackSpace
        // window Note /notepad/ : Global:    key X = B      （後に定義 → 先に照合）
        global.assign(
            A,
            ModSpec::none(),
            EdgeSpec::Both,
            any,
            vec![Action::key(B)],
        );
        let mut emacs = Keymap::new("EmacsEdit", Some(GLOBAL));
        emacs.assign(
            F,
            ModSpec::none(),
            EdgeSpec::Both,
            any,
            vec![Action::key(RIGHT)],
        );
        let mut edit = Keymap::new("Edit", Some(KeymapId(1)));
        edit.window = Some(WindowMatch::new(":Edit$", None, WindowOp::And).unwrap());
        edit.assign(
            H,
            ModSpec::none(),
            EdgeSpec::Both,
            any,
            vec![Action::key(BACK_SPACE)],
        );
        let mut note = Keymap::new("Note", Some(GLOBAL));
        note.window = Some(WindowMatch::new("notepad", None, WindowOp::And).unwrap());
        note.assign(
            X,
            ModSpec::none(),
            EdgeSpec::Both,
            any,
            vec![Action::key(B)],
        );
        let mut e = engine(config, vec![global, emacs, edit, note]);

        // 窓の情報が無いうちは Global だけ
        assert_eq!(e.focus(), &[GLOBAL]);
        assert_eq!(e.feed(Input::down(H)), vec![Output::Press(H)]);
        e.feed(Input::up(H));

        e.set_focus(0x10, "notepad.exe:Notepad:Edit", "無題 - メモ帳");
        assert_eq!(e.focus(), &[KeymapId(3), KeymapId(2), GLOBAL]);
        // Note で当たる
        assert_eq!(e.feed(Input::down(X)), vec![Output::Press(B)]);
        e.feed(Input::up(X));
        // Note → 親 Global → &OtherWindowClass → Edit で当たる
        assert_eq!(e.feed(Input::down(H)), vec![Output::Press(BACK_SPACE)]);
        e.feed(Input::up(H));
        // Note → Global → Edit → 親 EmacsEdit で当たる
        assert_eq!(e.feed(Input::down(F)), vec![Output::Press(RIGHT)]);
        e.feed(Input::up(F));
        // Note → Global で当たる（Global は Note の親として先に引かれる）
        assert_eq!(e.feed(Input::down(A)), vec![Output::Press(B)]);
        e.feed(Input::up(A));
        // どこにも無い: 素通し
        assert_eq!(
            e.feed(Input::down(CAPS_LOCK)),
            vec![Output::Press(CAPS_LOCK)]
        );
        e.feed(Input::up(CAPS_LOCK));

        // 別の窓: Global だけに戻る
        e.set_focus(0x20, "chrome.exe:Chrome_WidgetWin_1", "");
        assert_eq!(e.focus(), &[GLOBAL]);
        assert_eq!(e.feed(Input::down(H)), vec![Output::Press(H)]);
        e.feed(Input::up(H));
        assert_eq!(e.feed(Input::down(A)), vec![Output::Press(B)]);
    }

    #[test]
    fn help_message_and_variable() {
        let (config, mut global) = base();
        let (none, both, any) = (ModSpec::none(), EdgeSpec::Both, LockSpec::any());
        let text = HelpText {
            title: "T".into(),
            body: "B".into(),
        };
        global.assign(
            A,
            none,
            both,
            any,
            vec![Action::HelpMessage(Some(text.clone()))],
        );
        global.assign(B, none, both, any, vec![Action::HelpMessage(None)]);
        global.assign(
            H,
            none,
            both,
            any,
            vec![
                Action::Variable { mag: 1, inc: 42 },
                Action::HelpVariable("繰り返し".into()),
            ],
        );
        let mut e = engine(config, vec![global]);
        e.set_focus(0x1234, "x.exe:A", "");
        // 押したときだけ出す
        assert_eq!(
            e.feed(Input::down(A)),
            vec![Output::Help {
                hwnd: 0x1234,
                text: Some(text)
            }]
        );
        assert_eq!(e.feed(Input::up(A)), vec![]);
        assert_eq!(
            e.feed(Input::down(B)),
            vec![Output::Help {
                hwnd: 0x1234,
                text: None
            }]
        );
        e.feed(Input::up(B));
        // 本文は &Variable の値
        assert_eq!(
            e.feed(Input::down(H)),
            vec![Output::Help {
                hwnd: 0x1234,
                text: Some(HelpText {
                    title: "繰り返し".into(),
                    body: "42".into()
                })
            }]
        );
    }

    #[test]
    fn variable_counts_repeat() {
        let (config, mut global) = base();
        let (none, both, any) = (ModSpec::none(), EdgeSpec::Both, LockSpec::any());
        // key A = &Variable(10, 3)
        global.assign(
            A,
            none,
            both,
            any,
            vec![Action::Variable { mag: 10, inc: 3 }],
        );
        // key B = &Repeat((X), 100)
        global.assign(
            B,
            none,
            both,
            any,
            vec![Action::Repeat {
                actions: vec![Action::key(X)],
                max: 100,
            }],
        );
        // key H = &Repeat((X), 2)
        global.assign(
            H,
            none,
            both,
            any,
            vec![Action::Repeat {
                actions: vec![Action::key(X)],
                max: 2,
            }],
        );
        let mut e = engine(config, vec![global]);

        // 変数が 0 のうちは何も出ない
        assert_eq!(e.feed(Input::down(B)), vec![]);
        assert_eq!(e.feed(Input::up(B)), vec![]);

        e.feed(Input::down(A));
        e.feed(Input::up(A));
        assert_eq!(e.variable, 3);
        // 3 回。最後の 1 回は押したまま、離しで離す
        assert_eq!(
            e.feed(Input::down(B)),
            vec![
                Output::Press(X),
                Output::Release(X),
                Output::Press(X),
                Output::Release(X),
                Output::Press(X)
            ]
        );
        assert_eq!(e.feed(Input::up(B)), vec![Output::Release(X)]);
        // 上限 2
        assert_eq!(
            e.feed(Input::down(H)),
            vec![Output::Press(X), Output::Release(X), Output::Press(X)]
        );
        assert_eq!(e.feed(Input::up(H)), vec![Output::Release(X)]);
    }

    #[test]
    fn events_fire_around_keys_and_on_prefix() {
        let (config, mut global) = base();
        let (none, both, any) = (ModSpec::none(), EdgeSpec::Both, LockSpec::any());
        let sub = KeymapId(1);
        global.set_event(EventKind::BeforeKeyDown, vec![Action::key(X)]);
        global.set_event(EventKind::AfterKeyUp, vec![Action::key(H)]);
        // key A = &Prefix(Sub)
        global.assign(A, none, both, any, vec![Action::Prefix(sub)]);
        let mut subk = Keymap::new("Sub", Some(GLOBAL));
        subk.set_event(EventKind::Prefixed, vec![Action::key(F)]);
        // keymap Sub: key B = Right
        subk.assign(B, none, both, any, vec![Action::key(RIGHT)]);
        let mut e = engine(config, vec![global, subk]);

        // A 押し: Global の before-key-down → &Prefix → Sub の prefixed
        assert_eq!(
            e.feed(Input::down(A)),
            vec![
                Output::Press(X),
                Output::Release(X),
                Output::Press(F),
                Output::Release(F)
            ]
        );
        // A 離し: 離しは Global で引くので Global の after-key-up
        assert_eq!(
            e.feed(Input::up(A)),
            vec![Output::Press(H), Output::Release(H)]
        );
        // 次の B は Sub で引く。Sub に before-key-down は無い
        assert_eq!(e.feed(Input::down(B)), vec![Output::Press(RIGHT)]);
        // B 離しは Global に戻っているので Global の after-key-up
        assert_eq!(
            e.feed(Input::up(B)),
            vec![Output::Release(RIGHT), Output::Press(H), Output::Release(H)]
        );
    }

    #[test]
    fn keymap_window_resolves_in_the_window_keymap() {
        let (config, mut global) = base();
        let (none, both, any) = (ModSpec::none(), EdgeSpec::Both, LockSpec::any());
        let sub = KeymapId(1);
        // key A = &Prefix(Sub)   key B = X
        global.assign(A, none, both, any, vec![Action::Prefix(sub)]);
        global.assign(B, none, both, any, vec![Action::key(X)]);
        // keymap Sub: key B = &KeymapWindow   key H = F
        let mut subk = Keymap::new("Sub", None);
        subk.assign(B, none, both, any, vec![Action::KeymapWindow]);
        subk.assign(H, none, both, any, vec![Action::key(F)]);
        let mut e = engine(config, vec![global, subk]);

        e.feed(Input::down(A));
        e.feed(Input::up(A));
        // Sub の B は &KeymapWindow → 窓のキーマップ（Global）の B → X
        assert_eq!(e.feed(Input::down(B)), vec![Output::Press(X)]);
        assert_eq!(e.feed(Input::up(B)), vec![Output::Release(X)]);
        e.feed(Input::down(A));
        e.feed(Input::up(A));
        assert_eq!(e.feed(Input::down(H)), vec![Output::Press(F)]);
        assert_eq!(e.feed(Input::up(H)), vec![Output::Release(F)]);
    }

    #[test]
    fn sync_is_output_on_press_except_in_console() {
        let (config, mut global) = base();
        // key A = &Sync B
        global.assign(
            A,
            ModSpec::none(),
            EdgeSpec::Both,
            LockSpec::any(),
            vec![Action::Sync, Action::key(B)],
        );
        let mut e = engine(config, vec![global]);
        e.set_focus(1, "x.exe:Edit", "");
        assert_eq!(e.feed(Input::down(A)), vec![Output::Sync, Output::Press(B)]);
        assert_eq!(e.feed(Input::up(A)), vec![Output::Release(B)]);
        // コンソールでは待たない
        e.set_focus(2, "conhost.exe:ConsoleWindowClass", "");
        assert_eq!(e.feed(Input::down(A)), vec![Output::Press(B)]);
        assert_eq!(e.feed(Input::up(A)), vec![Output::Release(B)]);
    }

    #[test]
    fn func_is_output_on_press_with_focus_window() {
        use crate::model::Target;
        let (config, mut global) = base();
        // key C-X = &WindowMove(16, 0)
        let f = Func::WindowMove {
            dx: 16,
            dy: 0,
            target: Target::Overlapped,
        };
        global.assign(
            X,
            ModSpec::none().with(BasicMod::Control),
            EdgeSpec::Both,
            LockSpec::any(),
            vec![Action::Func(f)],
        );
        let mut e = engine(config, vec![global]);
        e.set_focus(0x1234, "x.exe:A", "");
        assert_eq!(
            e.feed(Input::down(L_CONTROL)),
            vec![Output::Press(L_CONTROL)]
        );
        assert_eq!(
            e.feed(Input::down(X)),
            vec![Output::Func {
                hwnd: 0x1234,
                func: f
            }]
        );
        // 離しでは何も出ない
        assert_eq!(e.feed(Input::up(X)), vec![]);
    }

    #[test]
    fn ignore_outputs_nothing_and_num_lock_is_a_condition() {
        let (config, mut global) = base();
        let d = LockSpec::assign_default();
        // key A = &Ignore / key NL-B = X / key ~NL-B = H
        global.assign(A, ModSpec::none(), EdgeSpec::Both, d, vec![Action::Ignore]);
        global.assign(
            B,
            ModSpec::none(),
            EdgeSpec::Both,
            d.with(LockMod::NumLock),
            vec![Action::key(X)],
        );
        global.assign(
            B,
            ModSpec::none(),
            EdgeSpec::Both,
            d.without(LockMod::NumLock),
            vec![Action::key(H)],
        );
        let mut e = engine(config, vec![global]);
        assert_eq!(e.feed(Input::down(A)), vec![]);
        assert_eq!(e.feed(Input::up(A)), vec![]);
        assert_eq!(e.feed(Input::down(B)), vec![Output::Press(H)]);
        e.feed(Input::up(B));
        e.set_lock(LockMod::NumLock, true);
        assert_eq!(e.feed(Input::down(B)), vec![Output::Press(X)]);
    }

    #[test]
    fn vk_follows_its_down_up_flags() {
        let (config, mut global) = base();
        let any = LockSpec::any();
        // key A = &VK(RButton) / key B = &VK(D-E-Return)
        global.assign(
            A,
            ModSpec::none(),
            EdgeSpec::Both,
            any,
            vec![Action::Vk {
                vk: 0x02,
                extended: false,
                down: true,
                up: true,
            }],
        );
        global.assign(
            B,
            ModSpec::none(),
            EdgeSpec::Both,
            any,
            vec![Action::Vk {
                vk: 0x0d,
                extended: true,
                down: true,
                up: false,
            }],
        );
        let mut e = engine(config, vec![global]);
        assert_eq!(
            e.feed(Input::down(A)),
            vec![Output::Vk {
                vk: 0x02,
                extended: false,
                up: false
            }]
        );
        assert_eq!(
            e.feed(Input::up(A)),
            vec![Output::Vk {
                vk: 0x02,
                extended: false,
                up: true
            }]
        );
        assert_eq!(
            e.feed(Input::down(B)),
            vec![Output::Vk {
                vk: 0x0d,
                extended: true,
                up: false
            }]
        );
        assert_eq!(e.feed(Input::up(B)), vec![]);
    }

    #[test]
    fn keymap_default_actions() {
        let (config, mut global) = base();
        let any = LockSpec::any();
        // keymap KeymapDefault = &Default
        // keymap Global: key A = B / key X = &Prefix(KeymapDefault)
        global.assign(
            A,
            ModSpec::none(),
            EdgeSpec::Both,
            any,
            vec![Action::key(B)],
        );
        let kd = KeymapId(1);
        global.assign(
            X,
            ModSpec::none(),
            EdgeSpec::Both,
            any,
            vec![Action::Prefix(kd)],
        );
        let mut keymap_default = Keymap::new("KeymapDefault", None);
        keymap_default.default_actions = vec![Action::Default];
        let mut e = engine(config, vec![global, keymap_default]);

        assert_eq!(e.feed(Input::down(A)), vec![Output::Press(B)]);
        e.feed(Input::up(A));
        // X の次の 1 キーは KeymapDefault で解決: 親が無くても &Default で素通し
        assert_eq!(e.feed(Input::down(X)), vec![]);
        e.feed(Input::up(X));
        assert_eq!(e.feed(Input::down(A)), vec![Output::Press(A)]);
        assert_eq!(e.feed(Input::up(A)), vec![Output::Release(A)]);
        assert_eq!(e.feed(Input::down(A)), vec![Output::Press(B)]);
    }

    #[test]
    fn child_keymap_falls_back_to_parent() {
        let (config, mut global) = base();
        let child = KeymapId(1);
        global.assign(
            A,
            ModSpec::none(),
            EdgeSpec::Both,
            LockSpec::any(),
            vec![Action::key(B)],
        );
        global.assign(
            X,
            ModSpec::none(),
            EdgeSpec::Both,
            LockSpec::any(),
            vec![Action::Prefix(child)],
        );
        let mut c = Keymap::new("Child", Some(GLOBAL));
        c.add_mod_key(BasicMod::Control, L_CONTROL);
        let mut e = engine(config, vec![global, c]);

        e.feed(Input::down(X));
        e.feed(Input::up(X));
        // Child に A の割付は無く、親 Global の A = B が使われる
        assert_eq!(e.feed(Input::down(A)), vec![Output::Press(B)]);
        assert_eq!(e.feed(Input::up(A)), vec![Output::Release(B)]);
    }
    #[test]
    fn held_key_is_released_even_if_lock_state_changed() {
        let (config, mut global) = base();
        // key A = B / key IC-A = X
        let d = LockSpec::assign_default();
        global.assign(A, ModSpec::none(), EdgeSpec::Both, d, vec![Action::key(B)]);
        global.assign(
            A,
            ModSpec::none(),
            EdgeSpec::Both,
            d.with(LockMod::ImeComp),
            vec![Action::key(X)],
        );
        let mut e = engine(config, vec![global]);

        // H を押したままにして、全キー解放時の後始末で離されないようにする
        assert_eq!(e.feed(Input::down(H)), vec![Output::Press(H)]);
        // 変換中でないときに押して B が出る。離す前に変換中になっても B が離される
        e.set_ime(true, false);
        assert_eq!(e.feed(Input::down(A)), vec![Output::Press(B)]);
        e.set_ime(true, true);
        assert_eq!(e.feed(Input::up(A)), vec![Output::Release(B)]);
        assert_eq!(e.feed(Input::up(H)), vec![Output::Release(H)]);
    }

    #[test]
    fn held_key_is_released_even_if_composition_ended() {
        let (config, mut global) = base();
        let d = LockSpec::assign_default();
        global.assign(A, ModSpec::none(), EdgeSpec::Both, d, vec![Action::key(B)]);
        global.assign(
            A,
            ModSpec::none(),
            EdgeSpec::Both,
            d.with(LockMod::ImeComp),
            vec![Action::key(X)],
        );
        let mut e = engine(config, vec![global]);

        assert_eq!(e.feed(Input::down(H)), vec![Output::Press(H)]);
        // 変換中に押して X が出る。離す前に変換が終わっても X が離される
        e.set_ime(true, true);
        assert_eq!(e.feed(Input::down(A)), vec![Output::Press(X)]);
        e.set_ime(true, false);
        assert_eq!(e.feed(Input::up(A)), vec![Output::Release(X)]);
        assert_eq!(e.feed(Input::up(H)), vec![Output::Release(H)]);
    }

    /// `def subst 左辺 = 置き換え先` を 1 つ作る。
    fn subst(key: Key, mods: ModSpec, edge: EdgeSpec, locks: LockSpec, to: Action) -> Subst {
        let Action::Key {
            key: to_key,
            mods: to_mods,
            locks: to_locks,
            ..
        } = to
        else {
            unreachable!()
        };
        Subst {
            key,
            mods,
            edge,
            locks,
            to_key,
            to_mods,
            to_locks,
        }
    }

    #[test]
    fn subst_replaces_the_key_before_the_keymap() {
        let (mut config, mut global) = base();
        // def subst *A = *B
        // def subst *F = C-*H
        // def subst *X = IC-*Right
        // key A = X / key B = F / key C-H = BackSpace / key IC-Right = A
        let any = LockSpec::any();
        let to = |key, mods, locks| Action::Key {
            key,
            mods,
            edge: EdgeSpec::Both,
            locks,
        };
        config.substs = vec![
            subst(
                A,
                ModSpec::any(),
                EdgeSpec::Both,
                any,
                to(B, ModSpec::any(), any),
            ),
            subst(
                F,
                ModSpec::any(),
                EdgeSpec::Both,
                any,
                to(H, ModSpec::any().with(BasicMod::Control), any),
            ),
            subst(
                X,
                ModSpec::any(),
                EdgeSpec::Both,
                any,
                to(RIGHT, ModSpec::any(), any.with(LockMod::ImeComp)),
            ),
        ];
        let d = LockSpec::assign_default();
        global.assign(A, ModSpec::none(), EdgeSpec::Both, d, vec![Action::key(X)]);
        global.assign(B, ModSpec::none(), EdgeSpec::Both, d, vec![Action::key(F)]);
        global.assign(
            H,
            ModSpec::none().with(BasicMod::Control),
            EdgeSpec::Both,
            d,
            vec![Action::key(BACK_SPACE)],
        );
        global.assign(
            RIGHT,
            ModSpec::none(),
            EdgeSpec::Both,
            d.with(LockMod::ImeComp),
            vec![Action::key(A)],
        );
        let mut e = engine(config, vec![global]);

        // A は B として引かれる（A の割付は見ない）
        assert_eq!(e.feed(Input::down(A)), vec![Output::Press(F)]);
        assert_eq!(e.feed(Input::up(A)), vec![Output::Release(F)]);
        // 置き換え先で書いた修飾は、押していなくても押している扱いで引く
        assert_eq!(e.feed(Input::down(F)), vec![Output::Press(BACK_SPACE)]);
        assert_eq!(e.feed(Input::up(F)), vec![Output::Release(BACK_SPACE)]);
        // 鍵も同じ。変換中でなくても変換中として引く
        assert_eq!(e.feed(Input::down(X)), vec![Output::Press(A)]);
        assert_eq!(e.feed(Input::up(X)), vec![Output::Release(A)]);
        // 割付が無ければ置き換えた後のキーを出す。dontcare の修飾は入力のまま
        assert_eq!(
            e.feed(Input::down(L_CONTROL)),
            vec![Output::Press(L_CONTROL)]
        );
        assert_eq!(e.feed(Input::down(A)), vec![Output::Press(B)]);
        assert_eq!(e.feed(Input::up(A)), vec![Output::Release(B)]);
        assert_eq!(
            e.feed(Input::up(L_CONTROL)),
            vec![Output::Release(L_CONTROL)]
        );
    }

    #[test]
    fn subst_is_looked_up_again_on_release() {
        let (mut config, global) = base();
        // def subst C-A = B
        // def subst D-H = F
        let d = LockSpec::assign_default();
        let c = ModSpec::none().with(BasicMod::Control);
        config.substs = vec![
            subst(A, c, EdgeSpec::Both, d, Action::key(B)),
            subst(H, ModSpec::none(), EdgeSpec::Down, d, Action::key(F)),
        ];
        let mut e = engine(config, vec![global]);

        // 左辺の修飾に合わなければ置き換えない
        assert_eq!(e.feed(Input::down(A)), vec![Output::Press(A)]);
        assert_eq!(e.feed(Input::up(A)), vec![Output::Release(A)]);
        // 以下、X を押したままにして「物理キーが全部離れたら全部離す」が効かないようにする。
        // C-A は B（置き換え先は修飾なしなので Control を離して出す）。
        // Control を先に離すと、A の離しは置き換わらず A として引かれ、B は離されない（yamy と同じ）
        assert_eq!(e.feed(Input::down(X)), vec![Output::Press(X)]);
        assert_eq!(
            e.feed(Input::down(L_CONTROL)),
            vec![Output::Press(L_CONTROL)]
        );
        assert_eq!(
            e.feed(Input::down(A)),
            vec![Output::Release(L_CONTROL), Output::Press(B)]
        );
        assert_eq!(e.feed(Input::up(L_CONTROL)), vec![]);
        assert_eq!(e.feed(Input::up(A)), vec![]);
        assert_eq!(
            e.feed(Input::up(X)),
            vec![Output::Release(X), Output::Release(B)]
        );
        // D- の左辺は押しだけ置き換わる。離しは H として引かれ、F は離されない
        assert_eq!(e.feed(Input::down(X)), vec![Output::Press(X)]);
        assert_eq!(e.feed(Input::down(H)), vec![Output::Press(F)]);
        assert_eq!(e.feed(Input::up(H)), vec![]);
        assert_eq!(
            e.feed(Input::up(X)),
            vec![Output::Release(X), Output::Release(F)]
        );
    }
}

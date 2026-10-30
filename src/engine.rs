//! キー入力を受けて出力を決める状態機械。
//!
//! OS には依存しない。フック側はキー 1 つごとに [`Engine::feed`] を呼び、
//! 返った [`Output`] を `SendInput` に流す（物理入力そのものは常に抑制する前提）。
//! yamy の `Engine::keyboardHandler` 以降の流れのうち、基本修飾・キーマップ木・
//! `key` 割付・キー出力・`&Prefix`・窓によるキーマップ選択を写している。
//!
//! 窓の情報（クラス名の列・タイトルの列）はフック DLL から届き、[`Engine::set_focus`] で入る。
//! 一致する `window` キーマップの列（yamy の `FocusOfThread::m_keymaps`）を持ち、
//! 先頭から順に引く。割付が無いキーはそのキーマップの既定動作に落ちる
//! （普通は `&KeymapParent` で親へ、`Global` は `&OtherWindowClass` で列の次へ）。

use std::collections::HashSet;

use crate::model::{
    Action, BasicMod, Config, Edge, EdgeSpec, Func, Key, KeymapId, LockMod, LockState, ModSpec,
    ModState,
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
}

/// キーマップ木の探索が戻らないときの上限（yamy と同じ 64）。
const MAX_KEYMAP_RECURSION: usize = 64;

/// 1 キーの処理中に持ち回る値（yamy の `Engine::Current`）。
#[derive(Clone, Copy)]
struct Current {
    /// 物理キー。`&Default` が使う。
    key: Key,
    /// 入力時の修飾の状態。割付の照合に使う。
    state: ModState,
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

pub struct Engine {
    config: Config,
    /// 前景窓に一致する `window` キーマップの列（照合順）。先頭が通常の解決の起点。
    focus: Vec<KeymapId>,
    /// フォーカスを持つ窓。同じ窓の通知を繰り返し受けても引き直さないために覚える。
    focus_hwnd: u64,
    /// いまキーを解決するキーマップ。通常は `focus` の先頭、`&Prefix` 後は遷移先。
    current: KeymapId,
    /// `&Prefix` を受けて次の 1 キーを待っている。
    is_prefix: bool,
    /// 物理的に押されているキー。
    pressed: HashSet<Key>,
    /// IME などの鍵の状態。外から `set_ime` で入る。分からないうちは全部オフ（yamy と同じ）。
    locks: LockState,
    /// OS に押したと伝えてまだ離していないキー。
    win32_pressed: HashSet<Key>,
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
            current,
            is_prefix: false,
            pressed: HashSet::new(),
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

        let keymap_id = self.current;
        let cur = Current {
            key,
            state,
            event_mods,
            focus_idx: 0,
        };
        self.generate_keyboard_events(keymap_id, cur, edge, 0);

        if own_mod.is_none() {
            if !self.is_prefix {
                self.current = self.root();
            } else if edge == Edge::Up {
                self.current = saved;
            }
        }

        // 物理キーが全部離れたら、OS 側に残した修飾とキーも全部離す。
        if self.pressed.is_empty() {
            self.generate_modifier_events(ModSpec::none());
            let mut rest: Vec<Key> = self.win32_pressed.drain().collect();
            rest.sort_by_key(|k| (k.prefix as u8, k.code));
            for k in rest {
                self.out.push(Output::Release(k));
            }
        }

        std::mem::take(&mut self.out)
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
        let locks = self.locks;
        let keymap = self.config.keymap(keymap_id);
        let part_of = |spec: EdgeSpec| match spec {
            EdgeSpec::Both => match edge {
                Edge::Down => Part::Down,
                Edge::Up => Part::Up,
            },
            EdgeSpec::Down | EdgeSpec::Up => Part::All,
        };
        let (actions, part) = match keymap.search(cur.key, cur.state, locks, edge) {
            Some(a) => (a.actions.clone(), part_of(a.edge)),
            None => (keymap.default_actions.clone(), part_of(EdgeSpec::Both)),
        };
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
                }
            }
            Action::Default => self.generate_default(key, event_mods, edge),
            Action::KeymapParent => match self.config.keymap(keymap_id).parent {
                Some(parent) => self.generate_keyboard_events(parent, cur, edge, depth + 1),
                None => self.generate_default(key, event_mods, edge),
            },
            Action::Ignore => {}
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
    use crate::model::{Action, Keymap, LockSpec, WindowMatch, WindowOp};

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

    /// `def mod Control = LControl RControl` と、それを継承した Global キーマップ。
    fn base() -> (Config, Keymap) {
        let mut config = Config::new(Vec::new());
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
}

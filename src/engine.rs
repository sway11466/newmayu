//! キー入力を受けて出力を決める状態機械。
//!
//! OS には依存しない。フック側はキー 1 つごとに [`Engine::feed`] を呼び、
//! 返った [`Output`] を `SendInput` に流す（物理入力そのものは常に抑制する前提）。
//! yamy の `Engine::keyboardHandler` 以降の流れのうち、基本修飾・キーマップ木・
//! `key` 割付・キー出力・`&Prefix` だけを写している。

use std::collections::HashSet;

use crate::model::{Action, BasicMod, Config, Edge, EdgeSpec, Key, KeymapId, ModSpec, ModState};

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
}

/// キーマップ木の探索が戻らないときの上限（yamy と同じ 64）。
const MAX_KEYMAP_RECURSION: usize = 64;

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
    /// いまキーを解決するキーマップ。通常は `config.root`、`&Prefix` 後は遷移先。
    current: KeymapId,
    /// `&Prefix` を受けて次の 1 キーを待っている。
    is_prefix: bool,
    /// 物理的に押されているキー。
    pressed: HashSet<Key>,
    /// OS に押したと伝えてまだ離していないキー。
    win32_pressed: HashSet<Key>,
    out: Vec<Output>,
}

impl Engine {
    pub fn new(config: Config) -> Self {
        let current = config.root;
        Engine {
            config,
            current,
            is_prefix: false,
            pressed: HashSet::new(),
            win32_pressed: HashSet::new(),
            out: Vec::new(),
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
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
                Edge::Up => self.current = self.config.root,
            }
        }

        let keymap_id = self.current;
        self.generate_keyboard_events(keymap_id, key, state, event_mods, edge, 0);

        if own_mod.is_none() {
            if !self.is_prefix {
                self.current = self.config.root;
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

    /// キーマップで割付を探して動作を出す。無ければ親へ、親も無ければ素通し。
    fn generate_keyboard_events(
        &mut self,
        keymap_id: KeymapId,
        key: Key,
        state: ModState,
        event_mods: ModSpec,
        edge: Edge,
        depth: usize,
    ) {
        if depth >= MAX_KEYMAP_RECURSION {
            return;
        }
        let keymap = self.config.keymap(keymap_id);
        match keymap.search(key, state, edge) {
            Some(a) => {
                let part = match a.edge {
                    EdgeSpec::Both => match edge {
                        Edge::Down => Part::Down,
                        Edge::Up => Part::Up,
                    },
                    EdgeSpec::Down | EdgeSpec::Up => Part::All,
                };
                let actions = a.actions.clone();
                self.generate_key_seq_events(&actions, part, key, event_mods);
            }
            None => match keymap.parent {
                Some(parent) => {
                    self.generate_keyboard_events(parent, key, state, event_mods, edge, depth + 1)
                }
                None => self.generate_default(key, event_mods, edge),
            },
        }
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

    /// `key` は割付を引いた物理キー（`&Default` が使う）。
    fn generate_key_seq_events(
        &mut self,
        actions: &[Action],
        part: Part,
        key: Key,
        event_mods: ModSpec,
    ) {
        let Some((last, head)) = actions.split_last() else {
            return;
        };
        if part == Part::Up {
            self.generate_action_events(last, Edge::Up, key, event_mods);
            return;
        }
        for a in head {
            self.generate_action_events(a, Edge::Down, key, event_mods);
            self.generate_action_events(a, Edge::Up, key, event_mods);
        }
        self.generate_action_events(last, Edge::Down, key, event_mods);
        if part == Part::All {
            self.generate_action_events(last, Edge::Up, key, event_mods);
        }
    }

    fn generate_action_events(
        &mut self,
        action: &Action,
        edge: Edge,
        key: Key,
        event_mods: ModSpec,
    ) {
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
    use crate::model::{Action, Keymap};

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
        let mut config = Config::new(Vec::new(), GLOBAL);
        config.def_mod(BasicMod::Control, [L_CONTROL, R_CONTROL]);
        let mut global = Keymap::new("Global", None);
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
        global.assign(A, ModSpec::none(), EdgeSpec::Both, vec![Action::key(B)]);
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
            vec![Action::key_with(L_CONTROL, ModSpec::any())],
        );
        // key C-H = BackSpace
        global.assign(
            H,
            ModSpec::none().with(BasicMod::Control),
            EdgeSpec::Both,
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
            vec![Action::Prefix(cx)],
        );
        // keymap EmacsC-X : Global: key F = Right
        let mut emacs_cx = Keymap::new("EmacsC-X", Some(GLOBAL));
        emacs_cx.add_mod_key(BasicMod::Control, L_CONTROL);
        emacs_cx.add_mod_key(BasicMod::Control, R_CONTROL);
        emacs_cx.assign(F, ModSpec::none(), EdgeSpec::Both, vec![Action::key(RIGHT)]);
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
        global.assign(A, ModSpec::none(), EdgeSpec::Both, vec![Action::key(B)]);
        global.assign(
            A,
            ModSpec::none().with(BasicMod::Control),
            EdgeSpec::Both,
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
    fn child_keymap_falls_back_to_parent() {
        let (config, mut global) = base();
        let child = KeymapId(1);
        global.assign(A, ModSpec::none(), EdgeSpec::Both, vec![Action::key(B)]);
        global.assign(
            X,
            ModSpec::none(),
            EdgeSpec::Both,
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

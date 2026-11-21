//! `&DescribeBindings` の書き出し（yamy の `Keymap::describe` と各 `operator<<`）。
//!
//! 窓に一致しているキーマップを順に、親までたどって、`.mayu` に近い形の行にする。
//! - キーマップの行: `keymap 名 : 親 = 既定動作` / `window 名 /クラス/ …`
//! - 最初のキーマップだけ、修飾キーの行: ` mod Shift\t= LShift RShift `
//! - 割付の行: ` key C-A\t= 動作列`（キー名、修飾の順に並べる）。前のキーマップで出した組は出さない
//! - `event` の行: ` event prefixed\t= 動作列`
//!
//! 動作列は 1 つずつ後ろに空白を付けて並べる。引数のある関数は `&名前(引数, …) ` と後ろに空白が付くので、
//! 空白が 2 つ続く（yamy と同じ）。動作列の文字列は読み込み時に作る（[`seq_text`]。`$keyseq名` は
//! 展開すると分からなくなるため）。

use std::collections::HashSet;

use crate::model::{
    Action, BasicMod, Config, EdgeSpec, EventKind, Func, Gravity, KeymapId, LockMod, LockSpec,
    ModSpec, ShowCommand, Target, ToWindow, WindowOp,
};

/// yamy の `Modifier::Type` の並び（ビットの位置）。
mod bit {
    pub const SHIFT: u32 = 0;
    pub const UP: u32 = 4;
    pub const DOWN: u32 = 5;
    pub const REPEAT: u32 = 6;
    pub const IME_LOCK: u32 = 7;
    pub const MAXIMIZED: u32 = 13;
    pub const TOUCHPAD_STICKY: u32 = 18;
    pub const MOD0: u32 = 19;
    pub const LOCK0: u32 = 29;
    pub const LOCK9: u32 = 38;
}

/// 基本修飾の yamy のビット。
fn basic_bit(m: BasicMod) -> u32 {
    match m {
        BasicMod::Shift => bit::SHIFT,
        BasicMod::Alt => 1,
        BasicMod::Control => 2,
        BasicMod::Windows => 3,
        m => bit::MOD0 + (m.index() - BasicMod::Mod0.index()) as u32,
    }
}

/// 鍵の yamy のビット。
fn lock_bit(m: LockMod) -> u32 {
    bit::IME_LOCK + m.index() as u32
}

/// yamy の `Modifier`（押されている、問わない）。比べるのと並べるのに使う。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct YamyMod {
    pressed: u64,
    dontcare: u64,
}

impl YamyMod {
    /// 割付の左辺・出力キーの修飾から。newmayu に無い修飾（R- MAX- T- L0- など）は問わない（yamy の既定）。
    fn of(mods: ModSpec, edge: EdgeSpec, locks: LockSpec) -> YamyMod {
        let mut m = YamyMod {
            pressed: 0,
            dontcare: 0,
        };
        for b in BasicMod::ALL {
            if mods.is_dontcare(b) {
                m.dontcare |= 1 << basic_bit(b);
            } else if mods.is_pressed(b) {
                m.pressed |= 1 << basic_bit(b);
            }
        }
        match edge {
            EdgeSpec::Both => m.dontcare |= (1 << bit::UP) | (1 << bit::DOWN),
            EdgeSpec::Down => {
                m.pressed |= 1 << bit::DOWN;
                m.dontcare |= 1 << bit::UP;
            }
            EdgeSpec::Up => {
                m.pressed |= 1 << bit::UP;
                m.dontcare |= 1 << bit::DOWN;
            }
        }
        for l in LockMod::ALL {
            if locks.is_dontcare(l) {
                m.dontcare |= 1 << lock_bit(l);
            } else if locks.is_on(l) {
                m.pressed |= 1 << lock_bit(l);
            }
        }
        m.dontcare |= 1 << bit::REPEAT;
        for b in bit::MAXIMIZED..=bit::TOUCHPAD_STICKY {
            m.dontcare |= 1 << b;
        }
        for b in bit::LOCK0..=bit::LOCK9 {
            m.dontcare |= 1 << b;
        }
        m
    }

    /// `event` の修飾（全部問わない）。
    fn event() -> YamyMod {
        YamyMod {
            pressed: 0,
            dontcare: u64::MAX,
        }
    }

    /// 押されていて問う修飾を、yamy の順に並べる。
    fn text(self) -> String {
        const MODS: [(u32, &str); 40] = [
            (bit::UP, "U-"),
            (bit::DOWN, "D-"),
            (0, "S-"),
            (1, "A-"),
            (2, "C-"),
            (3, "W-"),
            (bit::REPEAT, "R-"),
            (7, "IL-"),
            (8, "IC-"),
            (8, "I-"),
            (9, "NL-"),
            (10, "CL-"),
            (11, "SL-"),
            (12, "KL-"),
            (13, "MAX-"),
            (14, "MIN-"),
            (15, "MMAX-"),
            (16, "MMIN-"),
            (17, "T-"),
            (18, "TS-"),
            (19, "M0-"),
            (20, "M1-"),
            (21, "M2-"),
            (22, "M3-"),
            (23, "M4-"),
            (24, "M5-"),
            (25, "M6-"),
            (26, "M7-"),
            (27, "M8-"),
            (28, "M9-"),
            (29, "L0-"),
            (30, "L1-"),
            (31, "L2-"),
            (32, "L3-"),
            (33, "L4-"),
            (34, "L5-"),
            (35, "L6-"),
            (36, "L7-"),
            (37, "L8-"),
            (38, "L9-"),
        ];
        MODS.iter()
            .filter(|(b, _)| self.dontcare & (1 << b) == 0 && self.pressed & (1 << b) != 0)
            .map(|(_, s)| *s)
            .collect()
    }
}

/// 修飾付きのキー（`C-A` など）。
pub fn modified_key_text(name: &str, mods: ModSpec, edge: EdgeSpec, locks: LockSpec) -> String {
    format!("{}{name}", YamyMod::of(mods, edge, locks).text())
}

/// 文字列の引数（yamy の `tstringq`）。`"` で囲み、制御文字などを `\` で書く。
pub fn quoted(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '\u{7}' => out.push_str("\\a"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{b}' => out.push_str("\\v"),
            '"' => out.push_str("\\\""),
            c if c.is_control() => out.push_str(&format!("\\x{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn target(t: Target) -> &'static str {
    match t {
        Target::Overlapped => "overlapped",
        Target::Mdi => "mdi",
    }
}

fn gravity(g: Gravity) -> &'static str {
    match g {
        Gravity::Center => "C",
        Gravity::North => "N",
        Gravity::East => "E",
        Gravity::West => "W",
        Gravity::South => "S",
        Gravity::NorthWest => "NW",
        Gravity::NorthEast => "NE",
        Gravity::SouthWest => "SW",
        Gravity::SouthEast => "SE",
    }
}

fn to_window(t: ToWindow) -> &'static str {
    match t {
        ToWindow::Overlapped => "toOverlappedWindow",
        ToWindow::Main => "toMainWindow",
        ToWindow::Itself => "toItself",
        ToWindow::Parent => "toParentWindow",
    }
}

fn show(s: ShowCommand) -> &'static str {
    match s {
        ShowCommand::Hide => "hide",
        ShowCommand::Maximize => "maximize",
        ShowCommand::Minimize => "minimize",
        ShowCommand::Restore => "restore",
        ShowCommand::Show => "show",
        ShowCommand::ShowDefault => "showDefault",
        ShowCommand::ShowMaximized => "showMaximized",
        ShowCommand::ShowMinimized => "showMinimized",
        ShowCommand::ShowMinNoActive => "showMinNoActive",
        ShowCommand::ShowNA => "showNA",
        ShowCommand::ShowNoActivate => "showNoActivate",
        ShowCommand::ShowNormal => "showNormal",
    }
}

/// 引数のある関数（yamy の makefunc が作る `output`。後ろに空白が付く）。
fn func(name: &str, args: &[String]) -> String {
    format!("&{name}({}) ", args.join(", "))
}

/// `&VK` の引数（yamy の `operator<<(VKey)`）。
fn vkey(vk: u8, extended: bool, down: bool, up: bool) -> String {
    let mut s = String::new();
    if extended {
        s.push_str("E-");
    }
    // 押し・離しの両方（指定なし）は yamy では印が無い
    if up && !down {
        s.push_str("U-");
    }
    if down && !up {
        s.push_str("D-");
    }
    match crate::config::vkeys::VKEYS.iter().find(|(_, c)| *c == vk) {
        Some((name, _)) => s.push_str(name),
        None => s.push_str(&format!("0x{vk:x}")),
    }
    s
}

/// 窓の関数（yamy の `&WindowClingToLeft` 等は読み込み時に別に作る）。
fn func_text(f: Func) -> String {
    let t = |x: Target| target(x).to_string();
    match f {
        Func::WindowRaise(x) => func("WindowRaise", &[t(x)]),
        Func::WindowLower(x) => func("WindowLower", &[t(x)]),
        Func::WindowMinimize(x) => func("WindowMinimize", &[t(x)]),
        Func::WindowMaximize(x) => func("WindowMaximize", &[t(x)]),
        Func::WindowHMaximize(x) => func("WindowHMaximize", &[t(x)]),
        Func::WindowVMaximize(x) => func("WindowVMaximize", &[t(x)]),
        Func::WindowMove { dx, dy, target: x } => {
            func("WindowMove", &[dx.to_string(), dy.to_string(), t(x)])
        }
        Func::WindowMoveTo {
            gravity: g,
            dx,
            dy,
            target: x,
        } => func(
            "WindowMoveTo",
            &[gravity(g).to_string(), dx.to_string(), dy.to_string(), t(x)],
        ),
        Func::WindowMoveVisibly(x) => func("WindowMoveVisibly", &[t(x)]),
        Func::WindowClose(x) => func("WindowClose", &[t(x)]),
        Func::WindowToggleTopMost => "&WindowToggleTopMost".to_string(),
        Func::WindowSetAlpha(n) => func("WindowSetAlpha", &[n.to_string()]),
        Func::WindowRedraw => "&WindowRedraw".to_string(),
        Func::Recenter => "&Recenter".to_string(),
        Func::WindowIdentify => "&WindowIdentify".to_string(),
        Func::WindowResizeTo {
            width,
            height,
            target: x,
        } => func(
            "WindowResizeTo",
            &[width.to_string(), height.to_string(), t(x)],
        ),
        Func::MouseMove { dx, dy } => func("MouseMove", &[dx.to_string(), dy.to_string()]),
        Func::MouseWheel(d) => func("MouseWheel", &[d.to_string()]),
        Func::Undefined => "&Undefined".to_string(),
        Func::PostMessage {
            to,
            message,
            wparam,
            lparam,
        } => func(
            "PostMessage",
            &[
                to_window(to).to_string(),
                message.to_string(),
                wparam.to_string(),
                lparam.to_string(),
            ],
        ),
        Func::ClipboardUpcaseWord => "&ClipboardUpcaseWord".to_string(),
        Func::ClipboardDowncaseWord => "&ClipboardDowncaseWord".to_string(),
    }
}

/// `&WindowClingToLeft` 等（newmayu は `&WindowMoveTo` として持つので、読み込み時に名前で作る）。
pub fn cling_text(name: &str, t: Target) -> String {
    func(name, &[target(t).to_string()])
}

/// `&Repeat(動作列, 上限)`。動作列は `$名`（`( … )` なら `$`。yamy の無名の keyseq）。
///
/// yamy は動作列の引数をメモリの番地で出すが、実装の都合が漏れたものと想定し、名前で出すようにした。
pub fn repeat_text(seq: &str, max: i32) -> String {
    func("Repeat", &[seq.to_string(), max.to_string()])
}

/// `&EmacsEditKillLinePred(動作列1, 動作列2)`。
pub fn kill_line_pred_text(seq1: &str, seq2: &str) -> String {
    func(
        "EmacsEditKillLinePred",
        &[seq1.to_string(), seq2.to_string()],
    )
}

/// 1 つの動作（yamy の `Action::output`）。`$keyseq名` と、名前の分からない関数（`&Repeat` など）は
/// 読み込み時に別に作る。
pub fn action_text(
    a: &Action,
    key_name: &dyn Fn(crate::model::Key) -> String,
    keymap_name: &dyn Fn(KeymapId) -> String,
) -> String {
    match a {
        Action::Key {
            key,
            mods,
            edge,
            locks,
        } => modified_key_text(&key_name(*key), *mods, *edge, *locks),
        Action::Prefix(k, ignore) => func("Prefix", &[keymap_name(*k), ignore.to_string()]),
        Action::Keymap(k) => func("Keymap", &[keymap_name(*k)]),
        Action::Default => "&Default".to_string(),
        Action::KeymapParent => "&KeymapParent".to_string(),
        Action::OtherWindowClass => "&OtherWindowClass".to_string(),
        Action::KeymapWindow => "&KeymapWindow".to_string(),
        Action::Func(f) => func_text(*f),
        Action::Ignore => "&Ignore".to_string(),
        Action::Vk {
            vk,
            extended,
            down,
            up,
        } => func("VK", &[vkey(*vk, *extended, *down, *up)]),
        Action::Variable { mag, inc } => func("Variable", &[mag.to_string(), inc.to_string()]),
        // 動作列の引数は読み込み時に作る。ここに来るのは読み込みを通らない（テストで組んだ）もの
        Action::Repeat { max, .. } => repeat_text("$", *max),
        Action::EmacsEditKillLineFunc => "&EmacsEditKillLineFunc".to_string(),
        Action::EmacsEditKillLinePred { .. } => kill_line_pred_text("$", "$"),
        Action::Sync => "&Sync".to_string(),
        Action::HelpMessage(text) => {
            let (title, body) = match text {
                Some(t) => (t.title.as_str(), t.body.as_str()),
                None => ("", ""),
            };
            func("HelpMessage", &[quoted(title), quoted(body)])
        }
        Action::HelpVariable(title) => func("HelpVariable", &[quoted(title)]),
        Action::ShellExecute(s) => func(
            "ShellExecute",
            &[
                quoted(&s.operation),
                quoted(&s.file),
                quoted(&s.parameters),
                quoted(&s.directory),
                show(s.show).to_string(),
            ],
        ),
        Action::MayuDialog(s) => func("MayuDialog", &["log".to_string(), show(*s).to_string()]),
        Action::LogClear => "&LogClear".to_string(),
        Action::InvestigateCommand => "&InvestigateCommand".to_string(),
        Action::LoadSetting(name) => func("LoadSetting", &[quoted(name)]),
        Action::DescribeBindings => "&DescribeBindings".to_string(),
        Action::EditNextModifier(mods, locks) => func(
            "EditNextModifier",
            &[YamyMod::of(*mods, EdgeSpec::Both, *locks).text()],
        ),
    }
}

/// 動作列（yamy の `operator<<(KeySeq)`）。1 つずつ後ろに空白を付ける。
pub fn seq_text(items: &[String]) -> String {
    items.iter().map(|s| format!("{s} ")).collect()
}

fn event_name(kind: EventKind) -> &'static str {
    match kind {
        EventKind::Prefixed => "prefixed",
        EventKind::BeforeKeyDown => "before-key-down",
        EventKind::AfterKeyUp => "after-key-up",
    }
}

fn mod_type_name(m: BasicMod) -> &'static str {
    match m {
        BasicMod::Shift => "Shift",
        BasicMod::Alt => "Alt",
        BasicMod::Control => "Control",
        BasicMod::Windows => "Windows",
        BasicMod::Mod0 => "Mod0",
        BasicMod::Mod1 => "Mod1",
        BasicMod::Mod2 => "Mod2",
        BasicMod::Mod3 => "Mod3",
        BasicMod::Mod4 => "Mod4",
        BasicMod::Mod5 => "Mod5",
        BasicMod::Mod6 => "Mod6",
        BasicMod::Mod7 => "Mod7",
        BasicMod::Mod8 => "Mod8",
        BasicMod::Mod9 => "Mod9",
    }
}

/// 割付・event を照合する組（yamy の `ModifiedKey`。キーは名前で持つ）。
#[derive(Clone, PartialEq, Eq, Hash)]
enum Bound {
    Key(crate::model::Key, YamyMod),
    Event(EventKind),
}

/// いま窓に一致しているキーマップ `focus` の割付を行にする（yamy の `Engine::describeBindings`）。
pub fn describe(config: &Config, focus: &[KeymapId]) -> Vec<String> {
    let mut d = Describer {
        config,
        lines: Vec::new(),
        keymaps: HashSet::new(),
        bound: HashSet::new(),
        with_mods: true,
    };
    for &k in focus {
        d.keymap(k);
    }
    d.lines.push(String::new());
    d.lines
}

struct Describer<'a> {
    config: &'a Config,
    lines: Vec<String>,
    /// 出したキーマップ。
    keymaps: HashSet<usize>,
    /// 出した割付。
    bound: HashSet<Bound>,
    /// 修飾キーの行を出すか（最初のキーマップだけ）。
    with_mods: bool,
}

impl Describer<'_> {
    fn key_name(&self, key: crate::model::Key) -> String {
        self.config.key_name(key)
    }

    fn keymap(&mut self, id: KeymapId) {
        if !self.keymaps.insert(id.0) {
            return;
        }
        let km = &self.config.keymaps[id.0];
        let mut head = match &km.window {
            None => format!("keymap {}", km.name),
            Some(w) => match w.op() {
                WindowOp::And if w.title_src() == ".*" => {
                    format!("window {} /{}/", km.name, w.class_src())
                }
                WindowOp::And => format!(
                    "window {} ( /{}/ && /{}/ )",
                    km.name,
                    w.class_src(),
                    w.title_src()
                ),
                WindowOp::Or => format!(
                    "window {} ( /{}/ || /{}/ )",
                    km.name,
                    w.class_src(),
                    w.title_src()
                ),
            },
        };
        if let Some(p) = km.parent {
            head.push_str(&format!(" : {}", self.config.keymaps[p.0].name));
        }
        head.push_str(&format!(" = {}", km.default_text));
        self.lines.push(head);

        if self.with_mods {
            for m in BasicMod::ALL {
                let keys = &km.mod_keys[m.index()];
                if keys.is_empty() {
                    continue;
                }
                let mut line = format!(" mod {}\t= ", mod_type_name(m));
                for k in keys {
                    if km.is_one_shot(m, *k) {
                        line.push_str("!!");
                    }
                    line.push_str(&self.key_name(*k));
                    line.push(' ');
                }
                self.lines.push(line);
            }
            self.with_mods = false;
        }

        // キー名（大文字小文字を区別しない）、修飾の順（yamy の `ModifiedKey::operator<`）
        let mut items: Vec<(String, YamyMod, Bound, String)> = Vec::new();
        for a in &km.assignments {
            let m = YamyMod::of(a.mods, a.edge, a.locks);
            let name = self.key_name(a.key);
            let line = format!(
                " key {}\t= {}",
                modified_key_text(&name, a.mods, a.edge, a.locks),
                a.text
            );
            items.push((name, m, Bound::Key(a.key, m), line));
        }
        for kind in EventKind::ALL {
            if km.event(kind).is_some() {
                let name = event_name(kind);
                let line = format!(" event {name}\t= {}", km.event_texts[kind.index()]);
                items.push((name.to_string(), YamyMod::event(), Bound::Event(kind), line));
            }
        }
        items.sort_by(|a, b| {
            a.0.to_lowercase()
                .cmp(&b.0.to_lowercase())
                .then(a.1.cmp(&b.1))
        });
        for (_, _, bound, line) in items {
            if self.bound.insert(bound) {
                self.lines.push(line);
            }
        }
        self.lines.push(String::new());

        if let Some(p) = km.parent {
            self.keymap(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{HelpText, Key, Keymap, ShellExecute, WindowMatch};

    const A: Key = Key::plain(0x1e);
    const B: Key = Key::plain(0x30);

    fn names(k: Key) -> String {
        if k == A { "A" } else { "B" }.to_string()
    }

    fn kmn(_: KeymapId) -> String {
        "Sub".to_string()
    }

    fn t(a: &Action) -> String {
        action_text(a, &names, &kmn)
    }

    #[test]
    fn modifiers_follow_yamy_order() {
        let mods = ModSpec::none()
            .with(BasicMod::Control)
            .with(BasicMod::Shift)
            .with(BasicMod::Mod1);
        assert_eq!(
            modified_key_text("A", mods, EdgeSpec::Down, LockSpec::any()),
            "D-S-C-M1-A"
        );
        // IC- は yamy と同じく IC-I- と 2 回出る
        assert_eq!(
            modified_key_text(
                "A",
                ModSpec::none(),
                EdgeSpec::Both,
                LockSpec::any()
                    .with(LockMod::ImeComp)
                    .with(LockMod::ImeLock)
            ),
            "IL-IC-I-A"
        );
        // 問わない修飾は出ない
        assert_eq!(
            modified_key_text("A", ModSpec::any(), EdgeSpec::Both, LockSpec::any()),
            "A"
        );
    }

    #[test]
    fn functions_are_like_yamy() {
        assert_eq!(t(&Action::Sync), "&Sync");
        assert_eq!(t(&Action::Prefix(KeymapId(1), true)), "&Prefix(Sub, true) ");
        assert_eq!(
            t(&Action::Prefix(KeymapId(1), false)),
            "&Prefix(Sub, false) "
        );
        assert_eq!(
            t(&Action::Func(Func::WindowMoveTo {
                gravity: Gravity::NorthWest,
                dx: 1,
                dy: -2,
                target: Target::Mdi
            })),
            "&WindowMoveTo(NW, 1, -2, mdi) "
        );
        assert_eq!(
            t(&Action::Vk {
                vk: 0x0d,
                extended: true,
                down: true,
                up: false
            }),
            "&VK(E-D-RETURN) "
        );
        assert_eq!(
            t(&Action::Vk {
                vk: 0xff,
                extended: false,
                down: true,
                up: true
            }),
            "&VK(0xff) "
        );
        assert_eq!(
            t(&Action::HelpMessage(Some(HelpText {
                title: "t".into(),
                body: "a\"b\n".into()
            }))),
            "&HelpMessage(\"t\", \"a\\\"b\\n\") "
        );
        assert_eq!(t(&Action::HelpMessage(None)), "&HelpMessage(\"\", \"\") ");
        assert_eq!(
            t(&Action::ShellExecute(ShellExecute {
                operation: "open".into(),
                file: "a b".into(),
                parameters: String::new(),
                directory: String::new(),
                show: ShowCommand::ShowNormal
            })),
            "&ShellExecute(\"open\", \"a b\", \"\", \"\", showNormal) "
        );
        assert_eq!(
            t(&Action::MayuDialog(ShowCommand::Show)),
            "&MayuDialog(log, show) "
        );
        assert_eq!(
            t(&Action::Func(Func::PostMessage {
                to: ToWindow::Main,
                message: 274,
                wparam: 61536,
                lparam: -1
            })),
            "&PostMessage(toMainWindow, 274, 61536, -1) "
        );
        assert_eq!(
            cling_text("WindowClingToLeft", Target::Overlapped),
            "&WindowClingToLeft(overlapped) "
        );
        assert_eq!(repeat_text("$Seq", 10), "&Repeat($Seq, 10) ");
        assert_eq!(
            seq_text(&["A".into(), "&Prefix(Sub, true) ".into()]),
            "A &Prefix(Sub, true)  "
        );
    }

    #[test]
    fn describe_walks_keymaps_and_skips_described_keys() {
        let mut global = Keymap::global();
        global.add_mod_key(BasicMod::Shift, A);
        global.assign_text(
            B,
            ModSpec::none(),
            EdgeSpec::Both,
            LockSpec::assign_default(),
            vec![Action::key(A)],
            "A ".into(),
        );
        global.assign_text(
            A,
            ModSpec::none().with(BasicMod::Control),
            EdgeSpec::Both,
            LockSpec::assign_default(),
            vec![Action::key(B)],
            "B ".into(),
        );
        let mut win = Keymap::new("W", Some(KeymapId(0)));
        win.add_mod_key(BasicMod::Shift, A);
        win.window = Some(WindowMatch::new("notepad", None, WindowOp::And).unwrap());
        win.assign_text(
            B,
            ModSpec::none(),
            EdgeSpec::Both,
            LockSpec::assign_default(),
            vec![Action::Sync],
            "&Sync ".into(),
        );
        win.set_event_text(EventKind::Prefixed, vec![Action::Sync], "&Sync ".into());
        let mut config = Config::new(vec![global, win]);
        config.key_names = vec![(A, "A".into()), (B, "B".into())];
        let lines = describe(&config, &[KeymapId(1), KeymapId(0)]);
        assert_eq!(
            lines,
            [
                "window W /notepad/ : Global = &KeymapParent ",
                " mod Shift\t= A ",
                " key B\t= &Sync ",
                " event prefixed\t= &Sync ",
                "",
                "window Global ( /.*/ || /.*/ ) = &OtherWindowClass ",
                // B は W で出したので出さない。A の後に B の順
                " key C-A\t= B ",
                "",
                "",
            ]
        );
    }
}

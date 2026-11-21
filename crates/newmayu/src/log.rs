//! ログ。本体が出す文はすべてここを通す。
//!
//! 行き先は起動時に [`init`] で決める。`--log <path>` があればそのファイルに追記、
//! 無ければ標準エラー。各行の先頭にローカル時刻（`HH:MM:SS.mmm`）を付ける。
//!
//! ログダイアログ（[`crate::log_dialog`]）にも同じ行を渡す。[`start_view`] から溜め始め、
//! [`attach_view`] で渡した関数で「溜まった」と知らせる。ダイアログは [`take_view`] で受け取る。
//! ダイアログへは yamy と同じく、詳細（[`Level::Detail`]）の行は「詳細」が付いているときだけ渡す。
//! 付いていないときの詳細の行は捨てる（後から付けても出ない）。ファイルと標準エラーには全部書く。
//!
//! 使うときは [`log!`] マクロ（詳細は [`log_detail!`]）。`eprintln!` と同じ書き方。

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

use windows::Win32::System::SystemInformation::GetLocalTime;

/// 行の行き先。
enum Sink {
    Stderr,
    File(File),
}

static SINK: Mutex<Sink> = Mutex::new(Sink::Stderr);

/// 行の詳細度（yamy のログの debug level。0 が常に出す、1 が詳細）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Normal,
    Detail,
}

/// ログダイアログへ渡す行。
struct View {
    /// 溜めるか（[`start_view`] から）。
    collecting: bool,
    /// 詳細の行も溜めるか（ダイアログの「詳細」）。
    detail: bool,
    /// まだダイアログが受け取っていない行。
    pending: String,
    /// 空だった `pending` に行が入ったときに呼ぶ。
    notify: Option<Box<dyn Fn() + Send>>,
}

impl View {
    const fn new() -> View {
        View {
            collecting: false,
            detail: false,
            pending: String::new(),
            notify: None,
        }
    }

    /// 1 行溜める。知らせる必要があれば `true`。
    fn push(&mut self, level: Level, line: &str) -> bool {
        if !self.collecting || (level == Level::Detail && !self.detail) {
            return false;
        }
        let was_empty = self.pending.is_empty();
        self.pending.push_str(line);
        was_empty
    }
}

static VIEW: Mutex<View> = Mutex::new(View::new());

fn view() -> std::sync::MutexGuard<'static, View> {
    VIEW.lock().unwrap_or_else(|e| e.into_inner())
}

/// ログダイアログへ渡す行を溜め始める。
pub fn start_view() {
    view().collecting = true;
}

/// 行が溜まったら `notify` を呼ぶ。すでに溜まっていればすぐ呼ぶ。
/// `notify` はログを書いたスレッドで、ロックを持ったまま呼ぶ（`PostMessage` のように、すぐ返すこと）。
pub fn attach_view(notify: Box<dyn Fn() + Send>) {
    let mut v = view();
    if !v.pending.is_empty() {
        notify();
    }
    v.notify = Some(notify);
}

/// ログダイアログへ渡すのをやめる。
pub fn detach_view() {
    let mut v = view();
    v.notify = None;
    v.collecting = false;
    v.pending.clear();
}

/// 溜まった行を受け取る。
pub fn take_view() -> String {
    std::mem::take(&mut view().pending)
}

/// 詳細の行もダイアログへ渡すか（yamy の「詳細」）。
pub fn set_detail(on: bool) {
    view().detail = on;
}

/// 行き先を決める。`path` が `Some` ならそのファイルに追記（無ければ作る）。
/// 開けなければ `Err`。呼ばなければ標準エラー。
pub fn init(path: Option<&Path>) -> Result<(), String> {
    let sink = match path {
        None => Sink::Stderr,
        Some(p) => {
            let f = OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)
                .map_err(|e| format!("{} を開けない: {e}", p.display()))?;
            Sink::File(f)
        }
    };
    *SINK.lock().unwrap_or_else(|e| e.into_inner()) = sink;
    Ok(())
}

/// 1 行書く。[`log!`] / [`log_detail!`] から呼ぶ。
pub fn write(level: Level, args: fmt::Arguments<'_>) {
    let line = format_line(local_time(), args);
    {
        let mut sink = SINK.lock().unwrap_or_else(|e| e.into_inner());
        match &mut *sink {
            Sink::Stderr => {
                let mut e = std::io::stderr().lock();
                let _ = e.write_all(line.as_bytes());
            }
            Sink::File(f) => {
                let _ = f.write_all(line.as_bytes());
                let _ = f.flush();
            }
        }
    }
    let mut v = view();
    if v.push(level, &line)
        && let Some(n) = &v.notify
    {
        n();
    }
}

/// 時刻を付けて 1 行にする（末尾に改行を含む）。
fn format_line(t: (u16, u16, u16, u16), args: fmt::Arguments<'_>) -> String {
    let (h, m, s, ms) = t;
    format!("{h:02}:{m:02}:{s:02}.{ms:03} {args}\n")
}

/// ローカル時刻（時, 分, 秒, ミリ秒）。
fn local_time() -> (u16, u16, u16, u16) {
    // SAFETY: 引数なし、現在時刻を返すだけ。
    let t = unsafe { GetLocalTime() };
    (t.wHour, t.wMinute, t.wSecond, t.wMilliseconds)
}

/// ログを 1 行書く。`log!("{x} -> {y}")` のように `eprintln!` と同じに使う。
#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {
        $crate::log::write($crate::log::Level::Normal, format_args!($($arg)*))
    };
}

/// 詳細のログを 1 行書く（キー変換の過程・フォーカスの変化など。yamy の debug level 1）。
#[macro_export]
macro_rules! log_detail {
    ($($arg:tt)*) => {
        $crate::log::write($crate::log::Level::Detail, format_args!($($arg)*))
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 時刻は桁を揃えて先頭に付く() {
        let s = format_line((9, 5, 3, 7), format_args!("a {}", 1));
        assert_eq!(s, "09:05:03.007 a 1\n");
    }

    #[test]
    fn ファイルに追記する() {
        let dir = std::env::temp_dir().join(format!("newmayu-log-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.log");
        std::fs::write(&path, "before\n").unwrap();
        init(Some(&path)).unwrap();
        log!("hello {}", 42);
        init(None).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let mut lines = body.lines();
        assert_eq!(lines.next(), Some("before"));
        let l = lines.next().unwrap();
        assert_eq!(&l[12..], " hello 42");
        assert_eq!(l.as_bytes()[2], b':');
        assert_eq!(l.as_bytes()[8], b'.');
    }

    #[test]
    fn ダイアログへは溜め始めてから渡し_詳細は付いているときだけ() {
        let mut v = View::new();
        assert!(!v.push(
            Level::Normal,
            "a
"
        ));
        v.collecting = true;
        assert!(v.push(
            Level::Normal,
            "b
"
        ));
        // 空でなければ知らせ直さない
        assert!(!v.push(
            Level::Normal,
            "c
"
        ));
        assert!(!v.push(
            Level::Detail,
            "d
"
        ));
        v.detail = true;
        assert!(!v.push(
            Level::Detail,
            "e
"
        ));
        assert_eq!(
            v.pending,
            "b
c
e
"
        );
    }

    #[test]
    fn 開けない場所はエラー() {
        let path = Path::new("Z:/newmayu/no/such/dir/t.log");
        assert!(init(Some(path)).is_err());
    }
}

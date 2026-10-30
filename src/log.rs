//! ログ。本体が出す文はすべてここを通す。
//!
//! 行き先は起動時に [`init`] で決める。`--log <path>` があればそのファイルに追記、
//! 無ければ標準エラー。各行の先頭にローカル時刻（`HH:MM:SS.mmm`）を付ける。
//!
//! 使うときは [`log!`] マクロ。`eprintln!` と同じ書き方。

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

/// 1 行書く。[`log!`] から呼ぶ。
pub fn write(args: fmt::Arguments<'_>) {
    let line = format_line(local_time(), args);
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
        $crate::log::write(format_args!($($arg)*))
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
    fn 開けない場所はエラー() {
        let path = Path::new("Z:/newmayu/no/such/dir/t.log");
        assert!(init(Some(path)).is_err());
    }
}

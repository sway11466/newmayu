//! ログの見出しに出すビルドの情報（yamy の `LOGNAME`・`COMPUTERNAME`・`__DATE__`・`__TIME__`・コンパイラ）。

use std::process::Command;

#[repr(C)]
#[derive(Default)]
struct SystemTime {
    year: u16,
    month: u16,
    day_of_week: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
    milliseconds: u16,
}

unsafe extern "system" {
    fn GetLocalTime(st: *mut SystemTime);
}

fn main() {
    let user = std::env::var("USERNAME").unwrap_or_default();
    let host = std::env::var("COMPUTERNAME")
        .unwrap_or_default()
        .to_lowercase();
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let compiler = Command::new(rustc)
        .arg("-V")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let mut t = SystemTime::default();
    // SAFETY: t は書き込める SYSTEMTIME と同じ並び。
    unsafe { GetLocalTime(&mut t) };
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    // __DATE__ と __TIME__ の形
    let date = format!(
        "{} {:>2} {}",
        MONTHS[(t.month as usize).clamp(1, 12) - 1],
        t.day,
        t.year
    );
    let time = format!("{:02}:{:02}:{:02}", t.hour, t.minute, t.second);
    println!("cargo:rustc-env=NEWMAYU_BUILT_BY={user}@{host} ({date} {time}, {compiler})");
}

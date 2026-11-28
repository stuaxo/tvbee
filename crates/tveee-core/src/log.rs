//! A debug log: with `TVEEE_LOG=<file>` set (in the environment, `.env` or
//! `~/.config/tveee/env`), tveee appends what it's doing there: adb and HTTPS
//! calls with how long they took, and things that went wrong that are
//! otherwise only a status line (or nothing) in the ui. Off by default: the
//! `tlog!` calls then cost a check of a flag.
//!
//! An interactive front end can also keep the last lines in memory
//! ([`keep_recent`], [`recent`]) to show them, with or without a file.

use std::io::Write;
use std::sync::{Mutex, OnceLock};

static FILE: OnceLock<Option<Mutex<std::fs::File>>> = OnceLock::new();

fn file() -> Option<&'static Mutex<std::fs::File>> {
    FILE.get_or_init(|| {
        let path = std::env::var_os("TVEEE_LOG").filter(|p| !p.is_empty())?;
        let f = std::fs::OpenOptions::new().create(true).append(true).open(path).ok()?;
        Some(Mutex::new(f))
    })
    .as_ref()
}

/// How many lines [`recent`] keeps.
pub const RECENT: usize = 400;

static KEEP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static LINES: Mutex<std::collections::VecDeque<String>> = Mutex::new(std::collections::VecDeque::new());
/// How many lines have been kept since the start (the oldest kept is
/// number TOTAL - its length): a line's number stays the same while newer
/// ones push older ones out.
static TOTAL: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Keep the last [`RECENT`] lines in memory from now on (a front end
/// showing them).
pub fn keep_recent() {
    KEEP.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// The lines kept, oldest first (none unless [`keep_recent`] was called).
pub fn recent() -> Vec<String> {
    LINES.lock().map(|l| l.iter().cloned().collect()).unwrap_or_default()
}

/// The lines kept, oldest first, and the number of the first (counting
/// every line kept since the start; see [`recent`]).
pub fn recent_numbered() -> (usize, Vec<String>) {
    let Ok(l) = LINES.lock() else { return (0, Vec::new()) };
    let total = TOTAL.load(std::sync::atomic::Ordering::Relaxed);
    (total.saturating_sub(l.len()), l.iter().cloned().collect())
}

/// Whether there's a log to write to (a file, or lines kept in memory).
pub fn on() -> bool {
    KEEP.load(std::sync::atomic::Ordering::Relaxed) || file().is_some()
}

/// A log line: the time, the thread, then `msg`.
pub fn line(now: std::time::Duration, thread: &str, msg: &str) -> String {
    let (secs, ms) = (now.as_secs(), now.subsec_millis());
    // the time of day (UTC) is enough to line things up
    let (h, m, s) = (secs / 3600 % 24, secs / 60 % 60, secs % 60);
    format!("{h:02}:{m:02}:{s:02}.{ms:03} [{thread}] {msg}\n")
}

pub fn write(args: std::fmt::Arguments) {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let t = std::thread::current();
    let name = t.name().map_or_else(|| format!("{:?}", t.id()), str::to_string);
    let text = line(now, &name, &args.to_string());
    if KEEP.load(std::sync::atomic::Ordering::Relaxed)
        && let Ok(mut l) = LINES.lock()
    {
        if l.len() >= RECENT {
            l.pop_front();
        }
        l.push_back(text.trim_end().to_string());
        TOTAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    if let Some(Ok(mut f)) = file().map(Mutex::lock) {
        let _ = f.write_all(text.as_bytes());
    }
}

/// Write a line to the debug log, if there is one (`TVEEE_LOG`).
#[macro_export]
macro_rules! tlog {
    ($($t:tt)*) => {
        if $crate::log::on() {
            $crate::log::write(format_args!($($t)*));
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines() {
        let at = std::time::Duration::from_millis((13 * 3600 + 4 * 60 + 5) * 1000 + 67);
        assert_eq!(line(at, "refresh", "adb shell dumpsys power: 158ms"), "13:04:05.067 [refresh] adb shell dumpsys power: 158ms\n");
    }
}

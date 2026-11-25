//! Recording what the TV answers, and answering from the recording, for
//! working on tveee without the TV (guide parsing, Kodi's replies, how the
//! ui lays things out).
//!
//! `TVEEE_MOCK_DIR=<dir>` turns it on. With `TVEEE_MOCK=record` every adb
//! command, HTTP GET and HTTPS request (JointSpace, Kodi) is made as usual
//! and its answer saved in `<dir>`; otherwise (replay) they're answered from
//! `<dir>`, and one that wasn't recorded fails, saying which. Answers are
//! keyed by what was asked (not the TV's address), so a recording works for
//! any TV_IP.
//!
//! Replay with the TV_IP it was recorded with: tveee keeps pairings and
//! which plugins a TV has per address, and those aren't in the recording
//! (or set TVEEE_PLUGINS). A recording holds what the TV said, which can
//! include pairing replies: keep it to yourself, or don't pair while
//! recording.
//!
//! Not covered: streams (`adb shell` sessions, scrcpy, devtools websockets,
//! Kodi's TCP notifications); those reach the TV as usual, or fail without it.

use crate::error::{Error, Result};
use std::path::PathBuf;

enum Mode {
    Off,
    Record(PathBuf),
    Replay(PathBuf),
}

fn mode() -> &'static Mode {
    static MODE: std::sync::OnceLock<Mode> = std::sync::OnceLock::new();
    MODE.get_or_init(|| {
        let Some(dir) = std::env::var_os("TVEEE_MOCK_DIR").filter(|d| !d.is_empty()) else { return Mode::Off };
        let dir = PathBuf::from(dir);
        if std::env::var("TVEEE_MOCK").is_ok_and(|m| m == "record") {
            let _ = std::fs::create_dir_all(&dir);
            Mode::Record(dir)
        } else {
            Mode::Replay(dir)
        }
    })
}

/// FNV-1a: stable across runs and builds (std's hasher isn't promised to be).
fn fnv(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
}

/// The file a request's answer is kept in: a readable start (the first
/// words, tidied) and a hash of all of it.
pub fn file_name(what: &[&str]) -> String {
    let all = what.join("\u{1f}");
    let slug: String = what
        .iter()
        .flat_map(|w| w.split(|c: char| !c.is_ascii_alphanumeric()))
        .filter(|w| !w.is_empty())
        .take(6)
        .collect::<Vec<_>>()
        .join("-");
    let slug: String = slug.chars().take(60).collect();
    format!("{slug}-{:016x}", fnv(&all))
}

/// In replay: the recorded answer to `what` (Err: it failed then, or it
/// wasn't recorded). None when not replaying.
pub fn replay(what: &[&str]) -> Option<Result<Vec<u8>>> {
    let Mode::Replay(dir) = mode() else { return None };
    let base = dir.join(file_name(what));
    if let Ok(bytes) = std::fs::read(base.with_extension("out")) {
        return Some(Ok(bytes));
    }
    Some(Err(Error::Failed(match std::fs::read_to_string(base.with_extension("err")) {
        Ok(why) => why,
        Err(_) => format!("not recorded in TVEEE_MOCK_DIR: {} ({})", what.join(" "), base.display()),
    })))
}

/// While recording: keep `answer` to `what`.
pub fn record(what: &[&str], answer: std::result::Result<&[u8], String>) {
    let Mode::Record(dir) = mode() else { return };
    let base = dir.join(file_name(what));
    let _ = match answer {
        Ok(bytes) => std::fs::write(base.with_extension("out"), bytes),
        Err(why) => std::fs::write(base.with_extension("err"), why),
    };
}

/// An HTTP answer as kept: the status on the first line, then the body.
pub fn http_bytes(status: u16, body: &str) -> Vec<u8> {
    format!("{status}\n{body}").into_bytes()
}

/// An HTTP answer from how it's kept.
pub fn http_answer(bytes: &[u8]) -> Result<(u16, String)> {
    let text = String::from_utf8_lossy(bytes);
    let (status, body) = text.split_once('\n').unwrap_or((&text, ""));
    match status.parse() {
        Ok(s) => Ok((s, body.to_string())),
        Err(_) => Err(Error::Failed("TVEEE_MOCK_DIR: a recorded HTTP answer without its status".into())),
    }
}

/// A URL without its scheme and host, so a recording works for any TV_IP:
/// `https://192.168.1.19:1926/6/system` -> `:1926/6/system`.
pub fn url_key(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let path_at = rest.find('/').unwrap_or(rest.len());
    let host_port = &rest[..path_at];
    let port_at = host_port.rfind(':').unwrap_or(host_port.len());
    &rest[port_at..]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_stable_and_readable() {
        let n = file_name(&["adb", "shell", "dumpsys", "media_session"]);
        assert!(n.starts_with("adb-shell-dumpsys-media-session-"), "{n}");
        assert_eq!(n, file_name(&["adb", "shell", "dumpsys", "media_session"]));
        assert_ne!(n, file_name(&["adb", "shell", "dumpsys", "audio"]));
        // the words are kept apart: ["a b"] isn't ["a", "b"]
        assert_ne!(file_name(&["a b"]), file_name(&["a", "b"]));
    }

    #[test]
    fn urls_lose_their_host() {
        assert_eq!(url_key("https://192.168.1.19:1926/6/system"), ":1926/6/system");
        assert_eq!(url_key("http://tv.local/jsonrpc"), "/jsonrpc");
        assert_eq!(url_key("http://10.0.0.2:8080"), ":8080");
    }

    #[test]
    fn http_round_trips() {
        assert_eq!(http_answer(&http_bytes(200, "{\"a\":1}\nmore")).unwrap(), (200, "{\"a\":1}\nmore".to_string()));
        assert!(http_answer(b"nonsense").is_err());
    }
}

//! Keep a report on screen, refreshing it, until the user presses q.
//!
//! No crates: the terminal is switched to non-canonical, no-echo mode with
//! `stty` and drawn on the alternate screen with ANSI escapes. Fetching runs on
//! a background thread so keys stay responsive while the TV is slow.

use crate::error::{usage, Result};
use std::io::{IsTerminal, Read, Write};
use std::process::{Command as Proc, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

/// What a key press asks for.
#[derive(Debug, PartialEq)]
pub enum Key {
    Quit,
    Refresh,
    Other,
}

/// q / Q / Esc / Ctrl-C / Ctrl-D quit; r refreshes now.
pub fn key(b: u8) -> Key {
    match b {
        b'q' | b'Q' | 0x1b | 0x03 | 0x04 => Key::Quit,
        b'r' | b'R' => Key::Refresh,
        _ => Key::Other,
    }
}

/// One frame: a header line, then the body, each line cleared to its end so a
/// shorter frame doesn't leave bits of the last one behind.
pub fn frame(header: &str, body: &str, error: Option<&str>) -> String {
    let mut out = String::from("\x1b[H");
    let mut push = |l: &str| {
        out.push_str(l);
        out.push_str("\x1b[K\r\n");
    };
    push(header);
    push("");
    for l in body.lines() {
        push(l);
    }
    if let Some(e) = error {
        push("");
        push(&format!("last update failed: {e}"));
    }
    out.push_str("\x1b[J");
    out
}

fn clock() -> String {
    Proc::new("date")
        .arg("+%T")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// Raw-ish terminal for the lifetime of the guard.
struct Terminal {
    saved: String,
}

impl Terminal {
    fn enter() -> Result<Terminal> {
        // stty works on its stdin, which `output()` would otherwise set to /dev/null
        let out = Proc::new("stty").arg("-g").stdin(Stdio::inherit()).output()?;
        let saved = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !out.status.success() || saved.is_empty() {
            return usage("status --watch: can't set up the terminal (stty failed)");
        }
        // keys arrive one at a time without echo; reads give up after 0.1s;
        // Ctrl-C arrives as a byte so the terminal is always restored
        Proc::new("stty").args(["-icanon", "-echo", "-isig", "min", "0", "time", "1"]).status()?;
        print!("\x1b[?1049h\x1b[?25l\x1b[2J");
        let _ = std::io::stdout().flush();
        Ok(Terminal { saved })
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        print!("\x1b[?25h\x1b[?1049l");
        let _ = std::io::stdout().flush();
        let _ = Proc::new("stty").arg(&self.saved).status();
    }
}

enum Cmd {
    Refresh,
}

/// Redraw `fetch()` every `every` seconds until q. `title` heads the screen.
pub fn run<F>(title: &str, every: f64, fetch: F) -> Result<()>
where
    F: Fn() -> Result<String> + Send + 'static,
{
    if cfg!(not(unix)) {
        return usage(format!("{title} --watch needs a Unix terminal (it uses stty); on Windows, 'tvbee ui' shows the same live"));
    }
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return usage(format!("{title} --watch needs a terminal"));
    }
    let interval = Duration::from_secs_f64(every);
    let (results_tx, results) = mpsc::channel();
    let (cmd_tx, cmds) = mpsc::channel::<Cmd>();
    // not joined: on q the process just exits, even mid-fetch
    std::thread::spawn(move || {
        loop {
            if results_tx.send(fetch()).is_err() {
                return;
            }
            match cmds.recv_timeout(interval) {
                Ok(Cmd::Refresh) | Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    });

    let _term = Terminal::enter()?;
    let hint = format!("every {every}s - q quit, r refresh");
    let (mut body, mut error) = (String::from("waiting for the TV..."), None::<String>);
    let mut stamp = String::new();
    let draw = |body: &str, error: Option<&str>, stamp: &str| {
        let header = format!("tvbee {title}   ({hint})   {stamp}");
        print!("{}", frame(&header, body, error));
        let _ = std::io::stdout().flush();
    };
    draw(&body, None, &stamp);
    let mut stdin = std::io::stdin();
    let mut buf = [0u8; 16];
    loop {
        // returns after a key or 0.1s
        let n = stdin.read(&mut buf).unwrap_or(0);
        for &b in &buf[..n] {
            match key(b) {
                Key::Quit => return Ok(()),
                Key::Refresh => {
                    let _ = cmd_tx.send(Cmd::Refresh);
                }
                Key::Other => {}
            }
        }
        let mut changed = false;
        while let Ok(r) = results.try_recv() {
            match r {
                Ok(text) => (body, error) = (text, None),
                Err(e) => error = Some(e.to_string()),
            }
            stamp = clock();
            changed = true;
        }
        if changed {
            draw(&body, error.as_deref(), &stamp);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys() {
        assert_eq!(key(b'q'), Key::Quit);
        assert_eq!(key(3), Key::Quit);
        assert_eq!(key(b'r'), Key::Refresh);
        assert_eq!(key(b'x'), Key::Other);
    }

    #[test]
    fn frames_clear_leftovers() {
        let f = frame("head", "a\nb", None);
        assert!(f.starts_with("\x1b[H"));
        assert!(f.contains("a\x1b[K\r\nb\x1b[K\r\n"));
        assert!(f.ends_with("\x1b[J"));
        assert!(frame("h", "a", Some("offline")).contains("last update failed: offline"));
    }
}

//! Links (what's on's URL): shortened to fit with … in the middle, shown
//! whole on request, copied to the clipboard, and opened, with the
//! terminal's own hyperlinks (OSC 8) where it has them.
//!
//! The URL comes from the TV: anything that could end or start an escape
//! sequence is taken out before it's written to the terminal.

use std::io::Write;

/// `s` without control characters (ESC, BEL, newlines...): safe inside an
/// escape sequence.
pub fn clean(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}

/// `s` in at most `max` characters: its start and end with … between
/// (the host and the end of a path say most).
pub fn shorten_middle(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    if max <= 1 {
        return "…".chars().take(max).collect();
    }
    // a little more of the start (the host) than the end
    let keep = max - 1;
    let head = keep.div_ceil(2) + keep / 6;
    let head = head.min(keep);
    let tail = keep - head;
    let start: String = s.chars().take(head).collect();
    let end: String = s.chars().skip(n - tail).collect();
    format!("{start}…{end}")
}

/// `s` cut into rows of `width` characters (a URL has no spaces to wrap at).
pub fn rows(s: &str, width: usize) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    chars.chunks(width.max(1)).map(|c| c.iter().collect()).collect()
}

/// `text` as a terminal hyperlink to `url` (OSC 8): terminals that know them
/// open it on (ctrl+)click; others show `text` as it is.
pub fn osc8(url: &str, text: &str) -> String {
    format!("\x1b]8;;{}\x1b\\{}\x1b]8;;\x1b\\", clean(url), clean(text))
}

/// Put `text` on the clipboard through the terminal (OSC 52): works over
/// ssh, in terminals that allow it.
pub fn osc52(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", tveee_core::crypto::base64(clean(text).as_bytes()))
}

/// Whether this computer has a desktop to copy to or open a browser on.
fn desktop() -> bool {
    cfg!(any(target_os = "macos", windows)) || std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some()
}

/// Run `cmd` with `input` on its stdin; whether it worked.
fn pipe(cmd: &[&str], input: &str) -> bool {
    let Ok(mut child) = std::process::Command::new(cmd[0])
        .args(&cmd[1..])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    else {
        return false;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.as_bytes());
    }
    child.wait().is_ok_and(|s| s.success())
}

/// Copy `url`: through the terminal (OSC 52), and with the desktop's own
/// clipboard tool when there is one here (some terminals ignore OSC 52).
/// Returns what to say.
pub fn copy(url: &str) -> String {
    let url = clean(url);
    let mut out = std::io::stdout();
    let _ = out.write_all(osc52(&url).as_bytes());
    let _ = out.flush();
    let tools: &[&[&str]] = &[&["wl-copy"], &["xclip", "-selection", "clipboard"], &["xsel", "-b", "-i"], &["pbcopy"], &["clip.exe"]];
    if desktop() && tools.iter().any(|t| pipe(t, &url)) {
        "copied the link".into()
    } else {
        "copied the link (through the terminal: if it allows that)".into()
    }
}

/// Copy `text` (lines: their breaks kept, anything else that could start
/// an escape taken out) as [`copy`] does; `what` names it in what's said.
pub fn copy_text(text: &str, what: &str) -> String {
    let text = text.lines().map(clean).collect::<Vec<_>>().join("\n");
    let mut out = std::io::stdout();
    let _ = out.write_all(osc52_raw(&text).as_bytes());
    let _ = out.flush();
    let tools: &[&[&str]] = &[&["wl-copy"], &["xclip", "-selection", "clipboard"], &["xsel", "-b", "-i"], &["pbcopy"], &["clip.exe"]];
    if desktop() && tools.iter().any(|t| pipe(t, &text)) {
        format!("copied {what}")
    } else {
        format!("copied {what} (through the terminal: if it allows that)")
    }
}

/// [`osc52`] for text already cleaned (its line breaks kept).
fn osc52_raw(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", tveee_core::crypto::base64(text.as_bytes()))
}

/// Open `url` in this computer's browser. Only web links: anything else
/// (smb://, file://) is better copied.
pub fn open(url: &str) -> String {
    let url = clean(url);
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return "not a web link: y copies it".into();
    }
    if !desktop() {
        return "no browser here: ctrl+click the link (or y copies it)".into();
    }
    let cmd: &[&str] = if cfg!(target_os = "macos") {
        &["open"]
    } else if cfg!(windows) {
        &["explorer"]
    } else {
        &["xdg-open"]
    };
    let ok = std::process::Command::new(cmd[0])
        .args(&cmd[1..])
        .arg(&url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok();
    if ok { "opening the link in the browser".into() } else { format!("couldn't run {}: y copies the link", cmd[0]) }
}

/// A link drawn this frame: where, the text shown, and where it goes. The
/// terminal is told about it after drawing ([`emit`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Drawn {
    pub x: u16,
    pub y: u16,
    pub text: String,
    pub url: String,
}

/// The links whose text is still on screen where they were drawn: a pop-up
/// drawn over one afterwards hides it, and writing it again (as a
/// hyperlink) would put it on top of the pop-up.
pub fn showing(links: &[Drawn], screen: &ratatui::buffer::Buffer) -> Vec<Drawn> {
    links
        .iter()
        .filter(|l| {
            let area = screen.area;
            l.text.chars().enumerate().all(|(i, c)| {
                let x = l.x + u16::try_from(i).unwrap_or(u16::MAX);
                x < area.right() && l.y < area.bottom() && screen[(x, l.y)].symbol().starts_with(c)
            })
        })
        .cloned()
        .collect()
}

/// Draw `links` again over themselves as terminal hyperlinks, in the style
/// they were drawn in (cyan, underlined).
pub fn emit(links: &[Drawn]) -> std::io::Result<()> {
    use ratatui::crossterm::{cursor::MoveTo, queue, style};
    if links.is_empty() {
        return Ok(());
    }
    let mut out = std::io::stdout();
    for l in links {
        queue!(
            out,
            MoveTo(l.x, l.y),
            style::SetForegroundColor(style::Color::Cyan),
            style::SetAttribute(style::Attribute::Underlined),
            style::Print(osc8(&l.url, &l.text)),
            style::SetAttribute(style::Attribute::Reset),
        )?;
    }
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LONG: &str = "https://catchup.example.co.uk/stream/channel-four/2026-11-27/episode-12345678/manifest.m3u8?token=abcdef0123456789";

    #[test]
    fn shortened_in_the_middle() {
        let s = shorten_middle(LONG, 40);
        assert_eq!(s.chars().count(), 40);
        assert!(s.starts_with("https://catchup.example"));
        assert!(s.ends_with("0123456789"));
        assert!(s.contains('…'));
        // what fits is left alone
        assert_eq!(shorten_middle("https://bbc.co.uk/", 40), "https://bbc.co.uk/");
        assert_eq!(shorten_middle(LONG, 1), "…");
        assert_eq!(shorten_middle(LONG, 0), "");
    }

    #[test]
    fn rows_of_a_width() {
        let r = rows(LONG, 50);
        assert_eq!(r.concat(), LONG);
        assert!(r.iter().all(|row| row.chars().count() <= 50));
        assert_eq!(r.len(), LONG.len().div_ceil(50));
    }

    #[test]
    fn escapes_cant_be_smuggled_in() {
        // a link from the TV that tries to end the sequence and start its own
        let evil = "https://x.example/\x1b]8;;https://bad.example\x1b\\\x07";
        let s = osc8(evil, "x");
        assert_eq!(s.matches('\x1b').count(), 4);
        assert!(!osc52(evil).contains("\x1b]8"));
        assert_eq!(osc52("hi"), "\x1b]52;c;aGk=\x07");
    }

    #[test]
    fn only_web_links_open_here() {
        assert_eq!(open("smb://nas/films/a.mkv"), "not a web link: y copies it");
    }
}

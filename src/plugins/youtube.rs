//! YouTube for Android TV: its app entry (links for what's playing, search),
//! and commands that look inside the app.
//!
//! The YouTube TV app is a web app run by Google's Cobalt engine, which opens
//! a devtools socket. tveee reads the page through it ([`crate::devtools`]):
//! the route (`tv#/watch?v=...`), the focused element, the video. Enabled when
//! the app is installed.

use crate::devtools;
use crate::more::{Chapter, Details, Go, Link, Place};
use crate::error::{failed, Result};
use crate::registry::Command;
use crate::tv::Tv;

pub const PACKAGE: &str = "com.google.android.youtube.tv";

pub fn detect(tv: &Tv) -> Result<bool> {
    tv.connect()?;
    let out = tv.shell(&["cmd", "package", "list", "packages", PACKAGE])?;
    Ok(out.lines().any(|l| l.trim() == format!("package:{PACKAGE}")))
}

/// `https://www.youtube.com/tv#/watch?v=ID&...` -> `https://www.youtube.com/watch?v=ID`
pub fn watch_link(page: &str) -> Option<String> {
    let rest = &page[page.find("watch?v=")? + "watch?v=".len()..];
    let id: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
    (id.len() == 11).then(|| format!("https://www.youtube.com/watch?v={id}"))
}

/// Places in the app; each is a youtube.com link the app opens at the
/// matching screen (checked on a TV).
pub const PLACES: &[Place] = &[
    Place { id: "home", label: "Home", link: "https://www.youtube.com/" },
    Place { id: "subscriptions", label: "Subscriptions", link: "https://www.youtube.com/feed/subscriptions" },
    Place { id: "history", label: "History", link: "https://www.youtube.com/feed/history" },
    Place { id: "library", label: "Library (watch later, playlists)", link: "https://www.youtube.com/feed/library" },
    Place { id: "watchlater", label: "Watch later", link: "https://www.youtube.com/playlist?list=WL" },
];

pub fn apps() -> Vec<crate::plugins::apps::App> {
    use crate::plugins::apps::{app, Kind};
    vec![app("youtube", "YouTube", PACKAGE, Kind::Video)]
}

/// The YouTube provider: what's playing (via the app's devtools page), its
/// places, details via yt-dlp, and search by results link.
pub struct YouTube;
pub static YOUTUBE: YouTube = YouTube;

impl crate::provider::Provider for YouTube {
    fn id(&self) -> &'static str {
        "youtube"
    }
    fn name(&self) -> &'static str {
        "YouTube"
    }
    fn hosts(&self) -> &'static [&'static str] {
        &[PACKAGE]
    }
    fn uses_devtools(&self) -> bool {
        true
    }
    fn watch_link(&self, page: &str) -> Option<String> {
        watch_link(page)
    }
    // the TV app takes links, not the search intent
    fn search_link(&self) -> Option<&'static str> {
        Some("https://www.youtube.com/results?search_query={q}")
    }
    // each link into the running app stacks another drawing surface; a
    // stale one can cover the video (you hear it but don't see it)
    fn fresh_for_links(&self) -> bool {
        true
    }
    // already in front: hand the page the link as Android would, through
    // devtools, rather than closing the app for it (the screen would drop
    // to home, then reopen). The app ignores changes to location.hash.
    fn open_here(&self, tv: &Tv, link: &str) -> Result<bool> {
        if crate::plugins::info::focus_package(&tv.window_dump()?).as_deref() != Some(PACKAGE) {
            return Ok(false);
        }
        let Some(c) = devtools::Conn::open(tv, PACKAGE) else { return Ok(false) };
        Ok(c.eval(&deep_link_expr(link)).is_ok_and(|v| v == "sent"))
    }
    fn places(&self) -> &'static [Place] {
        PLACES
    }
    // the video and how far in: restarting comes back to it
    fn save_session(&self, tv: &Tv) -> Option<crate::provider::Session> {
        session(tv)
    }
    fn has_sessions(&self) -> bool {
        true
    }
    fn details(&self) -> Option<&dyn crate::provider::Details> {
        Some(self)
    }
}

impl crate::provider::Details for YouTube {
    fn details(&self, link: &str) -> Result<Details> {
        details(link)
    }
}

/// JavaScript that hands the page `link` the way the app's Android side does:
/// a `deeplink` event on Cobalt's `h5vcc.runtime`. "sent" if it could.
fn deep_link_expr(link: &str) -> String {
    // a JavaScript string: escape what could end it
    let mut quoted = String::from("'");
    for ch in link.chars() {
        match ch {
            '\\' | '\'' => quoted.extend(['\\', ch]),
            c if (c as u32) < 0x20 || c == '\u{2028}' || c == '\u{2029}' => quoted.push_str(&format!("\\u{:04x}", c as u32)),
            c => quoted.push(c),
        }
    }
    quoted.push('\'');
    format!(
        "(function(){{var r=window.h5vcc&&h5vcc.runtime;if(!r||!r.dispatchEvent)return 'no';\
         var e=new Event('deeplink');Object.defineProperty(e,'url',{{value:{quoted}}});\
         r.dispatchEvent(e);return 'sent'}})()"
    )
}

pub fn providers() -> Vec<&'static dyn crate::provider::Provider> {
    vec![&YOUTUBE]
}

/// The video id in a watch link.
pub fn video_id(link: &str) -> Option<String> {
    let rest = &link[link.find("v=")? + 2..];
    let id: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
    (id.len() == 11).then_some(id)
}

/// yt-dlp output templates: one JSON value per line, in this order.
const FIELDS: &[&str] = &[
    "title", "channel", "uploader_id", "channel_url", "upload_date", "view_count", "like_count", "duration", "chapters",
    "description",
];

/// Turn yt-dlp's lines (see [`FIELDS`]) into details.
pub fn parse_details(out: &str) -> Details {
    let lines: Vec<&str> = out.lines().collect();
    let get = |k: &str| FIELDS.iter().position(|f| *f == k).and_then(|i| lines.get(i)).copied().unwrap_or("NA");
    let text = |k: &str| crate::http::json_string(get(k)).filter(|s| !s.is_empty());
    let num = |k: &str| get(k).trim().parse::<f64>().ok();
    let mut d = Details::default();
    let handle = text("uploader_id");
    if let Some(ch) = text("channel") {
        d.fields.push(("channel", handle.as_ref().map_or(ch.clone(), |h| format!("{ch} ({h})"))));
    }
    if let Some(date) = text("upload_date").filter(|s| s.len() == 8) {
        d.fields.push(("uploaded", format!("{}-{}-{}", &date[..4], &date[4..6], &date[6..])));
    }
    if let Some(v) = num("view_count") {
        d.fields.push(("views", crate::more::count(v)));
    }
    if let Some(v) = num("like_count") {
        d.fields.push(("likes", crate::more::count(v)));
    }
    if let Some(v) = num("duration") {
        d.fields.push(("length", crate::nowplaying::clock(v)));
    }
    d.chapters = crate::http::objects(get("chapters"))
        .into_iter()
        .filter_map(|o| Some(Chapter { start: crate::http::number_field(o, "start_time")?, title: crate::http::string_field(o, "title")? }))
        .collect();
    d.description = text("description");
    // where to go from here: the channel, the chapters, links in the description
    let channel = handle.map(|h| format!("https://www.youtube.com/{h}")).or_else(|| text("channel_url"));
    if let Some(url) = channel {
        d.links.push(Link { label: format!("channel: {}", text("channel").unwrap_or_default()), go: Go::Link(url) });
    }
    for (i, c) in d.chapters.iter().enumerate() {
        d.links.push(Link { label: format!("chapter {}: {} ({})", i + 1, c.title, crate::nowplaying::clock(c.start)), go: Go::Seek(c.start) });
    }
    d.links.extend(crate::more::description_links(d.description.as_deref().unwrap_or("")));
    d
}

fn cache_file(id: &str) -> Option<std::path::PathBuf> {
    let base = crate::plugin::cache_path("x")?;
    Some(base.parent()?.join("youtube").join(format!("{id}.txt")))
}

/// Details for a watch link, from yt-dlp (cached a day per video).
pub fn details(link: &str) -> Result<Details> {
    let Some(id) = video_id(link) else { return failed("youtube: that isn't a video link") };
    let cache = cache_file(&id);
    let fresh = |p: &std::path::Path| -> Option<String> {
        let age = std::fs::metadata(p).ok()?.modified().ok()?.elapsed().ok()?;
        (age < std::time::Duration::from_secs(24 * 3600)).then(|| std::fs::read_to_string(p).ok()).flatten()
    };
    if let Some(text) = cache.as_deref().and_then(fresh) {
        return Ok(parse_details(&text));
    }
    let Some(ytdlp) = crate::tv::search_path("yt-dlp", std::env::var_os("PATH").as_deref()) else {
        return failed("youtube: details need yt-dlp (https://github.com/yt-dlp/yt-dlp), not found on PATH");
    };
    let mut cmd = std::process::Command::new(ytdlp);
    cmd.args(["--skip-download", "--no-warnings"]);
    for f in FIELDS {
        cmd.args(["--print", &format!("%({f})j")]);
    }
    let out = cmd.arg(format!("https://www.youtube.com/watch?v={id}")).output()?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return failed(format!("youtube: yt-dlp failed: {}", err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim()));
    }
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    if let Some(p) = cache {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(p, &text);
    }
    Ok(parse_details(&text))
}

/// Where YouTube is: the video and how far in (or home), from its page.
fn session(tv: &Tv) -> Option<crate::provider::Session> {
    let c = devtools::Conn::open(tv, PACKAGE)?;
    let raw = c.eval("JSON.stringify({h:location.hash,t:(document.querySelector('video')||{}).currentTime||0})").ok()?;
    let hash = crate::http::string_field(&raw, "h")?;
    Some(match video_id(&hash) {
        Some(id) => {
            let t = crate::http::number_field(&raw, "t").unwrap_or(0.0).floor() as u64;
            crate::provider::Session {
                link: format!("https://www.youtube.com/watch?v={id}&t={t}s"),
                said: format!("at {} in the same video", crate::nowplaying::clock(t as f64)),
            }
        }
        None => crate::provider::Session { link: "https://www.youtube.com/".into(), said: String::new() },
    })
}

/// No commands of its own: `tveee app youtube` covers it (see `apps`).
pub fn commands() -> Vec<Command> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deep_links_are_quoted() {
        let e = deep_link_expr("https://www.youtube.com/results?search_query=red%20panda");
        assert!(e.contains("{value:'https://www.youtube.com/results?search_query=red%20panda'}"), "{e}");
        // nothing in a link can end the JavaScript string
        let e = deep_link_expr("a'b\\c\nd");
        assert!(e.contains(r"{value:'a\'b\\c\u000ad'}"), "{e}");
    }

    #[test]
    fn reads_ytdlp_output() {
        let out = r#""Everyone\u2019s Letting AI Hack Their TVs\u2026 So I Tried It"
"RelfWolf"
"@RelfWolf"
"https://www.youtube.com/channel/UCWU67Tq18eRho_DN6z2FrZQ"
"20261115"
1051296
13384
574
[{"start_time": 0, "title": "Ads on a TV I paid for", "end_time": 54}, {"start_time": 54.0, "title": "Could anybody do this?", "end_time": 69}]
"Intro\n\u2022 Claude Code: https://code.claude.com/docs/en/setup\nbye"
"#;
        let d = parse_details(out);
        assert_eq!(
            d.fields,
            vec![
                ("channel", "RelfWolf (@RelfWolf)".to_string()),
                ("uploaded", "2026-11-15".into()),
                ("views", "1.05M".into()),
                ("likes", "13.4K".into()),
                ("length", "9:34".into()),
            ]
        );
        assert_eq!(d.chapters.len(), 2);
        assert_eq!(d.chapters[1], Chapter { start: 54.0, title: "Could anybody do this?".into() });
        assert_eq!(d.links[0], Link { label: "channel: RelfWolf".into(), go: Go::Link("https://www.youtube.com/@RelfWolf".into()) });
        assert_eq!(d.links[2].go, Go::Seek(54.0));
        assert_eq!(d.links[3].label, "Claude Code");
        // no chapters / missing values (NA)
        let d = parse_details("\"t\"\nNA\nNA\nNA\nNA\nNA\nNA\nNA\nNA\nNA\n");
        assert!(d.fields.is_empty() && d.chapters.is_empty() && d.links.is_empty());
        assert_eq!(video_id("https://www.youtube.com/watch?v=aN-65z6bvKk"), Some("aN-65z6bvKk".into()));
    }

    #[test]
    fn watch_links() {
        assert_eq!(
            watch_link("https://www.youtube.com/tv#/watch?v=nh5iCP5Ta_4&resume").as_deref(),
            Some("https://www.youtube.com/watch?v=nh5iCP5Ta_4")
        );
        assert_eq!(watch_link("https://www.youtube.com/tv#/"), None);
        assert_eq!(watch_link("https://www.youtube.com/tv#/watch?v=short"), None);
    }
}

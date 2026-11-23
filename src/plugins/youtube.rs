//! YouTube for Android TV: its app entry (links for what's playing, search),
//! and commands that look inside the app.
//!
//! The YouTube TV app is a web app run by Google's Cobalt engine, which opens
//! a devtools socket. tvbee reads the page through it ([`crate::devtools`]):
//! the route (`tv#/watch?v=...`), the focused element, the video. Enabled when
//! the app is installed.

use crate::devtools;
use crate::more::{Chapter, Details, Go, Link, Place};
use crate::error::{failed, usage, Result};
use crate::registry::Command;
use crate::tv::Tv;
use crate::Ctx;

const CAT: &str = "youtube";
pub const PACKAGE: &str = "com.google.android.youtube.tv";

pub fn detect(tv: &Tv) -> Result<bool> {
    tv.connect()?;
    let out = tv.shell(&["pm", "list", "packages", PACKAGE])?;
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
    // already in front: go to its search page through devtools rather than
    // closing it for a link (the screen would drop to home, then reopen)
    fn search_here(&self, tv: &Tv, query: &str) -> Result<bool> {
        if crate::plugins::info::focus_package(&tv.window_dump()?).as_deref() != Some(PACKAGE) {
            return Ok(false);
        }
        let Some(c) = devtools::Conn::open(tv, PACKAGE) else { return Ok(false) };
        let route = search_route(query);
        Ok(c.eval(&format!("(location.hash='{route}',location.hash)")).is_ok_and(|now| now == route))
    }
    fn places(&self) -> &'static [Place] {
        PLACES
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

/// The app's own route for a search (what its search page shows in
/// `location.hash`), safe to put in a JavaScript string.
fn search_route(query: &str) -> String {
    format!("#/search?q={}", crate::plugins::search::url_encode(query))
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

/// JSON with the page's route, title, focused element and the visible,
/// focusable things on it (tag and text), for working out what a screen is.
const PAGE_EXPR: &str = "(()=>{const f=document.activeElement;\
const txt=e=>(e.innerText||e.getAttribute('aria-label')||'').trim().replace(/\\s+/g,' ').slice(0,90);\
const seen=new Set();const items=[];\
for(const e of document.querySelectorAll('[tabindex],[role=button],[role=option],[aria-label]')){\
if(!e.offsetWidth)continue;const t=txt(e);if(!t||seen.has(t))continue;seen.add(t);\
items.push({tag:e.tagName.toLowerCase(),text:t,focused:e===f||e.contains(f)});if(items.length>=40)break}\
return JSON.stringify({route:location.hash,title:document.title,\
focused:f?{tag:f.tagName.toLowerCase(),text:txt(f)}:null,items})})()";

fn conn(tv: &Tv) -> Result<devtools::Conn> {
    tv.connect()?;
    devtools::Conn::open(tv, PACKAGE).map_or_else(
        || failed("youtube: can't reach the YouTube app's devtools socket (is it running?)"),
        Ok,
    )
}

/// `youtube page [--json]`: what the app is showing.
fn page(c: &Ctx, args: &[String]) -> Result<()> {
    let json = match args.first().map(String::as_str) {
        None => false,
        Some("--json") => true,
        Some(x) => return usage(format!("youtube page: unknown option '{x}'")),
    };
    let raw = conn(&c.tv)?.eval(PAGE_EXPR)?;
    if json {
        println!("{raw}");
        return Ok(());
    }
    let field = |k: &str| crate::http::string_field(&raw, k).unwrap_or_default();
    println!("route:   {}", field("route"));
    println!("title:   {}", field("title"));
    // the items are objects inside the "items" array
    let items = raw.find("\"items\":[").map_or("", |i| &raw[i + 9..]);
    let objs = crate::http::objects(items);
    if objs.is_empty() {
        println!("  (nothing focusable is visible: is YouTube in front?)");
    }
    for o in &objs {
        let tag = crate::http::string_field(o, "tag").unwrap_or_default();
        let text = crate::http::string_field(o, "text").unwrap_or_default();
        let focused = o.contains("\"focused\":true");
        println!("  {} {tag:<32} {text}", if focused { ">" } else { " " });
    }
    Ok(())
}

/// Restart the app where it was: the same video at the same point, or home.
/// Fixes a frozen picture (sound but no video) by giving it fresh surfaces.
pub fn restart(tv: &Tv) -> Result<String> {
    tv.connect()?;
    let at = devtools::Conn::open(tv, PACKAGE).and_then(|c| {
        let raw = c.eval("JSON.stringify({h:location.hash,t:(document.querySelector('video')||{}).currentTime||0})").ok()?;
        let hash = crate::http::string_field(&raw, "h")?;
        Some((video_id(&hash), crate::http::number_field(&raw, "t").unwrap_or(0.0)))
    });
    tv.shell(&["am", "force-stop", PACKAGE])?;
    let (link, said) = match at {
        Some((Some(id), t)) => {
            let t = t.floor() as u64;
            (format!("https://www.youtube.com/watch?v={id}&t={t}s"), format!("restarted YouTube at {} in the same video", crate::nowplaying::clock(t as f64)))
        }
        _ => ("https://www.youtube.com/".to_string(), "restarted YouTube".to_string()),
    };
    let cmd = format!("am start -a android.intent.action.VIEW -d {} -p {PACKAGE}", crate::plugins::open::sh_quote(&link));
    tv.shell(&[&cmd])?;
    Ok(said)
}

fn youtube(c: &Ctx, args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("page") => page(c, &args[1..]),
        Some("restart") => {
            println!("{}", restart(&c.tv)?);
            Ok(())
        }
        None => usage("usage: tvbee youtube page [--json] | restart"),
        Some(x) => usage(format!("youtube: unknown action '{x}' (try: page, restart)")),
    }
}

pub fn menu() -> Vec<crate::menu::MenuItem> {
    vec![crate::menu::MenuItem::new("youtube", 'Y', "restart YouTube here (fixes a frozen picture)", restart)]
}

pub fn commands() -> Vec<Command> {
    vec![Command::new(CAT, "youtube", "page [--json] | restart", "look inside the YouTube app; fix a stuck picture", youtube).long(
        "Reads the YouTube app's page through its devtools socket (the app must be\n\
         running).\n\
         \n\
         \x20 page          the current route (e.g. #/watch?v=..., #/ for home), the page\n\
         \x20               title, and the visible focusable items; > marks the focused one\n\
         \x20 page --json   the same, raw\n\
         \n\
         Useful for working out what a screen is, e.g. the 'Who's watching?' picker.\n\
         \n\
         \x20 restart       close YouTube and reopen it at the same point of the same\n\
         \x20               video (or at home). Fixes a frozen picture: you hear the\n\
         \x20               video but see an old screen. tvbee also closes YouTube before\n\
         \x20               sending it links (open, search, goto), which avoids that;\n\
         \x20               a search while YouTube is in front goes to its search page\n\
         \x20               without closing it.",
    )]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_routes_are_encoded() {
        assert_eq!(search_route("hot ones"), "#/search?q=hot%20ones");
        // nothing that could end the JavaScript string
        assert_eq!(search_route("it's \\ \"x\""), "#/search?q=it%27s%20%5C%20%22x%22");
    }

    #[test]
    fn reads_page_items() {
        let raw = r##"{"route":"#/","title":"YouTube on TV","focused":{"tag":"ytlr-tile-renderer","text":"A"},"items":[{"tag":"ytlr-tile-renderer","text":"A","focused":true},{"tag":"ytlr-button","text":"B","focused":false}]}"##;
        let items = raw.find("\"items\":[").map_or("", |i| &raw[i + 9..]);
        let objs = crate::http::objects(items);
        assert_eq!(objs.len(), 2);
        assert_eq!(crate::http::string_field(objs[1], "text").as_deref(), Some("B"));
        assert!(objs[0].contains("\"focused\":true"));
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

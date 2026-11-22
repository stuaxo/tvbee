//! YouTube for Android TV: its app entry (links for what's playing, search),
//! and commands that look inside the app.
//!
//! The YouTube TV app is a web app run by Google's Cobalt engine, which opens
//! a devtools socket. tvbee reads the page through it ([`crate::devtools`]):
//! the route (`tv#/watch?v=...`), the focused element, the video. Enabled when
//! the app is installed.

use crate::devtools;
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

pub fn apps() -> Vec<crate::plugins::apps::App> {
    use crate::plugins::apps::{app, App, Kind};
    vec![App {
        page_url: Some(watch_link),
        // the TV app takes links, not the search intent
        search_link: Some("https://www.youtube.com/results?search_query={q}"),
        ..app("youtube", "YouTube", PACKAGE, Kind::Video)
    }]
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

fn youtube(c: &Ctx, args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("page") => page(c, &args[1..]),
        None => usage("usage: tvbee youtube page [--json]"),
        Some(x) => usage(format!("youtube: unknown action '{x}' (try: page)")),
    }
}

pub fn commands() -> Vec<Command> {
    vec![Command::new(CAT, "youtube", "page [--json]", "look inside the YouTube app", youtube).long(
        "Reads the YouTube app's page through its devtools socket (the app must be\n\
         running).\n\
         \n\
         \x20 page          the current route (e.g. #/watch?v=..., #/ for home), the page\n\
         \x20               title, and the visible focusable items; > marks the focused one\n\
         \x20 page --json   the same, raw\n\
         \n\
         Useful for working out what a screen is, e.g. the 'Who's watching?' picker.",
    )]
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn watch_links() {
        assert_eq!(
            watch_link("https://www.youtube.com/tv#/watch?v=nh5iCP5Ta_4&resume").as_deref(),
            Some("https://www.youtube.com/watch?v=nh5iCP5Ta_4")
        );
        assert_eq!(watch_link("https://www.youtube.com/tv#/"), None);
        assert_eq!(watch_link("https://www.youtube.com/tv#/watch?v=short"), None);
    }
}

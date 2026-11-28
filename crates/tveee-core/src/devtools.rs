//! Chromium/Cobalt remote-debugging ("devtools") servers on the TV.
//!
//! Browser-based apps (the YouTube app is Cobalt) listen on an abstract unix
//! socket named `*devtools_remote*`. Forwarded with adb it serves `/json`
//! (the open pages) and a WebSocket per page that speaks the DevTools
//! protocol, which lets us read the page's `<video>` element. Everything here
//! is read-only.

use crate::error::{Error, Result, failed};
use crate::http;
use crate::tv::Tv;
use crate::ws::{Recv, WsClient};
use std::time::Duration;

/// Abstract socket names of devtools servers, from `/proc/net/unix`.
pub fn sockets(proc_net_unix: &str) -> Vec<String> {
    let mut names: Vec<String> = proc_net_unix
        .lines()
        .filter_map(|l| l.split_whitespace().last()?.strip_prefix('@'))
        .filter(|n| n.contains("devtools_remote"))
        .map(str::to_string)
        .collect();
    names.sort();
    names.dedup();
    names
}

/// The devtools socket of a WebView in process `pid`
/// (`webview_devtools_remote_<pid>`), if it has one: WebViews name theirs
/// after their process.
pub fn webview_socket(proc_net_unix: &str, pid: &str) -> Option<String> {
    let name = format!("webview_devtools_remote_{}", pid.trim());
    sockets(proc_net_unix).into_iter().find(|s| *s == name)
}

/// Keep connections open between calls (the ui, which asks every second);
/// otherwise each call opens one and closes it (one-off commands).
static KEEP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static KEPT: std::sync::Mutex<Option<Conn>> = std::sync::Mutex::new(None);

/// Keep connections open (the ui), or close the kept one and stop keeping.
pub fn keep_open(on: bool) {
    KEEP.store(on, std::sync::atomic::Ordering::Relaxed);
    if !on {
        KEPT.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
    }
}

/// Run `f` with a connection to `pkg`'s devtools server: the kept one if
/// there is (reopened once if it fails: the app restarted, say). No server:
/// `Error::Unavailable`.
pub fn with_conn<T>(tv: &Tv, pkg: &str, f: impl Fn(&Conn) -> Result<T>) -> Result<T> {
    let none = || Error::Unavailable(format!("can't reach {pkg}'s page (is it running?)"));
    if !KEEP.load(std::sync::atomic::Ordering::Relaxed) {
        return f(&Conn::open(tv, pkg).ok_or_else(none)?);
    }
    let mut kept = KEPT.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if kept.as_ref().is_some_and(|c| c.pkg != pkg) {
        *kept = None;
    }
    for fresh in [false, true] {
        if fresh || kept.is_none() {
            *kept = Conn::open(tv, pkg);
        }
        let Some(c) = kept.as_ref() else { return Err(none()) };
        match f(c) {
            Ok(t) => return Ok(t),
            Err(e) if fresh => return Err(e),
            Err(_) => *kept = None,
        }
    }
    Err(none())
}

/// Where the page's `<video>` is.
#[derive(Debug, PartialEq, Clone, Copy)]
pub struct Progress {
    pub position: f64,
    /// None for live streams (infinite or unknown duration).
    pub duration: Option<f64>,
    pub paused: bool,
    /// How fast it goes: 1 playing, 2, 4... fast-forwarding, below 0
    /// rewinding (as the app says; 1 when it doesn't).
    pub rate: f64,
}

/// An open page and, if it has a video, how far through it is.
#[derive(Debug, PartialEq)]
pub struct Page {
    pub url: String,
    pub progress: Option<Progress>,
}

/// JSON.stringify'd `[currentTime, duration, paused]` of the longest video on the page.
const VIDEO_EXPR: &str = "(()=>{const vs=[...document.querySelectorAll('video')].filter(v=>v.readyState>0);\
if(!vs.length)return '';vs.sort((a,b)=>(b.duration||0)-(a.duration||0));const v=vs[0];\
return JSON.stringify([v.currentTime,v.duration,v.paused])})()";

/// `[1516.34,1651.06,false]` (duration may be `null` for live) -> Progress.
pub fn parse_progress(s: &str) -> Option<Progress> {
    let inner = s.trim().strip_prefix('[')?.strip_suffix(']')?;
    let f: Vec<&str> = inner.split(',').map(str::trim).collect();
    let [pos, dur, paused] = f.as_slice() else { return None };
    let position: f64 = pos.parse().ok().filter(|p: &f64| p.is_finite() && *p >= 0.0)?;
    let duration = dur.parse().ok().filter(|d: &f64| d.is_finite() && *d > 0.0);
    Some(Progress { position, duration, paused: *paused == "true", rate: 1.0 })
}

/// Which app a devtools server says it is (`/json/version`'s
/// `Android-Package`; None: it doesn't say), at a forwarded `port`.
pub struct Version {
    pub port: u16,
}

impl crate::ask::Ask for Version {
    type Answer = Option<String>;
    fn name(&self) -> String {
        "devtools /json/version".into()
    }
    fn request(&self, _: &Tv) -> Result<crate::ask::Request> {
        Ok(crate::ask::Request::Get { host: "127.0.0.1".into(), port: self.port, path: "/json/version".into() })
    }
    fn parse(&self, reply: &crate::ask::Reply) -> Result<Option<Option<String>>> {
        if reply.status != 200 {
            return failed(format!("devtools: HTTP {} for /json/version", reply.status));
        }
        Ok(Some(http::json(&reply.body).and_then(|v| http::text(&v, "Android-Package"))))
    }
}

/// The open pages of a devtools server at a forwarded `port`: (url,
/// websocket path) of each (`/json`).
pub struct Pages {
    pub port: u16,
}

impl crate::ask::Ask for Pages {
    type Answer = Vec<(String, Option<String>)>;
    fn name(&self) -> String {
        "devtools /json".into()
    }
    fn request(&self, _: &Tv) -> Result<crate::ask::Request> {
        Ok(crate::ask::Request::Get { host: "127.0.0.1".into(), port: self.port, path: "/json".into() })
    }
    fn parse(&self, reply: &crate::ask::Reply) -> Result<Option<Self::Answer>> {
        if reply.status != 200 {
            return failed(format!("devtools: HTTP {} for /json", reply.status));
        }
        Ok(Some(pages_of(&reply.body)))
    }
}

/// The pages in a `/json` reply.
pub fn pages_of(list: &str) -> Vec<(String, Option<String>)> {
    let list = http::json(list).unwrap_or_default();
    list.as_array()
        .into_iter()
        .flatten()
        .filter(|o| http::text(o, "type").is_none_or(|t| t == "page"))
        .filter_map(|o| {
            let url = http::text(o, "url")?;
            Some((url, http::text(o, "webSocketDebuggerUrl").and_then(|w| ws_path(&w))))
        })
        .collect()
}

/// A forwarded connection to one app's devtools server. Kept open, it makes
/// repeated queries (a UI refreshing every second) cheap.
pub struct Conn {
    pub pkg: String,
    fwd: crate::tv::Forward,
}

impl Conn {
    /// Find and forward `pkg`'s devtools server. Servers that say they belong
    /// to another app are skipped.
    pub fn open(tv: &Tv, pkg: &str) -> Option<Conn> {
        let unix = tv.shell(&["cat", "/proc/net/unix"]).ok()?;
        let mut socks = sockets(&unix);
        // a WebView's socket names its process: the app's own one first (a
        // server that doesn't say whose it is would do otherwise)
        let pid = tv.shell(&["pidof", pkg]).unwrap_or_default();
        if let Some(own) = pid.split_whitespace().find_map(|p| webview_socket(&unix, p)) {
            socks.retain(|s| *s != own);
            socks.insert(0, own);
        }
        socks.into_iter().find_map(|sock| {
            let fwd = tv.forward_abstract(&sock).ok()?;
            let says = tv.ask(&Version { port: fwd.port }).ok()?;
            let theirs = says.is_some_and(|p| p != pkg);
            (!theirs).then(|| Conn { pkg: pkg.to_string(), fwd })
        })
    }

    /// (url, websocket path) of each open page.
    fn page_list(&self) -> Result<Vec<(String, Option<String>)>> {
        self.fwd.tv.ask(&Pages { port: self.fwd.port })
    }

    /// The open pages, with their video progress.
    pub fn pages(&self) -> Result<Vec<Page>> {
        Ok(self
            .page_list()?
            .into_iter()
            .map(|(url, ws)| {
                let progress = ws.and_then(|p| evaluate(self.fwd.port, &p, VIDEO_EXPR).ok()).and_then(|v| parse_progress(&v));
                Page { url, progress }
            })
            .collect())
    }

    /// Run `expr` on the first page that answers; returns its string result.
    pub fn eval(&self, expr: &str) -> Result<String> {
        for (_, ws) in self.page_list()? {
            let Some(ws) = ws else { continue };
            if let Ok(v) = evaluate(self.fwd.port, &ws, expr) {
                return Ok(v);
            }
        }
        failed("devtools: no page answered")
    }

    /// Run `expr` on each open page: (url, its string result, or why not).
    pub fn eval_each(&self, expr: &str) -> Result<Vec<(String, Result<String>)>> {
        Ok(self
            .page_list()?
            .into_iter()
            .map(|(url, ws)| {
                let r = match ws {
                    Some(ws) => evaluate(self.fwd.port, &ws, expr),
                    None => failed("devtools: the page has no debugger (another client may have it)"),
                };
                (url, r)
            })
            .collect())
    }

    /// Move the page's video: by `delta` seconds, or to `fraction` of its length.
    /// Returns the new position.
    pub fn seek(&self, to: SeekTo) -> Result<f64> {
        let expr = seek_expr(to);
        for (_, ws) in self.page_list()? {
            let Some(ws) = ws else { continue };
            if let Some(pos) = evaluate(self.fwd.port, &ws, &expr).ok().and_then(|v| v.parse().ok()) {
                return Ok(pos);
            }
        }
        failed("no video on the page to seek")
    }
}

impl Conn {
    /// Call `on` with each event the page's videos fire (play, pause,
    /// seeked, ended, a new video's loadedmetadata) as it happens, until
    /// `go` says to stop (asked every second) or the page goes. Done with a
    /// binding (`Runtime.addBinding`) the page's listeners call.
    pub fn watch(&self, on: &dyn Fn(&str), go: &dyn Fn() -> bool) -> Result<()> {
        let Some(path) = self.page_list()?.into_iter().find_map(|(_, ws)| ws) else {
            return Err(Error::Unavailable("devtools: no page".into()));
        };
        let mut ws = WsClient::connect("127.0.0.1", self.fwd.port, &path)?;
        let mut id = 0;
        let mut ask = |ws: &mut WsClient, method: &str, params: String| -> Result<serde_json::Value> {
            id += 1;
            ws.send_text(&format!(r#"{{"id":{id},"method":"{method}","params":{params}}}"#))?;
            for _ in 0..50 {
                let Some(text) = ws.recv_text()? else { break };
                let Some(reply) = http::json(&text) else { continue };
                if reply.get("id").and_then(serde_json::Value::as_u64) == Some(id) {
                    if let Some(e) = reply.get("error") {
                        return Err(Error::Unavailable(format!("devtools: {method}: {e}")));
                    }
                    return Ok(reply);
                }
            }
            failed(format!("devtools: no reply to {method}"))
        };
        ask(&mut ws, "Runtime.addBinding", format!(r#"{{"name":"{BINDING}"}}"#))?;
        let install = format!(r#"{{"expression":{},"returnByValue":true}}"#, serde_json::Value::from(LISTEN_EXPR));
        ask(&mut ws, "Runtime.evaluate", install.clone())?;
        crate::tlog!("devtools: watching {}'s videos", self.pkg);
        ws.set_timeout(Duration::from_secs(1))?;
        // the page may load again (dropping the listeners): put them back now and then
        let mut installed = std::time::Instant::now();
        while go() {
            match ws.recv()? {
                Recv::Closed => break,
                Recv::Quiet => {}
                Recv::Text(t) => {
                    let Some(v) = http::json(&t) else { continue };
                    if v.get("method").and_then(serde_json::Value::as_str) == Some("Runtime.bindingCalled")
                        && v.pointer("/params/name").and_then(serde_json::Value::as_str) == Some(BINDING)
                    {
                        on(v.pointer("/params/payload").and_then(serde_json::Value::as_str).unwrap_or(""));
                    }
                }
            }
            if installed.elapsed() > Duration::from_secs(30) {
                ws.set_timeout(Duration::from_secs(5))?;
                ask(&mut ws, "Runtime.evaluate", install.clone())?;
                ws.set_timeout(Duration::from_secs(1))?;
                installed = std::time::Instant::now();
            }
        }
        Ok(())
    }
}

/// The function the page calls with its videos' events.
const BINDING: &str = "tveeeEvent";

/// Listen (once: earlier listeners are replaced) for media events from any
/// video on the page; they don't bubble, so on the way down.
const LISTEN_EXPR: &str = "(()=>{const T=['play','pause','seeked','ended','loadedmetadata'];\
if(window.__tveee)for(const t of T)document.removeEventListener(t,window.__tveee,true);\
window.__tveee=e=>{if(e.target&&e.target.tagName==='VIDEO'){try{tveeeEvent(e.type)}catch(_){}}};\
for(const t of T)document.addEventListener(t,window.__tveee,true);return 'ok'})()";

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SeekTo {
    By(f64),
    Fraction(f64),
    /// To this many seconds from the start.
    At(f64),
}

/// JS that seeks the longest video and returns its new position as a string
/// ('' when there's no video, or no duration for a fractional seek).
pub fn seek_expr(to: SeekTo) -> String {
    let target = match to {
        SeekTo::By(d) => format!("v.currentTime+({d})"),
        SeekTo::Fraction(f) => format!("isFinite(v.duration)?v.duration*({f}):NaN"),
        SeekTo::At(t) => format!("({t})"),
    };
    format!(
        "(()=>{{const vs=[...document.querySelectorAll('video')].filter(v=>v.readyState>0);\
if(!vs.length)return '';vs.sort((a,b)=>(b.duration||0)-(a.duration||0));const v=vs[0];\
const t={target};if(isNaN(t))return '';\
v.currentTime=Math.max(0,isFinite(v.duration)?Math.min(t,v.duration-1):t);return String(v.currentTime)}})()"
    )
}

/// `ws://127.0.0.1:9229/devtools/page/ID` -> `/devtools/page/ID`
pub fn ws_path(ws_url: &str) -> Option<String> {
    let rest = ws_url.strip_prefix("ws://")?;
    Some(rest[rest.find('/')?..].to_string())
}

/// Run `Runtime.evaluate` on a page and return the string result.
pub fn evaluate(port: u16, path: &str, expr: &str) -> Result<String> {
    let mut ws = WsClient::connect("127.0.0.1", port, path)?;
    let quoted = format!("\"{}\"", expr.replace('\\', "\\\\").replace('"', "\\\""));
    ws.send_text(&format!(r#"{{"id":1,"method":"Runtime.evaluate","params":{{"expression":{quoted},"returnByValue":true}}}}"#))?;
    // skip any events until our reply arrives
    for _ in 0..20 {
        let Some(text) = ws.recv_text()? else { break };
        let Some(reply) = http::json(&text) else { continue };
        if reply.get("id").and_then(serde_json::Value::as_u64) == Some(1) {
            // {"id":1,"result":{"result":{"type":"string","value":"..."}}}
            return match reply.pointer("/result/result/value") {
                Some(serde_json::Value::String(s)) => Ok(s.clone()),
                Some(serde_json::Value::Null) | None => Err(Error::Failed("devtools: no value in reply".into())),
                Some(other) => Ok(other.to_string()),
            };
        }
    }
    failed("devtools: no reply")
}

/// JSON with the page's route, title, focused element and the visible,
/// focusable things on it (tag and text), for working out what a screen is.
pub const PAGE_EXPR: &str = "(()=>{const f=document.activeElement;\
const txt=e=>(e.innerText||e.getAttribute('aria-label')||'').trim().replace(/\\s+/g,' ').slice(0,90);\
const seen=new Set();const items=[];\
for(const e of document.querySelectorAll('[tabindex],[role=button],[role=option],[aria-label]')){\
if(!e.offsetWidth)continue;const t=txt(e);if(!t||seen.has(t))continue;seen.add(t);\
items.push({tag:e.tagName.toLowerCase(),text:t,focused:e===f||e.contains(f)});if(items.length>=40)break}\
return JSON.stringify({route:location.hash,title:document.title,\
focused:f?{tag:f.tagName.toLowerCase(),text:txt(f)}:null,items})})()";

/// [`PAGE_EXPR`]'s answer as lines: the route, the title, then the items
/// (> marks the focused one).
pub fn page_lines(raw: &str) -> Vec<String> {
    let v = http::json(raw).unwrap_or_default();
    let field = |k: &str| http::text(&v, k).unwrap_or_default();
    let mut out = vec![format!("route:   {}", field("route")), format!("title:   {}", field("title"))];
    let items = v.get("items").and_then(serde_json::Value::as_array).cloned().unwrap_or_default();
    if items.is_empty() {
        out.push("  (nothing focusable is visible: is the app in front?)".into());
    }
    for o in &items {
        let tag = http::text(o, "tag").unwrap_or_default();
        let text = http::text(o, "text").unwrap_or_default();
        let focused = o.get("focused").and_then(serde_json::Value::as_bool).unwrap_or(false);
        out.push(format!("  {} {tag:<32} {text}", if focused { ">" } else { " " }));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_list_from_json() {
        let list = r#"[{"type":"page","url":"https://www.youtube.com/tv#/watch?v=x","webSocketDebuggerUrl":"ws://127.0.0.1:9222/devtools/page/1"},{"type":"service_worker","url":"sw.js"}]"#;
        let pages = pages_of(list);
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].0, "https://www.youtube.com/tv#/watch?v=x");
        assert!(pages[0].1.is_some());
        assert!(pages_of("not json").is_empty());
    }

    #[test]
    fn reads_page_items() {
        let lines =
            page_lines(r##"{"route":"#/","title":"YouTube on TV","items":[{"tag":"a","text":"A","focused":true},{"tag":"b","text":"B","focused":false}]}"##);
        assert_eq!(lines[0], "route:   #/");
        assert!(lines[2].starts_with("  > a") && lines[3].starts_with("    b"), "{lines:?}");
        // the focused element is reported too: the items are what's listed
        let raw = r##"{"route":"#/","title":"YouTube on TV","focused":{"tag":"ytlr-tile-renderer","text":"A"},"items":[{"tag":"ytlr-tile-renderer","text":"A","focused":true},{"tag":"ytlr-button","text":"B","focused":false}]}"##;
        let lines = page_lines(raw);
        assert_eq!(lines.len(), 4);
        assert!(lines[3].contains("ytlr-button") && lines[3].ends_with(" B"));
    }

    #[test]
    fn finds_sockets() {
        let unix = "Num       RefCount Protocol Flags    Type St Inode Path
00000000: 00000002 00000000 00010000 0001 01 520622 @content_shell_devtools_remote
00000000: 00000002 00000000 00010000 0001 01 1234 @webview_devtools_remote_2020
00000000: 00000003 00000000 00000000 0001 03 99 /dev/socket/zygote
00000000: 00000002 00000000 00010000 0001 01 520623 @content_shell_devtools_remote";
        assert_eq!(sockets(unix), vec!["content_shell_devtools_remote", "webview_devtools_remote_2020"]);
        assert_eq!(webview_socket(unix, "2020\n").as_deref(), Some("webview_devtools_remote_2020"));
        assert_eq!(webview_socket(unix, "202"), None);
    }

    #[test]
    fn parses_video_progress() {
        assert_eq!(
            parse_progress("[1516.342226,1651.061,false]"),
            Some(Progress { position: 1516.342226, duration: Some(1651.061), paused: false, rate: 1.0 })
        );
        // live: duration is Infinity, which JSON.stringify writes as null
        assert_eq!(parse_progress("[12,null,true]").unwrap().duration, None);
        assert_eq!(parse_progress(""), None);
        assert_eq!(parse_progress("[NaN,1,false]"), None);
    }

    #[test]
    fn seek_expressions() {
        assert!(seek_expr(SeekTo::By(-10.0)).contains("const t=v.currentTime+(-10);"));
        assert!(seek_expr(SeekTo::Fraction(0.5)).contains("v.duration*(0.5)"));
    }

    #[test]
    fn ws_paths() {
        assert_eq!(ws_path("ws://127.0.0.1:9229/devtools/page/ABC").as_deref(), Some("/devtools/page/ABC"));
        assert_eq!(ws_path("http://x/y"), None);
    }
}

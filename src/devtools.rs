//! Chromium/Cobalt remote-debugging ("devtools") servers on the TV.
//!
//! Browser-based apps (the YouTube app is Cobalt) listen on an abstract unix
//! socket named `*devtools_remote*`. Forwarded with adb it serves `/json`
//! (the open pages) and a WebSocket per page that speaks the DevTools
//! protocol, which lets us read the page's `<video>` element. Everything here
//! is read-only.

use crate::error::{failed, Error, Result};
use crate::http;
use crate::tv::Tv;
use std::io::{Read, Write};
use std::net::TcpStream;
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

/// Keep connections open between calls (the ui, which asks every second);
/// otherwise each call opens one and closes it (one-off commands).
static KEEP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static KEPT: std::sync::Mutex<Option<Conn>> = std::sync::Mutex::new(None);

/// Keep connections open (the ui), or close the kept one and stop keeping.
#[cfg_attr(not(feature = "ui"), allow(dead_code))]
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
    Some(Progress { position, duration, paused: *paused == "true" })
}

/// A forwarded connection to one app's devtools server. Kept open, it makes
/// repeated queries (a UI refreshing every second) cheap.
#[cfg_attr(not(feature = "ui"), allow(dead_code))]
pub struct Conn {
    pub pkg: String,
    fwd: crate::tv::Forward,
}

impl Conn {
    /// Find and forward `pkg`'s devtools server. Servers that say they belong
    /// to another app are skipped.
    pub fn open(tv: &Tv, pkg: &str) -> Option<Conn> {
        let unix = tv.shell(&["cat", "/proc/net/unix"]).ok()?;
        sockets(&unix).into_iter().find_map(|sock| {
            let fwd = tv.forward_abstract(&sock).ok()?;
            let (status, v) = http::get("127.0.0.1", fwd.port, "/json/version").ok()?;
            let theirs = http::json(&v).and_then(|v| http::text(&v, "Android-Package")).is_some_and(|p| p != pkg);
            (status == 200 && !theirs).then(|| Conn { pkg: pkg.to_string(), fwd })
        })
    }

    /// (url, websocket path) of each open page.
    fn page_list(&self) -> Result<Vec<(String, Option<String>)>> {
        let (status, list) = http::get("127.0.0.1", self.fwd.port, "/json")?;
        if status != 200 {
            return failed(format!("devtools: HTTP {status} for /json"));
        }
        let list = http::json(&list).unwrap_or_default();
        Ok(list
            .as_array()
            .into_iter()
            .flatten()
            .filter(|o| http::text(o, "type").is_none_or(|t| t == "page"))
            .filter_map(|o| {
                let url = http::text(o, "url")?;
                Some((url, http::text(o, "webSocketDebuggerUrl").and_then(|w| ws_path(&w))))
            })
            .collect())
    }

    /// The open pages, with their video progress.
    pub fn pages(&self) -> Result<Vec<Page>> {
        Ok(self
            .page_list()?
            .into_iter()
            .map(|(url, ws)| {
                let progress = ws
                    .and_then(|p| evaluate(self.fwd.port, &p, VIDEO_EXPR).ok())
                    .and_then(|v| parse_progress(&v));
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

    /// Move the page's video: by `delta` seconds, or to `fraction` of its length.
    /// Returns the new position.
    #[cfg_attr(not(feature = "ui"), allow(dead_code))]
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

#[cfg_attr(not(feature = "ui"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SeekTo {
    By(f64),
    Fraction(f64),
    /// To this many seconds from the start.
    At(f64),
}

/// JS that seeks the longest video and returns its new position as a string
/// ('' when there's no video, or no duration for a fractional seek).
#[cfg_attr(not(feature = "ui"), allow(dead_code))]
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

/// A client-to-server WebSocket text frame (clients must mask).
pub fn text_frame(payload: &[u8], mask: [u8; 4]) -> Vec<u8> {
    let mut f = vec![0x81];
    match payload.len() {
        n if n < 126 => f.push(0x80 | n as u8),
        n if n <= 0xffff => {
            f.push(0x80 | 126);
            f.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            f.push(0x80 | 127);
            f.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    f.extend_from_slice(&mask);
    f.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
    f
}

fn read_exact(s: &mut TcpStream, n: usize) -> Result<Vec<u8>> {
    let mut buf = vec![0; n];
    s.read_exact(&mut buf)?;
    Ok(buf)
}

/// Read one server frame's payload (servers don't mask).
fn read_frame(s: &mut TcpStream) -> Result<(u8, Vec<u8>)> {
    let h = read_exact(s, 2)?;
    let len = match h[1] & 0x7f {
        126 => u16::from_be_bytes(read_exact(s, 2)?.try_into().unwrap()) as usize,
        127 => u64::from_be_bytes(read_exact(s, 8)?.try_into().unwrap()) as usize,
        n => n as usize,
    };
    if len > 1 << 20 {
        return failed("devtools: frame too large");
    }
    Ok((h[0] & 0x0f, read_exact(s, len)?))
}

fn mask() -> [u8; 4] {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    (t ^ std::process::id().rotate_left(16)).to_be_bytes()
}

/// Run `Runtime.evaluate` on a page and return the string result.
pub fn evaluate(port: u16, path: &str, expr: &str) -> Result<String> {
    let mut s = TcpStream::connect(("127.0.0.1", port))?;
    s.set_read_timeout(Some(Duration::from_secs(5)))?;
    // a fixed key is fine: we don't need to check the server's accept hash
    write!(
        s,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Key: dHZiZWUtZGV2dG9vbHMhIQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
    )?;
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        head.extend(read_exact(&mut s, 1)?);
        if head.len() > 8192 {
            return failed("devtools: bad handshake");
        }
    }
    if !head.starts_with(b"HTTP/1.1 101") {
        return failed("devtools: websocket refused");
    }
    let quoted = format!("\"{}\"", expr.replace('\\', "\\\\").replace('"', "\\\""));
    let msg = format!(
        r#"{{"id":1,"method":"Runtime.evaluate","params":{{"expression":{quoted},"returnByValue":true}}}}"#
    );
    s.write_all(&text_frame(msg.as_bytes(), mask()))?;
    // skip any events until our reply arrives
    for _ in 0..20 {
        let (op, data) = read_frame(&mut s)?;
        if op == 8 {
            break;
        }
        let Some(reply) = http::json(&String::from_utf8_lossy(&data)) else { continue };
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
    fn reads_page_items() {
        let lines = page_lines(r##"{"route":"#/","title":"YouTube on TV","items":[{"tag":"a","text":"A","focused":true},{"tag":"b","text":"B","focused":false}]}"##);
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
    }

    #[test]
    fn parses_video_progress() {
        assert_eq!(
            parse_progress("[1516.342226,1651.061,false]"),
            Some(Progress { position: 1516.342226, duration: Some(1651.061), paused: false })
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

    #[test]
    fn frames_are_masked() {
        let f = text_frame(b"hi", [1, 2, 3, 4]);
        assert_eq!(f, vec![0x81, 0x82, 1, 2, 3, 4, b'h' ^ 1, b'i' ^ 2]);
        let long = text_frame(&[0; 300], [0; 4]);
        assert_eq!(&long[1..4], &[0x80 | 126, 1, 44]);
    }
}

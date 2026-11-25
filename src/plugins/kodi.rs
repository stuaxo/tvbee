//! Kodi, through its own remote-control API (JSON-RPC over HTTP, port 8080
//! by default). It's off until switched on in Kodi: Settings > Services >
//! Control > "Allow remote control via HTTP". With it, tveee knows what Kodi
//! is playing and exactly where (the media session only gives a title),
//! seeks exactly, shows the artwork in the ui's preview (Kodi's video is on
//! a layer pictures can't see), goes to places in Kodi, plays links in it,
//! and restarts it back where it was.
//!
//! No password set in Kodi: nothing to do. With one: `tveee pair kodi`.

use crate::devtools::{Progress, SeekTo};
use crate::error::{failed, Error, Result};
use crate::more::Place;
use crate::provider::{Outcome, Playing, Session};
use crate::registry::Command;
use crate::tv::Tv;

pub const PACKAGE: &str = "org.xbmc.kodi";
const PAIRING: &str = "kodi";
const DEFAULT_PORT: u16 = 8080;
const DEFAULT_USER: &str = "kodi";

pub fn detect(tv: &Tv) -> Result<bool> {
    let out = tv.shell(&["cmd", "package", "list", "packages", PACKAGE])?;
    Ok(out.lines().any(|l| l.trim() == format!("package:{PACKAGE}")))
}

// --- JSON bits ---------------------------------------------------------------

use serde_json::Value;

/// A non-empty string at `key`.
fn str_of(v: &Value, key: &str) -> Option<String> {
    v.get(key)?.as_str().filter(|s| !s.is_empty()).map(str::to_string)
}

fn num_of(v: &Value, key: &str) -> Option<f64> {
    v.get(key)?.as_f64()
}

/// Kodi's time object (`{"hours":0,"minutes":3,"seconds":20,"milliseconds":5}`) in seconds.
pub fn seconds(time: &Value) -> Option<f64> {
    let n = |k| num_of(time, k).unwrap_or(0.0);
    time.get("seconds")?;
    Some(n("hours") * 3600.0 + n("minutes") * 60.0 + n("seconds") + n("milliseconds") / 1000.0)
}

/// Seconds as Kodi's time object.
pub fn time_json(secs: f64) -> String {
    let ms = (secs.max(0.0) * 1000.0).round() as u64;
    format!(
        r#"{{"hours":{},"minutes":{},"seconds":{},"milliseconds":{}}}"#,
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}

/// `s` as a JSON string.
fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// --- talking to Kodi -----------------------------------------------------------

/// Where Kodi's API is, and the login: saved by `tveee pair kodi`, else
/// Kodi's defaults (port 8080, user kodi, no password).
fn login(tv: &Tv) -> (u16, String, String) {
    match crate::pairing::load(PAIRING, &tv.ip) {
        Some(s) => (
            s.get("port").and_then(|p| p.parse().ok()).unwrap_or(DEFAULT_PORT),
            s.get("user").unwrap_or(DEFAULT_USER).to_string(),
            s.get("pass").unwrap_or("").to_string(),
        ),
        None => (DEFAULT_PORT, DEFAULT_USER.into(), String::new()),
    }
}

fn off() -> Error {
    Error::Unavailable(
        "Kodi isn't answering: it isn't running, or its remote control is off (in Kodi: Settings > Services > Control > Allow remote control via HTTP)"
            .into(),
    )
}

/// POST `body` to Kodi's JSON-RPC; returns the reply.
fn post(tv: &Tv, login: &(u16, String, String), body: &str) -> Result<String> {
    let (port, user, pass) = login;
    let url = format!("http://{}:{port}/jsonrpc", tv.ip);
    let auth = (!pass.is_empty()).then_some((user.as_str(), pass.as_str()));
    let reply = crate::https::request(&crate::https::Request { url: &url, body: Some(body), digest: None, basic: auth, retry: None, timeout_secs: 4 });
    match reply {
        Ok((200, text)) => Ok(text),
        Ok((401, _)) => failed("Kodi wants a username and password: run 'tveee pair kodi'"),
        Ok((code, _)) => failed(format!("Kodi answered HTTP {code}")),
        Err(_) => Err(off()),
    }
}

/// Call `method` with `params` (a JSON object); returns its `result`.
pub fn call(tv: &Tv, method: &str, params: &str) -> Result<Value> {
    let body = format!(r#"{{"jsonrpc":"2.0","id":1,"method":"{method}","params":{params}}}"#);
    let reply = post(tv, &login(tv), &body)?;
    result_of(&reply, method)
}

/// The `result` of a JSON-RPC reply, or its error.
pub fn result_of(reply: &str, method: &str) -> Result<Value> {
    let Ok(mut v) = serde_json::from_str::<Value>(reply) else { return failed(format!("Kodi: {method}: not JSON: {}", reply.trim())) };
    if let Some(e) = v.get("error") {
        let msg = str_of(e, "message").unwrap_or_else(|| e.to_string());
        return failed(format!("Kodi: {method}: {msg}"));
    }
    match v.get_mut("result") {
        Some(r) => Ok(r.take()),
        None => failed(format!("Kodi: {method}: no result")),
    }
}

/// Whether Kodi's API answers now.
pub fn ping(tv: &Tv) -> bool {
    call(tv, "JSONRPC.Ping", "{}").is_ok_and(|r| r == "pong")
}

/// Wait for Kodi's API to answer (Kodi starting), up to `secs`.
fn wait_ready(tv: &Tv, secs: u64) -> Result<()> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    while std::time::Instant::now() < deadline {
        if ping(tv) {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(700));
    }
    Err(off())
}

/// Kodi's active player: (id, "video" | "audio" | "picture").
fn player_id(tv: &Tv) -> Result<Option<(u32, String)>> {
    let r = call(tv, "Player.GetActivePlayers", "{}")?;
    let first = r.get(0);
    Ok(first.and_then(|p| num_of(p, "playerid")).map(|id| (id as u32, first.and_then(|p| str_of(p, "type")).unwrap_or_default())))
}

const ITEM_FIELDS: &str = r#"["title","showtitle","season","episode","artist","album","year","file","art","thumbnail","fanart","streamdetails"]"#;
const PLAYER_FIELDS: &str = r#"["time","totaltime","speed","currentaudiostream","currentsubtitle","subtitleenabled"]"#;

/// What's playing, from Player.GetItem's and Player.GetProperties' results.
pub fn parse_playing(item: &Value, props: &Value) -> Playing {
    let item = item.get("item").unwrap_or(item);
    let title = str_of(item, "title").or_else(|| str_of(item, "label"));
    let num = |k| num_of(item, k).filter(|n| *n >= 0.0).map(|n| n as u32);
    let subtitle = match (str_of(item, "showtitle"), num("season"), num("episode")) {
        (Some(show), Some(s), Some(e)) => Some(format!("{show} S{s:02}E{e:02}")),
        (Some(show), _, _) => Some(show),
        _ => item
            .get("artist")
            .and_then(|a| a.get(0)?.as_str().filter(|a| !a.is_empty()).map(str::to_string))
            .or_else(|| num("year").filter(|y| *y > 0).map(|y| y.to_string())),
    };
    let time = props.get("time").and_then(seconds);
    let total = props.get("totaltime").and_then(seconds).filter(|t| *t > 0.0);
    let paused = num_of(props, "speed").is_some_and(|s| s == 0.0);
    let progress = time.map(|position| Progress { position, duration: total, paused });
    // a picture: fanart or a poster/thumb
    let art = item
        .get("art")
        .and_then(|a| ["fanart", "poster", "thumb"].iter().find_map(|k| str_of(a, k)))
        .or_else(|| str_of(item, "fanart"))
        .or_else(|| str_of(item, "thumbnail"));
    let mut extra = Vec::new();
    if let Some(v) = item.pointer("/streamdetails/video/0") {
        let h = num_of(v, "height").unwrap_or(0.0) as u32;
        let codec = str_of(v, "codec").unwrap_or_default().to_uppercase();
        if h > 0 {
            extra.push(("video", format!("{h}p {codec}").trim().to_string()));
        }
    }
    let track = |k: &str| {
        let t = props.get(k)?;
        let name = str_of(t, "name").or_else(|| str_of(t, "language"))?;
        let codec = str_of(t, "codec").map(|c| format!(" {}", c.to_uppercase())).unwrap_or_default();
        Some(format!("{name}{codec}"))
    };
    if let Some(a) = track("currentaudiostream") {
        extra.push(("audio", a));
    }
    let subs_on = props.get("subtitleenabled").and_then(Value::as_bool).unwrap_or(false);
    if let Some(s) = track("currentsubtitle").filter(|_| subs_on) {
        extra.push(("subtitles", s));
    }
    let link = None;
    Playing { title, subtitle, progress, link, art, art_moves: false, extra }
}

/// What Kodi is playing, and its file (for sessions).
fn now(tv: &Tv) -> Result<Option<(Playing, Option<String>)>> {
    let Some((id, _)) = player_id(tv)? else { return Ok(None) };
    let item = call(tv, "Player.GetItem", &format!(r#"{{"playerid":{id},"properties":{ITEM_FIELDS}}}"#))?;
    let props = call(tv, "Player.GetProperties", &format!(r#"{{"playerid":{id},"properties":{PLAYER_FIELDS}}}"#))?;
    let file = item.get("item").and_then(|i| str_of(i, "file"));
    let mut playing = parse_playing(&item, &props);
    // the file is the link (`tveee details`), unless it has a login in it:
    // it's shown
    playing.link = file.clone().filter(|f| !has_login(f));
    // a copy this computer can read: frames of it for the preview, not the artwork
    if let Some(local) = file.as_deref().and_then(|f| local_path(f, &std::env::var(PATHS_VAR).unwrap_or_default())).filter(|p| p.is_file()) {
        let art = playing.art.take().unwrap_or_default();
        playing.art = Some(format!("{FRAME}{}\n{art}", local.display()));
        playing.art_moves = true;
    }
    Ok(Some((playing, file)))
}

/// Whether a path has a login in it (`smb://user:pass@nas/...`).
pub fn has_login(path: &str) -> bool {
    path.split_once("://").is_some_and(|(_, rest)| rest.split('/').next().is_some_and(|host| host.contains('@')))
}

/// Where Kodi's files are on this computer: `TVEEE_KODI_PATHS`, pairs of
/// Kodi's path and the local one, `;`-separated
/// (`smb://nas/media=/mnt/nas/media`).
const PATHS_VAR: &str = "TVEEE_KODI_PATHS";
/// [`Playing::art`] for a frame of a local copy: `frame:<path>\n<artwork>`.
const FRAME: &str = "frame:";

/// Kodi's `file` on this computer, by the first mapping in `map` it starts with.
pub fn local_path(file: &str, map: &str) -> Option<std::path::PathBuf> {
    map.split(';').filter_map(|pair| pair.split_once('=')).find_map(|(kodi, here)| {
        let kodi = kodi.trim().trim_end_matches('/');
        let rest = file.strip_prefix(kodi)?;
        // a whole path part, not "media2" for "media"
        if !(rest.is_empty() || rest.starts_with('/')) {
            return None;
        }
        let rest = percent_decode(rest);
        Some(std::path::PathBuf::from(format!("{}{rest}", here.trim().trim_end_matches('/'))))
    })
}

/// `%20` and the like, as Kodi writes some paths.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        match (b[i], b.get(i + 1).copied().and_then(hex), b.get(i + 2).copied().and_then(hex)) {
            (b'%', Some(h), Some(l)) => {
                out.push((h * 16 + l) as u8);
                i += 3;
            }
            (c, _, _) => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The body that seeks to `to` (Kodi's own forms: relative seconds, a
/// percentage, or a time).
pub fn seek_value(to: SeekTo) -> String {
    match to {
        SeekTo::By(d) => format!(r#"{{"seconds":{}}}"#, d.round() as i64),
        SeekTo::Fraction(f) => format!(r#"{{"percentage":{:.3}}}"#, (f * 100.0).clamp(0.0, 100.0)),
        SeekTo::At(t) => format!(r#"{{"time":{}}}"#, time_json(t)),
    }
}

// --- the provider --------------------------------------------------------------

pub struct Kodi;
pub static KODI: Kodi = Kodi;

const PLACES: &[Place] = &[
    Place { id: "home", label: "Home", link: "kodi-window:home" },
    Place { id: "movies", label: "Movies", link: "kodi-window:videos|videodb://movies/titles/" },
    Place { id: "tvshows", label: "TV shows", link: "kodi-window:videos|videodb://tvshows/titles/" },
    Place { id: "music", label: "Music", link: "kodi-window:music" },
    Place { id: "addons", label: "Add-ons", link: "kodi-window:addonbrowser" },
    Place { id: "favourites", label: "Favourites", link: "kodi-window:favouritesbrowser" },
    Place { id: "settings", label: "Settings", link: "kodi-window:settings" },
];

/// The GUI.ActivateWindow params for a `kodi-window:` link.
pub fn window_params(link: &str) -> Option<String> {
    let w = link.strip_prefix("kodi-window:")?;
    Some(match w.split_once('|') {
        Some((window, param)) => format!(r#"{{"window":{},"parameters":[{}]}}"#, quote(window), quote(param)),
        None => format!(r#"{{"window":{}}}"#, quote(w)),
    })
}

/// Kodi in front, and its API answering (starting Kodi if need be).
fn bring_up(tv: &Tv) -> Result<()> {
    let _ = crate::plugins::apps::bring_to_front(tv, PACKAGE);
    wait_ready(tv, 40)
}

impl crate::provider::Provider for Kodi {
    fn id(&self) -> &'static str {
        "kodi"
    }
    fn name(&self) -> &'static str {
        "Kodi"
    }
    fn hosts(&self) -> &'static [&'static str] {
        &[PACKAGE]
    }
    fn places(&self) -> &'static [Place] {
        PLACES
    }
    fn player(&self) -> Option<&dyn crate::provider::Player> {
        Some(self)
    }
    fn details(&self) -> Option<&dyn crate::provider::Details> {
        Some(self)
    }
    fn actions(&self) -> Option<(&'static str, crate::provider::Actions)> {
        Some((ACTIONS, action))
    }
    fn has_sessions(&self) -> bool {
        true
    }
    // what's playing and how far in: restarting comes back to it
    fn save_session(&self, tv: &Tv) -> Outcome<Session> {
        let saved = now(tv).map(|n| {
            let (playing, file) = n?;
            let at = playing.progress.map(|p| p.position.floor())?;
            Some(Session { link: file?, said: format!("at {} in the same file", crate::nowplaying::clock(at)), resume: Some(at) })
        });
        match Outcome::from(saved) {
            Outcome::Handled(Some(s)) => Outcome::Handled(s),
            Outcome::Handled(None) | Outcome::Fallback => Outcome::Fallback,
            Outcome::Failed(e) => Outcome::Failed(e),
        }
    }
    fn restore_session(&self, tv: &Tv, target: &str, s: &Session) -> Outcome<()> {
        let restored = (|| {
            crate::plugins::apps::start(tv, target)?;
            wait_ready(tv, 40)?;
            let options = s.resume.map(|t| format!(r#","options":{{"resume":{}}}"#, time_json(t))).unwrap_or_default();
            call(tv, "Player.Open", &format!(r#"{{"item":{{"file":{}}}{options}}}"#, quote(&s.link))).map(drop)
        })();
        // started, but its remote control never answered: that's a failure,
        // not "do it another way" (Kodi is already up)
        match restored {
            Err(Error::Unavailable(m)) => Outcome::Failed(Error::Failed(m)),
            r => r.into(),
        }
    }
    // places in Kodi, and media links played in the running Kodi
    fn open_here(&self, tv: &Tv, link: &str) -> Outcome<()> {
        if let Some(params) = window_params(link) {
            // only this way: not reaching Kodi is a failure
            return match bring_up(tv).and_then(|()| call(tv, "GUI.ActivateWindow", &params)) {
                Ok(_) => Outcome::Handled(()),
                Err(e) => Outcome::Failed(Error::Failed(e.to_string())),
            };
        }
        // its remote control off (or Kodi not running): the usual way, an intent
        if !ping(tv) {
            return Outcome::Fallback;
        }
        let played = call(tv, "Player.Open", &format!(r#"{{"item":{{"file":{}}}}}"#, quote(link)));
        if played.is_ok() {
            let _ = crate::plugins::apps::bring_to_front(tv, PACKAGE);
        }
        played.map(drop).into()
    }
}

impl crate::provider::Player for Kodi {
    fn playing(&self, tv: &Tv) -> Outcome<Playing> {
        match Outcome::from(now(tv)) {
            Outcome::Handled(Some((p, _))) => Outcome::Handled(p),
            Outcome::Handled(None) | Outcome::Fallback => Outcome::Fallback,
            Outcome::Failed(e) => Outcome::Failed(e),
        }
    }
    fn seek(&self, tv: &Tv, to: SeekTo) -> Outcome<f64> {
        let seeked = player_id(tv).and_then(|p| {
            // nothing playing: keys won't do better, but they can't hurt
            let Some((id, _)) = p else { return Err(Error::Unavailable("Kodi isn't playing anything".into())) };
            let r = call(tv, "Player.Seek", &format!(r#"{{"playerid":{id},"value":{}}}"#, seek_value(to)))?;
            r.get("time").and_then(seconds).map_or_else(|| failed("Kodi didn't say where it seeked to"), Ok)
        });
        seeked.into()
    }
    fn play_pause(&self, tv: &Tv) -> Outcome<()> {
        let done = player_id(tv).and_then(|p| {
            let Some((id, _)) = p else { return Err(Error::Unavailable("Kodi isn't playing anything".into())) };
            call(tv, "Player.PlayPause", &format!(r#"{{"playerid":{id}}}"#)).map(drop)
        });
        done.into()
    }
    fn picture(&self, tv: &Tv, art: &str, at: Option<f64>) -> Outcome<Vec<u8>> {
        let art = match art.strip_prefix(FRAME).and_then(|a| a.split_once('\n')) {
            Some((path, artwork)) => {
                if let Some(jpeg) = crate::plugins::youtube::frame_of(std::path::Path::new(path), at.unwrap_or(0.0)) {
                    return Outcome::Handled(jpeg);
                }
                if artwork.is_empty() {
                    return Outcome::Fallback;
                }
                artwork
            }
            None => art,
        };
        self.art_bytes(tv, art).into()
    }
    // Kodi's TCP API (port 9090) sends notifications as they happen
    fn watch(&self, tv: &Tv, changed: &dyn Fn(&str), go: &dyn Fn() -> bool) -> Outcome<()> {
        watch_notifications(tv, changed, go).into()
    }
}

/// Kodi's TCP JSON-RPC port: the same API as over HTTP, plus notifications
/// pushed as they happen (on while remote control from other systems is).
const TCP_PORT: u16 = 9090;

/// The playback notification in a message from Kodi's TCP API, if it's one
/// ("Player.OnPause", ...).
pub fn player_notification(msg: &str) -> Option<String> {
    let v: Value = serde_json::from_str(msg).ok()?;
    str_of(&v, "method").filter(|m| m.starts_with("Player.On"))
}

/// Call `on` with each of Kodi's player notifications until `go` says stop.
fn watch_notifications(tv: &Tv, on: &dyn Fn(&str), go: &dyn Fn() -> bool) -> Result<()> {
    use std::io::{ErrorKind, Read};
    let wait = std::time::Duration::from_secs(2);
    let direct = tv.ip.parse().ok().and_then(|ip| std::net::TcpStream::connect_timeout(&std::net::SocketAddr::new(ip, TCP_PORT), wait).ok());
    // Kodi only listens on the TV itself unless "Allow remote control from
    // applications on other systems" is on: reach it through adb then
    let mut _fwd = None;
    let mut s = match direct {
        Some(s) => s,
        None => {
            let fwd = tv.forward_tcp(TCP_PORT)?;
            let local = std::net::SocketAddr::from(([127, 0, 0, 1], fwd.port));
            let s = std::net::TcpStream::connect_timeout(&local, wait).map_err(|_| off())?;
            _fwd = Some(fwd);
            s
        }
    };
    s.set_read_timeout(Some(std::time::Duration::from_secs(1)))?;
    crate::tlog!("kodi: listening for notifications on port {TCP_PORT}");
    let mut split = crate::ws::JsonSplitter::default();
    let mut buf = [0u8; 8192];
    while go() {
        match s.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                for msg in split.push(&buf[..n]) {
                    if let Some(m) = player_notification(&msg) {
                        on(&m);
                    }
                }
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

impl Kodi {
    /// Kodi's picture `art` (an image:// address), fetched from it.
    #[cfg_attr(not(feature = "ui"), allow(dead_code))]
    fn art_bytes(&self, tv: &Tv, art: &str) -> Result<Vec<u8>> {
        let r = call(tv, "Files.PrepareDownload", &format!(r#"{{"path":{}}}"#, quote(art)))?;
        let Some(path) = r.get("details").and_then(|d| str_of(d, "path")) else { return failed("Kodi didn't offer the picture") };
        let (port, user, pass) = login(tv);
        let url = format!("http://{}:{port}/{}", tv.ip, path.trim_start_matches('/'));
        let mut cmd = std::process::Command::new("curl");
        cmd.args(["-sSf", "--max-time", "10"]);
        if !pass.is_empty() {
            // the login on stdin, not in the process list
            cmd.args(["--config", "-"]).stdin(std::process::Stdio::piped());
        }
        let mut child = cmd.arg(&url).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).spawn()?;
        if !pass.is_empty() {
            if let Some(mut stdin) = child.stdin.take() {
                use std::io::Write;
                let _ = writeln!(stdin, "user = {}", quote(&format!("{user}:{pass}")));
            }
        }
        let out = child.wait_with_output()?;
        if !out.status.success() || out.stdout.is_empty() {
            return failed("Kodi's picture didn't come");
        }
        Ok(out.stdout)
    }
}

// --- details ----------------------------------------------------------------------

const DETAIL_FIELDS: &str = r#"["title","showtitle","season","episode","year","plot","tagline","rating","genre","director","cast","studio","runtime","streamdetails","file"]"#;

/// Names in a JSON list of strings, joined; None if there are none.
fn names(v: &Value, key: &str) -> Option<String> {
    let all: Vec<&str> = v.get(key)?.as_array()?.iter().filter_map(Value::as_str).filter(|s| !s.is_empty()).collect();
    (!all.is_empty()).then(|| all.join(", "))
}

/// `tveee details` from a Kodi item (Player.GetItem's or Files.GetFileDetails').
pub fn parse_details(item: &Value) -> crate::more::Details {
    let mut fields: Vec<(&'static str, String)> = Vec::new();
    let mut add = |k: &'static str, v: Option<String>| {
        if let Some(v) = v {
            fields.push((k, v));
        }
    };
    add("title", str_of(item, "title").or_else(|| str_of(item, "label")));
    let num = |k| num_of(item, k).filter(|n| *n > 0.0);
    add(
        "show",
        str_of(item, "showtitle").map(|show| match (num("season"), num("episode")) {
            (Some(s), Some(e)) => format!("{show}, series {s}, episode {e}"),
            _ => show,
        }),
    );
    add("year", num("year").map(|y| format!("{y}")));
    add("rating", num("rating").map(|r| format!("{r:.1}/10")));
    add("genre", names(item, "genre"));
    add("director", names(item, "director"));
    let cast: Vec<String> = item
        .get("cast")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(6)
        .filter_map(|c| {
            let name = str_of(c, "name")?;
            Some(match str_of(c, "role") {
                Some(role) => format!("{name} ({role})"),
                None => name,
            })
        })
        .collect();
    add("cast", (!cast.is_empty()).then(|| cast.join(", ")));
    add("studio", names(item, "studio"));
    add("length", num("runtime").map(crate::nowplaying::clock));
    if let Some(v) = item.pointer("/streamdetails/video/0") {
        let h = num_of(v, "height").unwrap_or(0.0) as u32;
        let codec = str_of(v, "codec").unwrap_or_default().to_uppercase();
        add("video", (h > 0).then(|| format!("{h}p {codec}").trim().to_string()));
    }
    let audio: Vec<String> = item
        .pointer("/streamdetails/audio")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|a| {
            let lang = str_of(a, "language").unwrap_or_else(|| "?".into());
            let codec = str_of(a, "codec").map(|c| format!(" {}", c.to_uppercase())).unwrap_or_default();
            let ch = num_of(a, "channels").map(|c| format!(" {c}ch")).unwrap_or_default();
            format!("{lang}{codec}{ch}")
        })
        .collect();
    add("audio", (!audio.is_empty()).then(|| audio.join(", ")));
    let subs: Vec<String> =
        item.pointer("/streamdetails/subtitle").and_then(Value::as_array).into_iter().flatten().filter_map(|s| str_of(s, "language")).collect();
    add("subtitles", (!subs.is_empty()).then(|| subs.join(", ")));
    let description = match (str_of(item, "tagline"), str_of(item, "plot")) {
        (Some(t), Some(p)) => Some(format!("{t}\n\n{p}")),
        (t, p) => p.or(t),
    };
    crate::more::Details { fields, description, chapters: Vec::new(), links: Vec::new() }
}

impl crate::provider::Details for Kodi {
    // `link` is the file: what's playing if it's that, else what Kodi knows of the file
    fn details(&self, tv: &Tv, link: &str) -> Result<crate::more::Details> {
        if let Some((id, _)) = player_id(tv)? {
            let r = call(tv, "Player.GetItem", &format!(r#"{{"playerid":{id},"properties":{DETAIL_FIELDS}}}"#))?;
            let item = r.get("item").cloned().unwrap_or_default();
            if str_of(&item, "file").as_deref() == Some(link) {
                return Ok(parse_details(&item));
            }
        }
        let r = call(tv, "Files.GetFileDetails", &format!(r#"{{"file":{},"media":"video","properties":{DETAIL_FIELDS}}}"#, quote(link)))?;
        Ok(parse_details(r.get("filedetails").unwrap_or(&Value::Null)))
    }
}

// --- the app's own actions -------------------------------------------------------

const ACTIONS: &str = "find <words> | play <words> | subtitles [next|off|on] | audio [next] | scan [clean] | notify <text>";

/// `tveee app kodi <action...>`.
fn action(tv: &Tv, args: &[String]) -> Result<String> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let usage = || crate::error::usage(format!("app kodi: one of: {ACTIONS}"));
    match words.as_slice() {
        ["find", q @ ..] if !q.is_empty() => find(tv, &q.join(" ")),
        ["play", q @ ..] if !q.is_empty() => play(tv, &q.join(" ")),
        ["subtitles"] => subtitles(tv, "next"),
        ["subtitles", how @ ("next" | "off" | "on" | "previous")] => subtitles(tv, how),
        ["audio"] | ["audio", "next"] => audio(tv, "next"),
        ["audio", "previous"] => audio(tv, "previous"),
        ["scan"] => call(tv, "VideoLibrary.Scan", "{}").map(|_| "Kodi is looking for new videos (its library scan)".into()),
        ["scan", "clean"] => call(tv, "VideoLibrary.Clean", "{}").map(|_| "Kodi is removing missing videos from its library".into()),
        ["notify", text @ ..] if !text.is_empty() => notify(tv, &text.join(" ")),
        _ => usage(),
    }
}

/// Something in Kodi's library.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    /// "movie" or "tvshow".
    pub kind: &'static str,
    pub id: u64,
    pub title: String,
    pub year: Option<u32>,
}

/// The library's movies and TV shows with `q` in their title.
fn search_library(tv: &Tv, q: &str) -> Result<Vec<Found>> {
    let filter = format!(r#"{{"filter":{{"field":"title","operator":"contains","value":{}}},"properties":["title","year"],"limits":{{"end":25}}}}"#, quote(q));
    let mut out = Vec::new();
    for (kind, method, list, id) in [("movie", "VideoLibrary.GetMovies", "movies", "movieid"), ("tvshow", "VideoLibrary.GetTVShows", "tvshows", "tvshowid")] {
        let r = call(tv, method, &filter)?;
        out.extend(parse_found(&r, kind, list, id));
    }
    Ok(out)
}

pub fn parse_found(r: &Value, kind: &'static str, list: &str, id: &str) -> Vec<Found> {
    r.get(list)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|m| {
            Some(Found {
                kind,
                id: m.get(id)?.as_u64()?,
                title: str_of(m, "title").or_else(|| str_of(m, "label"))?,
                year: num_of(m, "year").filter(|y| *y > 0.0).map(|y| y as u32),
            })
        })
        .collect()
}

fn found_line(f: &Found) -> String {
    let kind = if f.kind == "movie" { "film" } else { "TV show" };
    match f.year {
        Some(y) => format!("{kind:<8} {} ({y})", f.title),
        None => format!("{kind:<8} {}", f.title),
    }
}

fn find(tv: &Tv, q: &str) -> Result<String> {
    let found = search_library(tv, q)?;
    if found.is_empty() {
        return Ok(format!("nothing in Kodi's library called \"{q}\""));
    }
    Ok(found.iter().map(found_line).collect::<Vec<_>>().join("\n"))
}

/// The best match for `q`: the one whose title is `q` (any case), else the
/// only one.
pub fn best_match<'a>(found: &'a [Found], q: &str) -> Option<&'a Found> {
    found.iter().find(|f| f.title.eq_ignore_ascii_case(q)).or(match found {
        [one] => Some(one),
        _ => None,
    })
}

/// Play a film, or open a TV show's episodes, by title.
fn play(tv: &Tv, q: &str) -> Result<String> {
    let found = search_library(tv, q)?;
    let Some(f) = best_match(&found, q) else {
        return match found.len() {
            0 => failed(format!("nothing in Kodi's library called \"{q}\"")),
            n => failed(format!("{n} things in Kodi's library match \"{q}\"; say which:\n{}", found.iter().map(found_line).collect::<Vec<_>>().join("\n"))),
        };
    };
    let _ = crate::plugins::apps::bring_to_front(tv, PACKAGE);
    if f.kind == "movie" {
        call(tv, "Player.Open", &format!(r#"{{"item":{{"movieid":{}}}}}"#, f.id))?;
        Ok(format!("playing {}", f.title))
    } else {
        let link = format!("kodi-window:videos|videodb://tvshows/titles/{}/", f.id);
        call(tv, "GUI.ActivateWindow", &window_params(&link).unwrap_or_default())?;
        Ok(format!("opened {} in Kodi", f.title))
    }
}

/// The playing video's player id, or a failure saying nothing is playing.
fn video_player(tv: &Tv) -> Result<u32> {
    match player_id(tv)? {
        Some((id, kind)) if kind == "video" => Ok(id),
        _ => failed("Kodi isn't playing a video"),
    }
}

fn subtitles(tv: &Tv, how: &str) -> Result<String> {
    let id = video_player(tv)?;
    let params = match how {
        "on" | "off" => format!(r#"{{"playerid":{id},"subtitle":"{how}"}}"#),
        _ => format!(r#"{{"playerid":{id},"subtitle":"{how}","enable":true}}"#),
    };
    call(tv, "Player.SetSubtitle", &params)?;
    let props = call(tv, "Player.GetProperties", &format!(r#"{{"playerid":{id},"properties":["currentsubtitle","subtitleenabled"]}}"#))?;
    Ok(match props.get("subtitleenabled").and_then(Value::as_bool) {
        Some(true) => format!("subtitles: {}", track_name(props.get("currentsubtitle")).unwrap_or_else(|| "on".into())),
        _ => "subtitles off".into(),
    })
}

fn audio(tv: &Tv, how: &str) -> Result<String> {
    let id = video_player(tv)?;
    call(tv, "Player.SetAudioStream", &format!(r#"{{"playerid":{id},"stream":"{how}"}}"#))?;
    let props = call(tv, "Player.GetProperties", &format!(r#"{{"playerid":{id},"properties":["currentaudiostream"]}}"#))?;
    Ok(format!("audio: {}", track_name(props.get("currentaudiostream")).unwrap_or_else(|| "changed".into())))
}

/// "English AC3", from a stream Kodi describes.
fn track_name(t: Option<&Value>) -> Option<String> {
    let t = t?;
    let name = str_of(t, "name").or_else(|| str_of(t, "language"))?;
    Some(match str_of(t, "codec") {
        Some(c) => format!("{name} {}", c.to_uppercase()),
        None => name,
    })
}

fn notify(tv: &Tv, text: &str) -> Result<String> {
    call(tv, "GUI.ShowNotification", &format!(r#"{{"title":"tveee","message":{}}}"#, quote(text)))?;
    Ok("shown in Kodi".into())
}

/// What it can do (for now, the ui menu's items).
pub fn actions() -> Vec<crate::actions::ActionDescriptor> {
    use crate::actions::ActionDescriptor as A;
    vec![
        A::new("kodi.subtitles.next", "kodi", "kodi: next subtitles", |cx, _| subtitles(cx.tv, "next").map(Into::into)).key('k'),
        A::new("kodi.subtitles.off", "kodi", "kodi: subtitles off", |cx, _| subtitles(cx.tv, "off").map(Into::into)).key('K'),
        A::new("kodi.audio.next", "kodi", "kodi: next audio track", |cx, _| audio(cx.tv, "next").map(Into::into)).key('a'),
        A::new("kodi.scan", "kodi", "kodi: look for new videos (library scan)", |cx, _| {
            call(cx.tv, "VideoLibrary.Scan", "{}").map(|_| "Kodi is scanning its library".into())
        })
        .key('L'),
        A::new("kodi.find", "kodi", "kodi: find in the library", |cx, q| {
            // one line, for the status line
            find(cx.tv, &q.join(" ")).map(|s| s.replace('\n', "; ").into())
        })
        .key('F')
        .text("Find in Kodi:", "a film or show's title"),
    ]
}

pub fn providers() -> Vec<&'static dyn crate::provider::Provider> {
    vec![&KODI]
}

// --- pairing ---------------------------------------------------------------------

pub struct KodiLogin;
pub static KODI_LOGIN: KodiLogin = KodiLogin;

fn ask(prompt: &str, default: &str) -> Result<String> {
    use std::io::{BufRead, Write};
    print!("{prompt}{}: ", if default.is_empty() { String::new() } else { format!(" [{default}]") });
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    let line = line.trim();
    Ok(if line.is_empty() { default.to_string() } else { line.to_string() })
}

impl crate::pairing::Pairing for KodiLogin {
    fn id(&self) -> &'static str {
        PAIRING
    }
    fn name(&self) -> &'static str {
        "Kodi's remote control (JSON-RPC)"
    }
    fn unlocks(&self) -> &'static str {
        "Kodi's position and progress, exact seeking, artwork, places, restart where it was"
    }
    fn how(&self) -> &'static str {
        "type the port, username and password set in Kodi (Settings > Services > Control); only needed if it has a password"
    }
    fn pair(&self, tv: &Tv) -> Result<crate::pairing::Saved> {
        use std::io::IsTerminal;
        if !std::io::stdin().is_terminal() {
            return crate::error::usage("pair kodi: run it in a terminal to type Kodi's login");
        }
        println!("In Kodi: Settings > Services > Control > Allow remote control via HTTP (on).");
        let port = ask("Port", &DEFAULT_PORT.to_string())?;
        let user = ask("Username", DEFAULT_USER)?;
        let pass = ask("Password (empty if none)", "")?;
        let Ok(port_n) = port.parse::<u16>() else { return crate::error::usage(format!("pair kodi: '{port}' isn't a port")) };
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"JSONRPC.Ping"}"#;
        let reply = post(tv, &(port_n, user.clone(), pass.clone()), body)?;
        if !reply.contains("pong") {
            return failed(format!("pair kodi: Kodi answered oddly: {}", reply.trim()));
        }
        Ok(crate::pairing::Saved(vec![("port".into(), port), ("user".into(), user), ("pass".into(), pass)]))
    }
    fn check(&self, tv: &Tv, _saved: &crate::pairing::Saved) -> Option<String> {
        Some(match now(tv) {
            Ok(Some((p, _))) => format!("check: Kodi is playing {}", p.title.unwrap_or_default()),
            Ok(None) => "check: Kodi answers (nothing playing)".into(),
            Err(e) => format!("check failed: {e}"),
        })
    }
}

pub fn pairings() -> Vec<&'static dyn crate::pairing::Pairing> {
    vec![&KODI_LOGIN]
}

/// Nothing of its own: `tveee app kodi`, `goto kodi`, `seek`, ... cover it.
pub fn commands() -> Vec<Command> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    // as Kodi 21 answers
    const ITEM: &str = r#"{"item":{"art":{"fanart":"image://smb%3a%2f%2fnas%2ffanart.jpg/","poster":"image://p.jpg/"},"episode":4,"fanart":"","file":"smb://nas/tv/Taskmaster/S03E04.mkv","id":7,"label":"S03E04","season":3,"showtitle":"Taskmaster","streamdetails":{"audio":[{"channels":6,"codec":"ac3","language":"eng"}],"subtitle":[{"language":"eng"}],"video":[{"aspect":1.78,"codec":"h264","duration":2700,"height":1080,"width":1920}]},"thumbnail":"","title":"A Very Nuanced Character","type":"episode","year":2016}}"#;
    const PROPS: &str = r#"{"currentaudiostream":{"bitrate":0,"channels":6,"codec":"ac3","index":0,"language":"eng","name":"English"},"currentsubtitle":{"index":0,"language":"eng","name":"English"},"speed":1,"subtitleenabled":false,"time":{"hours":0,"milliseconds":500,"minutes":3,"seconds":20},"totaltime":{"hours":0,"milliseconds":0,"minutes":45,"seconds":0}}"#;

    #[test]
    fn details_of_an_episode() {
        let item = json(r#"{"title":"A Very Nuanced Character","showtitle":"Taskmaster","season":3,"episode":4,"year":2016,"rating":7.84,"genre":["Comedy"],"director":[],"cast":[{"name":"Greg Davies","role":"Taskmaster"},{"name":"Alex Horne"}],"runtime":2700,"plot":"Five comedians.","tagline":"","streamdetails":{"audio":[{"channels":6,"codec":"ac3","language":"eng"},{"channels":2,"codec":"aac","language":"fre"}],"subtitle":[{"language":"eng"}],"video":[{"codec":"h264","height":1080}]}}"#);
        let d = parse_details(&item);
        let f = |k| d.fields.iter().find(|(n, _)| *n == k).map(|(_, v)| v.as_str());
        assert_eq!(f("show"), Some("Taskmaster, series 3, episode 4"));
        assert_eq!(f("rating"), Some("7.8/10"));
        assert_eq!(f("cast"), Some("Greg Davies (Taskmaster), Alex Horne"));
        assert_eq!(f("director"), None);
        assert_eq!(f("length"), Some("45:00"));
        assert_eq!(f("video"), Some("1080p H264"));
        assert_eq!(f("audio"), Some("eng AC3 6ch, fre AAC 2ch"));
        assert_eq!(f("subtitles"), Some("eng"));
        assert_eq!(d.description.as_deref(), Some("Five comedians."));
    }

    #[test]
    fn local_copies() {
        let map = "smb://nas/media=/mnt/nas/media; nfs://box/films/ = /srv/films";
        assert_eq!(local_path("smb://nas/media/TV/A%20B.mkv", map), Some("/mnt/nas/media/TV/A B.mkv".into()));
        assert_eq!(local_path("nfs://box/films/x.mkv", map), Some("/srv/films/x.mkv".into()));
        assert_eq!(local_path("smb://nas/media2/x.mkv", map), None);
        assert_eq!(local_path("smb://nas/media/x.mkv", ""), None);
        assert!(has_login("smb://me:secret@nas/x.mkv") && !has_login("smb://nas/a@b.mkv") && !has_login("/storage/x.mkv"));
    }

    #[test]
    fn library_search() {
        let r = json(r#"{"limits":{"end":2,"start":0,"total":2},"movies":[{"label":"Dune","movieid":3,"title":"Dune","year":2021},{"label":"Dune: Part Two","movieid":9,"title":"Dune: Part Two","year":2024}]}"#);
        let found = parse_found(&r, "movie", "movies", "movieid");
        assert_eq!(found.len(), 2);
        assert_eq!(found[0], Found { kind: "movie", id: 3, title: "Dune".into(), year: Some(2021) });
        assert_eq!(best_match(&found, "dune").map(|f| f.id), Some(3));
        assert_eq!(best_match(&found, "part"), None);
        assert_eq!(best_match(&found[1..], "part").map(|f| f.id), Some(9));
        assert!(parse_found(&json(r#"{"limits":{"total":0}}"#), "movie", "movies", "movieid").is_empty());
    }

    #[test]
    fn notifications() {
        let pause = r#"{"jsonrpc":"2.0","method":"Player.OnPause","params":{"data":{"item":{"id":7,"type":"episode"},"player":{"playerid":1,"speed":0}},"sender":"xbmc"}}"#;
        assert_eq!(player_notification(pause).as_deref(), Some("Player.OnPause"));
        assert_eq!(player_notification(r#"{"jsonrpc":"2.0","method":"GUI.OnScreensaverActivated","params":{}}"#), None);
        assert_eq!(player_notification(r#"{"jsonrpc":"2.0","id":1,"result":"pong"}"#), None);
    }

    fn json(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn reads_what_is_playing() {
        let p = parse_playing(&json(ITEM), &json(PROPS));
        assert_eq!(p.title.as_deref(), Some("A Very Nuanced Character"));
        assert_eq!(p.subtitle.as_deref(), Some("Taskmaster S03E04"));
        let pr = p.progress.unwrap();
        assert_eq!((pr.position, pr.duration, pr.paused), (200.5, Some(2700.0), false));
        assert_eq!(p.art.as_deref(), Some("image://smb%3a%2f%2fnas%2ffanart.jpg/"));
        assert_eq!(p.extra, vec![("video", "1080p H264".to_string()), ("audio", "English AC3".to_string())]);
        // paused, subtitles on, a film
        let props = PROPS.replace(r#""speed":1"#, r#""speed":0"#).replace(r#""subtitleenabled":false"#, r#""subtitleenabled":true"#);
        let film = r#"{"item":{"title":"Heat","year":1995,"art":{},"file":"/sdcard/heat.mkv","season":-1,"episode":-1,"showtitle":""}}"#;
        let p = parse_playing(&json(film), &json(&props));
        assert_eq!((p.subtitle.as_deref(), p.progress.unwrap().paused, p.art), (Some("1995"), true, None));
        assert!(p.extra.iter().any(|(k, v)| *k == "subtitles" && v == "English"));
    }

    #[test]
    fn replies() {
        let ok = r#"{"result":{"a":"x\"}","n":-3.5},"id":1}"#;
        assert_eq!(result_of(ok, "m").unwrap(), json(r#"{"a":"x\"}","n":-3.5}"#));
        let err = r#"{"error":{"code":-32601,"message":"Method not found."},"id":1}"#;
        assert!(result_of(err, "X.Y").unwrap_err().to_string().contains("Method not found"));
        assert!(result_of("<html>", "X.Y").is_err());
    }

    #[test]
    fn times_and_seeks() {
        assert_eq!(seconds(&json(r#"{"hours":1,"milliseconds":250,"minutes":2,"seconds":3}"#)), Some(3723.25));
        assert_eq!(time_json(3723.25), r#"{"hours":1,"minutes":2,"seconds":3,"milliseconds":250}"#);
        assert_eq!(seek_value(SeekTo::By(-10.0)), r#"{"seconds":-10}"#);
        assert_eq!(seek_value(SeekTo::Fraction(0.4)), r#"{"percentage":40.000}"#);
        assert!(seek_value(SeekTo::At(90.0)).contains(r#""minutes":1,"seconds":30"#));
    }

    #[test]
    fn places_are_windows() {
        assert_eq!(window_params("kodi-window:home").as_deref(), Some(r#"{"window":"home"}"#));
        assert_eq!(
            window_params("kodi-window:videos|videodb://movies/titles/").as_deref(),
            Some(r#"{"window":"videos","parameters":["videodb://movies/titles/"]}"#)
        );
        assert_eq!(window_params("https://x"), None);
    }
}

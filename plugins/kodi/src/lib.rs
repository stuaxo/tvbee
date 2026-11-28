#![warn(missing_docs)]
//! Kodi, through its own remote-control API (JSON-RPC over HTTP, port 8080
//! by default). It's off until switched on in Kodi: Settings > Services >
//! Control > "Allow remote control via HTTP". With it, tveee knows what Kodi
//! is playing and exactly where (the media session only gives a title),
//! seeks exactly, shows the artwork in the ui's preview (Kodi's video is on
//! a layer pictures can't see), goes to places in Kodi, plays links in it,
//! and restarts it back where it was.
//!
//! No password set in Kodi: nothing to do. With one: `tveee pair kodi`.

use tveee_core::actions::ActionDescriptor;
use tveee_core::devtools::{Progress, SeekTo};
use tveee_core::error::{Error, Result, failed};
use tveee_core::more::Place;
use tveee_core::provider::{Outcome, Playing, Session};
use tveee_core::tv::Tv;

/// Kodi's Android package.
pub const PACKAGE: &str = "org.xbmc.kodi";

pub mod addons;
pub mod browse;
pub mod library;
pub mod link;
pub mod rpc;
pub mod sideload;
const PAIRING: &str = "kodi";
const DEFAULT_PORT: u16 = 8080;
const DEFAULT_USER: &str = "kodi";

/// Whether the TV has Kodi.
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
pub fn time_json(secs: f64) -> Value {
    let ms = (secs.max(0.0) * 1000.0).round() as u64;
    serde_json::json!({ "hours": ms / 3_600_000, "minutes": ms / 60_000 % 60, "seconds": ms / 1000 % 60, "milliseconds": ms % 1000 })
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
    match tveee_core::pairing::load(PAIRING, &tv.ip) {
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
        "Kodi isn't answering: it isn't running, or its remote control is off (in Kodi: Settings > Services > Control > Allow remote control via HTTP)".into(),
    )
}

/// POST `body` to Kodi's JSON-RPC; returns the reply.
fn post(tv: &Tv, login: &(u16, String, String), body: &str) -> Result<String> {
    post_within(tv, login, body, 4)
}

/// [`post`], waiting up to `secs` for the answer. No answer: Kodi is off,
/// or (it answers a ping) just slow, which is said as such.
fn post_within(tv: &Tv, login: &(u16, String, String), body: &str, secs: u32) -> Result<String> {
    let (port, user, pass) = login;
    let url = format!("http://{}:{port}/jsonrpc", tv.ip);
    let auth = (!pass.is_empty()).then_some((user.as_str(), pass.as_str()));
    let reply =
        tveee_core::https::request(&tveee_core::https::Request { url: &url, body: Some(body), digest: None, basic: auth, retry: None, timeout_secs: secs });
    match reply {
        Ok((200, text)) => Ok(text),
        Ok((401, _)) => failed("Kodi wants a username and password: run 'tveee pair kodi'"),
        Ok((code, _)) => failed(format!("Kodi answered HTTP {code}")),
        Err(_) if !body.contains("JSONRPC.Ping") && ping(tv) => {
            failed(format!("Kodi didn't answer within {secs}s (it's running: a slow share or a big library?)"))
        }
        Err(_) => Err(off()),
    }
}

/// Call `method` with `params` (a JSON object); returns its `result`
/// (through [`rpc`], as every call to Kodi goes).
pub fn call(tv: &Tv, method: &str, params: &str) -> Result<Value> {
    let Ok(params) = serde_json::from_str::<Value>(params) else { return failed(format!("Kodi: {method}: params aren't JSON")) };
    rpc::ask(tv, &rpc::Raw { method, params })
}

pub use rpc::result_of;

/// Call `method` with `params` built as JSON (`json!`); returns its `result`.
pub fn call_json(tv: &Tv, method: &str, params: Value) -> Result<Value> {
    rpc::ask(tv, &rpc::Raw { method, params })
}

/// Whether Kodi's API answers now.
pub fn ping(tv: &Tv) -> bool {
    call(tv, "JSONRPC.Ping", "{}").is_ok_and(|r| r == "pong")
}

/// `tveee diagnose`: is its remote control on?
pub fn diagnose(tv: &Tv) -> Vec<tveee_core::plugin::Finding> {
    let (ok, detail) = if ping(tv) {
        (Some(true), "its remote control answers (position, exact seeking, artwork, places)")
    } else {
        (
            None,
            "installed, but its remote control doesn't answer (Kodi not running, or off: Settings > Services > Control > Allow remote control via HTTP; with a password, 'tveee pair kodi')",
        )
    };
    vec![tveee_core::plugin::Finding { ok, what: "kodi", detail: detail.into() }]
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

const ITEM_FIELDS: &[&str] = &["title", "showtitle", "season", "episode", "artist", "album", "year", "file", "art", "thumbnail", "fanart", "streamdetails"];
const PLAYER_FIELDS: &[&str] = &["time", "totaltime", "speed", "currentaudiostream", "currentsubtitle", "subtitleenabled"];

/// What kind of thing a Kodi player plays, from its type.
pub fn media_of(kind: &str) -> Option<&'static str> {
    match kind {
        "video" => Some("video"),
        "audio" => Some("audio"),
        "picture" => Some("picture"),
        _ => None,
    }
}

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
    // 0 paused, 1 playing, 2 4 8... fast-forwarding, -2... rewinding
    let speed = num_of(props, "speed").unwrap_or(1.0);
    let paused = speed == 0.0;
    let progress = time.map(|position| Progress { position, duration: total, paused, rate: speed });
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
    Playing { title, subtitle, progress, link, art, art_moves: false, extra, idle: false, media: None }
}

/// What Kodi is playing, its file (for sessions), and the player's
/// properties (its tracks, for sessions).
fn now(tv: &Tv) -> Result<Option<(Playing, Option<String>, Value)>> {
    let Some((id, kind)) = player_id(tv)? else { return Ok(None) };
    let item = call_json(tv, "Player.GetItem", serde_json::json!({ "playerid": id, "properties": ITEM_FIELDS }))?;
    let props = call_json(tv, "Player.GetProperties", serde_json::json!({ "playerid": id, "properties": PLAYER_FIELDS }))?;
    let file = item.get("item").and_then(|i| str_of(i, "file"));
    let mut playing = parse_playing(&item, &props);
    playing.media = media_of(&kind);
    // the file is the link (`tveee details`), unless it has a login in it:
    // it's shown
    playing.link = file.clone().filter(|f| !has_login(f));
    // a copy this computer can read: frames of it for the preview, not the artwork
    if let Some(local) = file.as_deref().and_then(|f| local_path(f, &std::env::var(PATHS_VAR).unwrap_or_default())).filter(|p| p.is_file()) {
        let art = playing.art.take().unwrap_or_default();
        playing.art = Some(format!("{FRAME}{}\n{art}", local.display()));
        playing.art_moves = true;
    }
    Ok(Some((playing, file, props)))
}

/// The audio and subtitle tracks chosen in Kodi's player: kept in a
/// session's `extra` and put back after a restart.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Tracks {
    /// The audio stream chosen (Kodi's index).
    pub audio: Option<u64>,
    /// The subtitle chosen (Kodi's index).
    pub subtitle: Option<u64>,
    /// Whether subtitles are showing.
    pub subtitles_on: bool,
}

impl Tracks {
    /// From Player.GetProperties' result (`currentaudiostream`,
    /// `currentsubtitle`, `subtitleenabled`).
    pub fn of(props: &Value) -> Tracks {
        let index = |k: &str| props.get(k)?.get("index")?.as_u64();
        Tracks {
            audio: index("currentaudiostream"),
            subtitle: index("currentsubtitle"),
            subtitles_on: props.get("subtitleenabled").and_then(Value::as_bool).unwrap_or(false),
        }
    }

    /// As a session keeps them.
    pub fn to_extra(&self) -> String {
        serde_json::json!({ "audio": self.audio, "subtitle": self.subtitle, "subtitles_on": self.subtitles_on }).to_string()
    }

    /// Back from a session (none kept: nothing to put back).
    pub fn from_extra(extra: &str) -> Option<Tracks> {
        let v: Value = serde_json::from_str(extra).ok()?;
        Some(Tracks {
            audio: v.get("audio")?.as_u64(),
            subtitle: v.get("subtitle").and_then(Value::as_u64),
            subtitles_on: v.get("subtitles_on").and_then(Value::as_bool).unwrap_or(false),
        })
    }

    /// The calls that put them back on player `id`.
    pub fn calls(&self, id: u32) -> Vec<(&'static str, String)> {
        let mut calls = Vec::new();
        if let Some(a) = self.audio {
            calls.push(("Player.SetAudioStream", format!(r#"{{"playerid":{id},"stream":{a}}}"#)));
        }
        match (self.subtitles_on, self.subtitle) {
            (true, Some(s)) => calls.push(("Player.SetSubtitle", format!(r#"{{"playerid":{id},"subtitle":{s},"enable":true}}"#))),
            _ => calls.push(("Player.SetSubtitle", format!(r#"{{"playerid":{id},"subtitle":"off"}}"#))),
        }
        calls
    }
}

/// What a session says about the tracks: ", English subtitles" when they're
/// on, and the audio when it isn't the first track.
pub fn tracks_said(props: &Value) -> String {
    let t = Tracks::of(props);
    let mut said = String::new();
    if t.audio.is_some_and(|a| a > 0)
        && let Some(a) = track_name(props.get("currentaudiostream"))
    {
        said += &format!(", {a} audio");
    }
    if t.subtitles_on {
        said += &format!(", {} subtitles", track_name(props.get("currentsubtitle")).unwrap_or_else(|| "with".into()));
    }
    said
}

/// After Player.Open: wait for the video to start (up to `secs`), bring it
/// to the front, and put the tracks back. Best effort: the video plays
/// either way.
///
/// Opened just after Kodi starts, the video plays behind the home screen
/// with Kodi's busy spinner left over it (seen live): GUI.SetFullscreen
/// brings the video up, and the spinner goes.
fn put_back(tv: &Tv, tracks: Option<&Tracks>, secs: u64) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    let id = loop {
        if let Ok(Some((id, kind))) = player_id(tv)
            && kind == "video"
        {
            break id;
        }
        if std::time::Instant::now() >= deadline {
            tveee_core::tlog!("kodi: the video didn't start; not brought to the front, tracks not put back");
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    };
    if let Err(e) = call(tv, "GUI.SetFullscreen", r#"{"fullscreen":true}"#) {
        tveee_core::tlog!("kodi: GUI.SetFullscreen: {e}");
    }
    for (method, params) in tracks.map(|t| t.calls(id)).unwrap_or_default() {
        if let Err(e) = call(tv, method, &params) {
            tveee_core::tlog!("kodi: {method}: {e}");
        }
    }
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
pub fn seek_value(to: SeekTo) -> Value {
    match to {
        SeekTo::By(d) => serde_json::json!({ "seconds": d.round() as i64 }),
        // to a thousandth of a percent, as before
        SeekTo::Fraction(f) => serde_json::json!({ "percentage": ((f * 100.0).clamp(0.0, 100.0) * 1000.0).round() / 1000.0 }),
        SeekTo::At(t) => serde_json::json!({ "time": time_json(t) }),
    }
}

// --- the provider --------------------------------------------------------------

/// Kodi's provider: what's playing, exact seeking, its library, lists,
/// pictures, next and previous (through its JSON-RPC API).
pub struct Kodi;
/// The one Kodi provider.
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
pub fn window_params(link: &str) -> Option<Value> {
    match link::Link::parse(link)? {
        link::Link::Window { window, path: Some(p) } => Some(serde_json::json!({ "window": window, "parameters": [p] })),
        link::Link::Window { window, path: None } => Some(serde_json::json!({ "window": window })),
        _ => None,
    }
}

/// Kodi in front, and its API answering (starting Kodi if need be).
fn bring_up(tv: &Tv) -> Result<()> {
    let _ = tveee_core::builtin::apps::bring_to_front(tv, PACKAGE);
    wait_ready(tv, 40)
}

impl tveee_core::provider::Provider for Kodi {
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
    fn player(&self) -> Option<&dyn tveee_core::provider::Player> {
        Some(self)
    }
    fn details(&self) -> Option<&dyn tveee_core::provider::Details> {
        Some(self)
    }
    fn has_sessions(&self) -> bool {
        true
    }
    // what's playing and how far in: restarting comes back to it
    fn save_session(&self, tv: &Tv) -> Outcome<Session> {
        let saved = now(tv).map(|n| {
            let (playing, file, props) = n?;
            let at = playing.progress.map(|p| p.position.floor())?;
            Some(Session {
                link: file?,
                said: format!("at {} in the same file{}", tveee_core::nowplaying::clock(at), tracks_said(&props)),
                resume: Some(at),
                extra: Tracks::of(&props).to_extra(),
            })
        });
        match Outcome::from(saved) {
            Outcome::Handled(Some(s)) => Outcome::Handled(s),
            Outcome::Handled(None) | Outcome::Fallback => Outcome::Fallback,
            Outcome::Failed(e) => Outcome::Failed(e),
        }
    }
    fn restore_session(&self, tv: &Tv, target: &str, s: &Session) -> Outcome<()> {
        let restored = (|| {
            tveee_core::builtin::apps::start(tv, target)?;
            wait_ready(tv, 40)?;
            let mut params = serde_json::json!({ "item": { "file": s.link } });
            if let Some(t) = s.resume {
                params["options"] = serde_json::json!({ "resume": time_json(t) });
            }
            call_json(tv, "Player.Open", params)?;
            put_back(tv, Tracks::from_extra(&s.extra).as_ref(), 20);
            Ok(())
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
            return match bring_up(tv).and_then(|()| call_json(tv, "GUI.ActivateWindow", params)) {
                Ok(_) => Outcome::Handled(()),
                Err(e) => Outcome::Failed(Error::Failed(e.to_string())),
            };
        }
        // its remote control off (or Kodi not running): the usual way, an intent
        if !ping(tv) {
            return Outcome::Fallback;
        }
        let played = call_json(tv, "Player.Open", serde_json::json!({ "item": { "file": link } }));
        if played.is_ok() {
            let _ = tveee_core::builtin::apps::bring_to_front(tv, PACKAGE);
        }
        played.map(drop).into()
    }
}

impl tveee_core::provider::Player for Kodi {
    fn has_visualiser(&self) -> bool {
        true
    }
    fn visualiser(&self, tv: &Tv, want: Option<bool>) -> Outcome<bool> {
        match visualiser(tv, want) {
            Err(Error::Unavailable(_)) => Outcome::Fallback,
            r => r.into(),
        }
    }
    fn has_list(&self) -> bool {
        true
    }
    fn list_place(&self, tv: &Tv) -> Outcome<Option<tveee_core::provider::ListPlace>> {
        match browse::place_now(tv) {
            Ok(p) => Outcome::Handled(p),
            Err(Error::Unavailable(_)) => Outcome::Fallback,
            Err(e) => Outcome::Failed(e),
        }
    }
    fn list(&self, tv: &Tv) -> Outcome<tveee_core::provider::ContextList> {
        // browsed here, from where Kodi is (its library's top if elsewhere)
        match library::start(tv) {
            Ok(l) => Outcome::Handled(l),
            // its API is off: nothing here
            Err(Error::Unavailable(_)) => Outcome::Fallback,
            Err(e) => Outcome::Failed(e),
        }
    }
    fn list_at(&self, tv: &Tv, at: &str) -> Outcome<tveee_core::provider::ContextList> {
        match library::list_at(tv, at) {
            Err(Error::Unavailable(_)) => Outcome::Fallback,
            r => r.into(),
        }
    }
    fn show_at(&self, tv: &Tv, at: &str) -> Outcome<()> {
        use link::Link;
        let window = match Link::parse(at) {
            // Kodi's top: its home screen
            Some(Link::Root) => Link::window("home", None),
            _ => {
                let (m, _, path) = library::parse(at);
                Link::window(m.window_name(), Some(&path))
            }
        };
        tveee_core::provider::Provider::open_here(self, tv, &window.to_string())
    }
    fn go(&self, tv: &Tv, to: &tveee_core::more::Go) -> Outcome<String> {
        use tveee_core::provider::Provider;
        let tveee_core::more::Go::Link(link) = to else { return Outcome::Fallback };
        match link::Link::parse(link) {
            Some(link::Link::Play { .. }) => return library::play(tv, link).into(),
            // an audio stream or subtitle of the video playing
            Some(link::Link::Track { audio, index }) => return switch_track(tv, audio, index).into(),
            // an item of the queue playing: play from there
            Some(link::Link::GoTo { position }) => {
                let done = player_id(tv).and_then(|p| {
                    let Some((id, _)) = p else { return Err(Error::Unavailable("Kodi isn't playing anything".into())) };
                    call_json(tv, "Player.GoTo", serde_json::json!({ "playerid": id, "to": position }))
                        .map(|_| format!("playing item {} of the queue", position + 1))
                });
                return done.into();
            }
            _ => {}
        }
        // a folder (from the command line's list): shown on Kodi's screen
        if library::is_link(link) {
            return match tveee_core::provider::Player::show_at(self, tv, link) {
                Outcome::Handled(()) => Outcome::Handled("opened it in Kodi".into()),
                Outcome::Fallback => Outcome::Fallback,
                Outcome::Failed(e) => Outcome::Failed(e),
            };
        }
        match self.open_here(tv, link) {
            Outcome::Handled(()) if matches!(link::Link::parse(link), Some(link::Link::Window { .. })) => Outcome::Handled("opened it in Kodi".into()),
            Outcome::Handled(()) => Outcome::Handled("playing it in Kodi".into()),
            Outcome::Fallback => Outcome::Fallback,
            Outcome::Failed(e) => Outcome::Failed(e),
        }
    }
    fn playing(&self, tv: &Tv) -> Outcome<Playing> {
        match Outcome::from(now(tv)) {
            Outcome::Handled(Some((p, _, _))) => Outcome::Handled(p),
            // its API answers: nothing is playing, whatever the media session says
            Outcome::Handled(None) => Outcome::Handled(Playing { idle: true, ..Playing::default() }),
            Outcome::Fallback => Outcome::Fallback,
            Outcome::Failed(e) => Outcome::Failed(e),
        }
    }
    fn seek(&self, tv: &Tv, to: SeekTo) -> Outcome<f64> {
        let seeked = player_id(tv).and_then(|p| {
            // nothing playing: keys won't do better, but they can't hurt
            let Some((id, _)) = p else { return Err(Error::Unavailable("Kodi isn't playing anything".into())) };
            let r = call_json(tv, "Player.Seek", serde_json::json!({ "playerid": id, "value": seek_value(to) }))?;
            r.get("time").and_then(seconds).map_or_else(|| failed("Kodi didn't say where it seeked to"), Ok)
        });
        seeked.into()
    }
    fn skip_item(&self, tv: &Tv, forward: bool) -> Outcome<String> {
        let done = player_id(tv).and_then(|p| {
            let Some((id, _)) = p else { return Err(Error::Unavailable("Kodi isn't playing anything".into())) };
            let to = if forward { "next" } else { "previous" };
            call_json(tv, "Player.GoTo", serde_json::json!({ "playerid": id, "to": to })).map(|_| format!("{to} in Kodi"))
        });
        match done {
            // nothing playing: the media keys can't do better, but can't hurt
            Err(Error::Unavailable(_)) => Outcome::Fallback,
            r => r.into(),
        }
    }
    fn play_pause(&self, tv: &Tv) -> Outcome<()> {
        let done = player_id(tv).and_then(|p| {
            let Some((id, _)) = p else { return Err(Error::Unavailable("Kodi isn't playing anything".into())) };
            call_json(tv, "Player.PlayPause", serde_json::json!({ "playerid": id })).map(drop)
        });
        done.into()
    }
    fn picture(&self, tv: &Tv, art: &str, at: Option<f64>) -> Outcome<Vec<u8>> {
        let art = match art.strip_prefix(FRAME).and_then(|a| a.split_once('\n')) {
            Some((path, artwork)) => {
                if let Some(jpeg) = tveee_core::frames::frame_of(std::path::Path::new(path), at.unwrap_or(0.0)) {
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
    tveee_core::tlog!("kodi: listening for notifications on port {TCP_PORT}");
    let mut split = tveee_core::ws::JsonSplitter::default();
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
    fn art_bytes(&self, tv: &Tv, art: &str) -> Result<Vec<u8>> {
        let r = call_json(tv, "Files.PrepareDownload", serde_json::json!({ "path": art }))?;
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
        if !pass.is_empty()
            && let Some(mut stdin) = child.stdin.take()
        {
            use std::io::Write;
            let _ = writeln!(stdin, "user = {}", quote(&format!("{user}:{pass}")));
        }
        let out = child.wait_with_output()?;
        if !out.status.success() || out.stdout.is_empty() {
            return failed("Kodi's picture didn't come");
        }
        Ok(out.stdout)
    }
}

// --- details ----------------------------------------------------------------------

const DETAIL_FIELDS: &[&str] = &[
    "title",
    "showtitle",
    "season",
    "episode",
    "year",
    "plot",
    "tagline",
    "rating",
    "genre",
    "director",
    "cast",
    "studio",
    "runtime",
    "streamdetails",
    "file",
];

/// Names in a JSON list of strings, joined; None if there are none.
fn names(v: &Value, key: &str) -> Option<String> {
    let all: Vec<&str> = v.get(key)?.as_array()?.iter().filter_map(Value::as_str).filter(|s| !s.is_empty()).collect();
    (!all.is_empty()).then(|| all.join(", "))
}

/// `tveee details` from a Kodi item (Player.GetItem's or Files.GetFileDetails').
pub fn parse_details(item: &Value) -> tveee_core::more::Details {
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
    add("length", num("runtime").map(tveee_core::nowplaying::clock));
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
    tveee_core::more::Details { fields, description, chapters: Vec::new(), links: Vec::new() }
}

impl tveee_core::provider::Details for Kodi {
    // `link` is the file: what's playing if it's that, else what Kodi knows of the file
    fn details(&self, tv: &Tv, link: &str) -> Result<tveee_core::more::Details> {
        if let Some((id, _)) = player_id(tv)? {
            let r = call_json(tv, "Player.GetItem", serde_json::json!({ "playerid": id, "properties": DETAIL_FIELDS }))?;
            let item = r.get("item").cloned().unwrap_or_default();
            if str_of(&item, "file").as_deref() == Some(link) {
                return Ok(parse_details(&item));
            }
        }
        let r = call_json(tv, "Files.GetFileDetails", serde_json::json!({ "file": link, "media": "video", "properties": DETAIL_FIELDS }))?;
        Ok(parse_details(r.get("filedetails").unwrap_or(&Value::Null)))
    }
}

// --- the app's own actions -------------------------------------------------------

/// Something in Kodi's library.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    /// "movie" or "tvshow".
    pub kind: &'static str,
    /// Its id in the library (movieid, tvshowid).
    pub id: u64,
    /// Its title.
    pub title: String,
    /// Its year, when the library has one.
    pub year: Option<u32>,
}

/// The library's movies and TV shows with `q` in their title.
fn search_library(tv: &Tv, q: &str) -> Result<Vec<Found>> {
    let filter =
        serde_json::json!({ "filter": { "field": "title", "operator": "contains", "value": q }, "properties": ["title", "year"], "limits": { "end": 25 } });
    let mut out = Vec::new();
    for (kind, method, list, id) in [("movie", "VideoLibrary.GetMovies", "movies", "movieid"), ("tvshow", "VideoLibrary.GetTVShows", "tvshows", "tvshowid")] {
        let r = call_json(tv, method, filter.clone())?;
        out.extend(parse_found(&r, kind, list, id));
    }
    Ok(out)
}

/// The movies or shows (`kind`) in a `VideoLibrary.Get…` answer's `list`,
/// each by its `id` field.
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

/// Play a film, or open a TV show's episodes, by title; Kodi to the front
/// first unless `front` is false.
fn play(tv: &Tv, q: &str, front: bool) -> Result<String> {
    let found = search_library(tv, q)?;
    let Some(f) = best_match(&found, q) else {
        return match found.len() {
            0 => failed(format!("nothing in Kodi's library called \"{q}\"")),
            n => failed(format!("{n} things in Kodi's library match \"{q}\"; say which:\n{}", found.iter().map(found_line).collect::<Vec<_>>().join("\n"))),
        };
    };
    if front {
        let _ = tveee_core::builtin::apps::bring_to_front(tv, PACKAGE);
    }
    if f.kind == "movie" {
        call_json(tv, "Player.Open", serde_json::json!({ "item": { "movieid": f.id } }))?;
        Ok(format!("playing {}", f.title))
    } else {
        let link = link::Link::window("videos", Some(&format!("videodb://tvshows/titles/{}/", f.id))).to_string();
        call_json(tv, "GUI.ActivateWindow", window_params(&link).unwrap_or_default())?;
        Ok(format!("opened {} in Kodi", f.title))
    }
}

/// Kodi's music visualisation window (shown full screen while music plays).
pub const VISUALISATION: u64 = 12006;

/// What to set full screen to, to get the visualiser `want` (None: switch)
/// when it's `now` showing; None: nothing to do.
pub fn visualiser_step(now: bool, want: Option<bool>) -> Option<bool> {
    let want = want.unwrap_or(!now);
    (want != now).then_some(want)
}

fn window_id(tv: &Tv) -> Result<u64> {
    let r = call(tv, "GUI.GetProperties", r#"{"properties":["currentwindow"]}"#)?;
    Ok(r.pointer("/currentwindow/id").and_then(Value::as_u64).unwrap_or(0))
}

/// Show Kodi's music visualiser or go back from it (None: switch), while it
/// plays music: full screen is the visualiser then. Whether it's showing.
fn visualiser(tv: &Tv, want: Option<bool>) -> Result<bool> {
    match player_id(tv)? {
        Some((_, kind)) if kind == "audio" => {}
        Some(_) => return failed("Kodi's visualiser is for music: it's playing a video"),
        None => return failed("Kodi isn't playing music (its visualiser needs some)"),
    }
    let now = window_id(tv)? == VISUALISATION;
    let Some(to) = visualiser_step(now, want) else { return Ok(now) };
    call_json(tv, "GUI.SetFullscreen", serde_json::json!({ "fullscreen": to }))?;
    // as Kodi has it now
    std::thread::sleep(std::time::Duration::from_millis(500));
    Ok(window_id(tv)? == VISUALISATION)
}

/// The playing video's player id, or a failure saying nothing is playing.
/// Switch the video playing to audio stream `index` (`audio`), or to
/// subtitle `index` (None: subtitles off).
fn switch_track(tv: &Tv, audio: bool, index: Option<u64>) -> Result<String> {
    let id = video_player(tv)?;
    match (audio, index) {
        (true, Some(n)) => {
            call_json(tv, "Player.SetAudioStream", serde_json::json!({ "playerid": id, "stream": n })).map(|_| format!("audio: track {}", n + 1))
        }
        (true, None) => failed("an audio track to switch to, by its number"),
        (false, Some(n)) => call_json(tv, "Player.SetSubtitle", serde_json::json!({ "playerid": id, "subtitle": n, "enable": true }))
            .map(|_| format!("subtitles: track {}", n + 1)),
        (false, None) => call_json(tv, "Player.SetSubtitle", serde_json::json!({ "playerid": id, "subtitle": "off" })).map(|_| "subtitles off".to_string()),
    }
}

/// The video player's id; an error if Kodi isn't playing a video.
pub(crate) fn video_player(tv: &Tv) -> Result<u32> {
    match player_id(tv)? {
        Some((id, kind)) if kind == "video" => Ok(id),
        _ => failed("Kodi isn't playing a video"),
    }
}

fn subtitles(tv: &Tv, how: &str) -> Result<String> {
    let id = video_player(tv)?;
    let params = match how {
        "on" | "off" => serde_json::json!({ "playerid": id, "subtitle": how }),
        _ => serde_json::json!({ "playerid": id, "subtitle": how, "enable": true }),
    };
    call_json(tv, "Player.SetSubtitle", params)?;
    let props = call_json(tv, "Player.GetProperties", serde_json::json!({ "playerid": id, "properties": ["currentsubtitle", "subtitleenabled"] }))?;
    Ok(match props.get("subtitleenabled").and_then(Value::as_bool) {
        Some(true) => format!("subtitles: {}", track_name(props.get("currentsubtitle")).unwrap_or_else(|| "on".into())),
        _ => "subtitles off".into(),
    })
}

fn audio(tv: &Tv, how: &str) -> Result<String> {
    let id = video_player(tv)?;
    call_json(tv, "Player.SetAudioStream", serde_json::json!({ "playerid": id, "stream": how }))?;
    let props = call_json(tv, "Player.GetProperties", serde_json::json!({ "playerid": id, "properties": ["currentaudiostream"] }))?;
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
    call_json(tv, "GUI.ShowNotification", serde_json::json!({ "title": "tveee", "message": text }))?;
    Ok("shown in Kodi".into())
}

/// `kodi.rpc`'s arguments: a method, then its params as JSON (`{}` when
/// there are none). The params are checked here, so a typo is said plainly
/// rather than as Kodi's parse error.
pub fn rpc_request(args: &[String]) -> Result<(String, String)> {
    let all = args.join(" ");
    let (method, params) = all.trim().split_once(char::is_whitespace).unwrap_or((all.trim(), ""));
    if method.is_empty() || !method.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_') {
        return tveee_core::error::usage("usage: tveee kodi-rpc <Method.Name> [json params], like: tveee kodi-rpc Player.GetActivePlayers");
    }
    let params = match params.trim() {
        "" => "{}".to_string(),
        p => match serde_json::from_str::<Value>(p) {
            Ok(v @ (Value::Object(_) | Value::Array(_))) => v.to_string(),
            Ok(_) => return tveee_core::error::usage("kodi-rpc: the params are a JSON object, like {\"playerid\":1}"),
            Err(e) => return tveee_core::error::usage(format!("kodi-rpc: the params aren't JSON: {e}")),
        },
    };
    Ok((method.to_string(), params))
}

fn rpc(tv: &Tv, args: &[String]) -> Result<String> {
    let (method, params) = rpc_request(args)?;
    // anything goes here, slow ones too (a directory on a network share)
    let params = serde_json::from_str::<Value>(&params).unwrap_or(Value::Null);
    let result = rpc::ask_within(tv, &rpc::Raw { method: &method, params }, 30)?;
    Ok(serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string()))
}

/// Words joined into one (a title, a message), or a usage error saying
/// what's missing.
fn words(args: &[String], what: &str) -> Result<String> {
    match args.join(" ").trim() {
        "" => tveee_core::error::usage(format!("missing {what}")),
        w => Ok(w.to_string()),
    }
}

/// What it can do: `tveee app kodi <action>`, `tveee do kodi.<action>`, the ui menu.
pub fn actions() -> Vec<ActionDescriptor> {
    use tveee_core::actions::ActionDescriptor as A;
    vec![
        A::new("kodi.subtitles", "kodi", "kodi: next subtitles", |cx, a| match a.first().map(String::as_str) {
            None => subtitles(cx.tv, "next").map(Into::into),
            Some(how @ ("next" | "off" | "on" | "previous")) => subtitles(cx.tv, how).map(Into::into),
            Some(other) => tveee_core::error::usage(format!("subtitles: next, off, on or previous, not '{other}'")),
        })
        .args("[next|off|on|previous]")
        .key('k')
        .while_playing("video"),
        A::new("kodi.subtitles.off", "kodi", "kodi: subtitles off", |cx, _| subtitles(cx.tv, "off").map(Into::into)).key('K').while_playing("video"),
        A::new("kodi.audio", "kodi", "kodi: next audio track", |cx, a| match a.first().map(String::as_str) {
            None => audio(cx.tv, "next").map(Into::into),
            Some(how @ ("next" | "previous")) => audio(cx.tv, how).map(Into::into),
            Some(other) => tveee_core::error::usage(format!("audio: next or previous, not '{other}'")),
        })
        .args("[next|previous]")
        .key('a')
        .while_playing("video"),
        A::new("kodi.scan", "kodi", "kodi: look for new videos (library scan)", |cx, a| match a.first().map(String::as_str) {
            None => call(cx.tv, "VideoLibrary.Scan", "{}").map(|_| "Kodi is looking for new videos (its library scan)".into()),
            // the older spelling, `app kodi scan clean`
            Some("clean") => clean(cx.tv),
            Some(other) => tveee_core::error::usage(format!("scan: takes nothing (or 'clean'), not '{other}'")),
        })
        .args("[clean]")
        .key('L'),
        A::new("kodi.find", "kodi", "kodi: find in the library", |cx, a| find(cx.tv, &words(a, "a title")?).map(Into::into))
            .args("<words>")
            .key('F')
            .text("Find in Kodi:", "a film or show's title"),
        A::new("kodi.play", "kodi", "kodi: play from the library", |cx, a| play(cx.tv, &words(a, "a title")?, cx.front).map(Into::into))
            .args("<words>")
            .brings_to_front(tveee_core::builtin::apps::Front::Package(PACKAGE))
            .text("Play in Kodi:", "a film or show's title"),
        A::new("kodi.clean", "kodi", "kodi: forget videos that are gone (library clean)", |cx, _| clean(cx.tv))
            .confirm("Remove the videos Kodi can't find any more from its library?"),
        A::new("kodi.notify", "kodi", "kodi: show a notification", |cx, a| notify(cx.tv, &words(a, "a message")?).map(Into::into))
            .args("<text>")
            .text("Show in Kodi:", "a line of text"),
        A::new("kodi.addons", "kodi", "kodi: list add-ons", |cx, a| addons::list_cmd(cx.tv, a).map(Into::into))
            .cli("kodi-addons", "[installed|available] [words]", "list Kodi's add-ons, or those you can install")
            .long(
                "Lists Kodi's plugins (video, music, pictures and programs add-ons):\n\
                 those installed, or with 'available' those its repositories offer.\n\
                 Words narrow it down (name, id or what it says it is):\n\
                 \n\
                 \x20 tveee kodi-addons\n\
                 \x20 tveee kodi-addons available radio\n\
                 \n\
                 tveee kodi-addon <name> opens one's page in Kodi; tveee kodi-install\n\
                 <name> installs it.",
            )
            .args("[installed|available] [words]")
            .text("Kodi add-ons (available, or words):", "available radio"),
        A::new("kodi.addon", "kodi", "kodi: open an add-on's page", |cx, a| addons::page_cmd(cx.tv, &words(a, "an add-on's name")?).map(Into::into))
            .cli("kodi-addon", "<name or id>", "open a Kodi add-on's page (installed or not)")
            .args("<name or id>")
            .text("Open the Kodi add-on:", "an add-on's name or id"),
        A::new("kodi.install", "kodi", "kodi: install an add-on", |cx, a| {
            let what = words(a, "an add-on's name, or a zip file")?;
            let path = std::path::Path::new(&what);
            if what.to_lowercase().ends_with(".zip") || path.is_file() {
                sideload::install_zip(cx.tv, path).map(Into::into)
            } else {
                addons::install_cmd(cx.tv, &what).map(Into::into)
            }
        })
        .cli("kodi-install", "<name, id or file.zip>", "install a Kodi add-on: from its repositories, or a zip here")
        .long(
            "Installs a Kodi add-on:\n\
             \n\
             \x20 tveee kodi-install <name or id>   from Kodi's repositories (see\n\
             \x20                                   'tveee kodi-addons available')\n\
             \x20 tveee kodi-install <file.zip>     from a zip file on this computer\n\
             \n\
             From the repositories: Kodi's API can't install, so tveee opens the\n\
             add-on's page in Kodi and presses Install there, then waits until Kodi\n\
             has it (and what it needs). If Kodi asks anything else on the way, it's\n\
             left on the TV's screen and said.\n\
             \n\
             From a zip: no Kodi screens. tveee checks the zip (one folder, named\n\
             after the add-on, with its addon.xml) and what it needs, copies it into\n\
             Kodi's add-ons over adb, and restarts Kodi (back where it was) so it\n\
             finds it. What it needs from Kodi's repositories is said first, to\n\
             install by name. Installing a zip again updates it. An add-on runs with\n\
             Kodi's access to the TV: install zips you trust.",
        )
        .args("<name, id or file.zip>")
        .text("Install in Kodi:", "an add-on's name or id, or a zip file's path"),
        A::new("kodi.uninstall", "kodi", "kodi: uninstall an add-on", |cx, a| sideload::uninstall(cx.tv, &words(a, "an add-on's id")?).map(Into::into))
            .cli("kodi-uninstall", "<id>", "uninstall a Kodi add-on (restarts Kodi)")
            .long(
                "Takes an add-on out of Kodi's add-ons folder over adb and restarts\n\
                 Kodi (back where it was). Not when another add-on needs it. Its\n\
                 settings (in Kodi's userdata) stay, for if it comes back.",
            )
            .args("<id>")
            .text("Uninstall from Kodi:", "an add-on's id (plugin.video....)")
            .confirm("Take this add-on off Kodi? Kodi restarts."),
        A::new("kodi.rpc", "kodi", "kodi: call its JSON-RPC API", |cx, a| rpc(cx.tv, a).map(Into::into))
            .cli("kodi-rpc", "<method> [json params]", "call Kodi's JSON-RPC API and print the result")
            .long(
                "Calls a method of Kodi's JSON-RPC API (its remote control, the same one\n\
                 tveee uses) and prints the result, like 'tveee jointspace' does for\n\
                 Philips' API. The params are a JSON object (quote them for the shell):\n\
                 \n\
                 \x20 tveee kodi-rpc JSONRPC.Version\n\
                 \x20 tveee kodi-rpc Player.GetActivePlayers\n\
                 \x20 tveee kodi-rpc Player.GetProperties '{\"playerid\":1,\"properties\":[\"subtitles\"]}'\n\
                 \n\
                 The methods are listed by 'tveee kodi-rpc JSONRPC.Introspect' and at\n\
                 https://kodi.wiki/view/JSON-RPC_API. With a password set in Kodi, run\n\
                 'tveee pair kodi' first.",
            )
            .args("<method> [json params]")
            .text("Kodi JSON-RPC:", "Player.GetActivePlayers"),
    ]
}

fn clean(tv: &Tv) -> Result<tveee_core::actions::Output> {
    call(tv, "VideoLibrary.Clean", "{}").map(|_| "Kodi is removing missing videos from its library".into())
}

/// This plugin's providers: Kodi.
pub fn providers() -> Vec<&'static dyn tveee_core::provider::Provider> {
    vec![&KODI]
}

// --- pairing ---------------------------------------------------------------------

/// Pairing with Kodi (`tveee pair kodi`): its port, user and password.
pub struct KodiLogin;
/// The one Kodi pairing.
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

impl tveee_core::pairing::Pairing for KodiLogin {
    fn id(&self) -> &'static str {
        PAIRING
    }
    // only with a password: its diagnose check says when
    fn optional(&self) -> bool {
        true
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
    fn pair(&self, tv: &Tv) -> Result<tveee_core::pairing::Saved> {
        use std::io::IsTerminal;
        if !std::io::stdin().is_terminal() {
            return tveee_core::error::usage("pair kodi: run it in a terminal to type Kodi's login");
        }
        println!("In Kodi: Settings > Services > Control > Allow remote control via HTTP (on).");
        let port = ask("Port", &DEFAULT_PORT.to_string())?;
        let user = ask("Username", DEFAULT_USER)?;
        let pass = ask("Password (empty if none)", "")?;
        let Ok(port_n) = port.parse::<u16>() else { return tveee_core::error::usage(format!("pair kodi: '{port}' isn't a port")) };
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"JSONRPC.Ping"}"#;
        let reply = post(tv, &(port_n, user.clone(), pass.clone()), body)?;
        if !reply.contains("pong") {
            return failed(format!("pair kodi: Kodi answered oddly: {}", reply.trim()));
        }
        Ok(tveee_core::pairing::Saved(vec![("port".into(), port), ("user".into(), user), ("pass".into(), pass)]))
    }
    fn check(&self, tv: &Tv, _saved: &tveee_core::pairing::Saved) -> Option<String> {
        Some(match now(tv) {
            Ok(Some((p, _, _))) => format!("check: Kodi is playing {}", p.title.unwrap_or_default()),
            Ok(None) => "check: Kodi answers (nothing playing)".into(),
            Err(e) => format!("check failed: {e}"),
        })
    }
}

/// This plugin's pairings: Kodi's login.
pub fn pairings() -> Vec<&'static dyn tveee_core::pairing::Pairing> {
    vec![&KODI_LOGIN]
}

/// Nothing of its own: `tveee app kodi`, `goto kodi`, `seek`, ... cover it.
#[cfg(test)]
mod tests {

    #[test]
    fn visualiser_steps() {
        use super::visualiser_step;
        // switch
        assert_eq!(visualiser_step(false, None), Some(true));
        assert_eq!(visualiser_step(true, None), Some(false));
        // asked for what it is: nothing to do
        assert_eq!(visualiser_step(true, Some(true)), None);
        assert_eq!(visualiser_step(false, Some(false)), None);
        assert_eq!(visualiser_step(false, Some(true)), Some(true));
    }

    use super::*;

    // as Kodi 21 answers
    const ITEM: &str = r#"{"item":{"art":{"fanart":"image://smb%3a%2f%2fnas%2ffanart.jpg/","poster":"image://p.jpg/"},"episode":4,"fanart":"","file":"smb://nas/tv/Taskmaster/S03E04.mkv","id":7,"label":"S03E04","season":3,"showtitle":"Taskmaster","streamdetails":{"audio":[{"channels":6,"codec":"ac3","language":"eng"}],"subtitle":[{"language":"eng"}],"video":[{"aspect":1.78,"codec":"h264","duration":2700,"height":1080,"width":1920}]},"thumbnail":"","title":"A Very Nuanced Character","type":"episode","year":2016}}"#;
    const PROPS: &str = r#"{"currentaudiostream":{"bitrate":0,"channels":6,"codec":"ac3","index":0,"language":"eng","name":"English"},"currentsubtitle":{"index":0,"language":"eng","name":"English"},"speed":1,"subtitleenabled":false,"time":{"hours":0,"milliseconds":500,"minutes":3,"seconds":20},"totaltime":{"hours":0,"milliseconds":0,"minutes":45,"seconds":0}}"#;

    #[test]
    fn details_of_an_episode() {
        let item = json(
            r#"{"title":"A Very Nuanced Character","showtitle":"Taskmaster","season":3,"episode":4,"year":2016,"rating":7.84,"genre":["Comedy"],"director":[],"cast":[{"name":"Greg Davies","role":"Taskmaster"},{"name":"Alex Horne"}],"runtime":2700,"plot":"Five comedians.","tagline":"","streamdetails":{"audio":[{"channels":6,"codec":"ac3","language":"eng"},{"channels":2,"codec":"aac","language":"fre"}],"subtitle":[{"language":"eng"}],"video":[{"codec":"h264","height":1080}]}}"#,
        );
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
        let r = json(
            r#"{"limits":{"end":2,"start":0,"total":2},"movies":[{"label":"Dune","movieid":3,"title":"Dune","year":2021},{"label":"Dune: Part Two","movieid":9,"title":"Dune: Part Two","year":2024}]}"#,
        );
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
    fn tracks_come_back_after_a_restart() {
        // English audio (the first), English subtitles off
        let t = Tracks::of(&json(PROPS));
        assert_eq!(t, Tracks { audio: Some(0), subtitle: Some(0), subtitles_on: false });
        assert_eq!(Tracks::from_extra(&t.to_extra()), Some(t.clone()));
        assert_eq!(
            t.calls(1),
            vec![
                ("Player.SetAudioStream", r#"{"playerid":1,"stream":0}"#.to_string()),
                ("Player.SetSubtitle", r#"{"playerid":1,"subtitle":"off"}"#.to_string())
            ]
        );
        // nothing said about the usual tracks
        assert_eq!(tracks_said(&json(PROPS)), "");
        // the second audio track, French subtitles on
        let props = json(
            r#"{"currentaudiostream":{"codec":"dts","index":1,"name":"Commentary"},"currentsubtitle":{"index":2,"language":"fre","name":"French"},"subtitleenabled":true}"#,
        );
        let t = Tracks::of(&props);
        assert_eq!(t.calls(1)[1].1, r#"{"playerid":1,"subtitle":2,"enable":true}"#);
        assert_eq!(tracks_said(&props), ", Commentary DTS audio, French subtitles");
        // an older session, or another app's: nothing to put back
        assert_eq!(Tracks::from_extra(""), None);
    }

    #[test]
    fn rpc_arguments() {
        let args = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
        assert_eq!(rpc_request(&args("JSONRPC.Version")).unwrap(), ("JSONRPC.Version".into(), "{}".into()));
        // as the shell splits it, or as one line typed in the ui
        let want = ("Player.GetProperties".to_string(), r#"{"playerid":1,"properties":["time"]}"#.to_string());
        assert_eq!(rpc_request(&args(r#"Player.GetProperties {"playerid": 1, "properties": ["time"]}"#)).unwrap(), want);
        assert_eq!(rpc_request(&[r#"Player.GetProperties {"playerid":1,"properties":["time"]}"#.into()]).unwrap(), want);
        assert!(rpc_request(&[]).is_err());
        assert!(rpc_request(&args(r#"Bad"Method"#)).is_err());
        assert!(rpc_request(&args("Player.Stop {playerid:1}")).is_err());
        assert!(rpc_request(&args("Player.Stop 1")).is_err());
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
        assert_eq!(time_json(3723.25), serde_json::json!({ "hours": 1, "minutes": 2, "seconds": 3, "milliseconds": 250 }));
        assert_eq!(seek_value(SeekTo::By(-10.0)), serde_json::json!({ "seconds": -10 }));
        assert_eq!(seek_value(SeekTo::Fraction(0.4)), serde_json::json!({ "percentage": 40.0 }));
        assert_eq!(seek_value(SeekTo::At(90.0))["time"]["minutes"], 1);
        assert_eq!(seek_value(SeekTo::At(90.0))["time"]["seconds"], 30);
        // sent as an object, not a string of one
        assert!(serde_json::json!({ "value": seek_value(SeekTo::By(5.0)) })["value"].is_object());
    }

    #[test]
    fn places_are_windows() {
        assert_eq!(window_params("kodi-window:home"), Some(serde_json::json!({ "window": "home" })));
        assert_eq!(
            window_params("kodi-window:videos|videodb://movies/titles/"),
            Some(serde_json::json!({ "window": "videos", "parameters": ["videodb://movies/titles/"] }))
        );
        assert_eq!(window_params("https://x"), None);
    }
}

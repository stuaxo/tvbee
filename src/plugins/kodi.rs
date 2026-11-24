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
    tv.connect()?;
    let out = tv.shell(&["cmd", "package", "list", "packages", PACKAGE])?;
    Ok(out.lines().any(|l| l.trim() == format!("package:{PACKAGE}")))
}

// --- JSON bits ---------------------------------------------------------------

/// The raw JSON value after the first `"key":` in `json` (an object, array,
/// string, number, ...).
pub fn value_of<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let pat = format!("\"{key}\"");
    let mut from = 0;
    loop {
        let at = from + json[from..].find(&pat)?;
        let rest = json[at + pat.len()..].trim_start();
        if let Some(v) = rest.strip_prefix(':') {
            let v = v.trim_start();
            let start = json.len() - v.len();
            return Some(&json[start..start + value_len(v)?]);
        }
        from = at + pat.len();
    }
}

/// The length of the JSON value `v` starts with.
fn value_len(v: &str) -> Option<usize> {
    let b = v.as_bytes();
    match b.first()? {
        b'{' | b'[' => {
            let (mut depth, mut in_str, mut esc) = (0i32, false, false);
            for (i, c) in b.iter().enumerate() {
                if in_str {
                    match (esc, c) {
                        (true, _) => esc = false,
                        (false, b'\\') => esc = true,
                        (false, b'"') => in_str = false,
                        _ => {}
                    }
                    continue;
                }
                match c {
                    b'"' => in_str = true,
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(i + 1);
                        }
                    }
                    _ => {}
                }
            }
            None
        }
        b'"' => {
            let mut esc = false;
            for (i, c) in b.iter().enumerate().skip(1) {
                match (esc, c) {
                    (true, _) => esc = false,
                    (false, b'\\') => esc = true,
                    (false, b'"') => return Some(i + 1),
                    _ => {}
                }
            }
            None
        }
        _ => Some(v.find([',', '}', ']']).unwrap_or(v.len())),
    }
}

/// The first string in a JSON list (`["a", "b"]`).
fn first_string(list: &str) -> Option<String> {
    let start = list.find('"')?;
    let len = value_len(&list[start..])?;
    crate::http::json_string(&list[start..start + len]).filter(|s| !s.is_empty())
}

fn str_of(json: &str, key: &str) -> Option<String> {
    crate::http::json_string(value_of(json, key)?).filter(|s| !s.is_empty())
}

fn num_of(json: &str, key: &str) -> Option<f64> {
    value_of(json, key)?.trim().parse().ok()
}

/// Kodi's time object (`{"hours":0,"minutes":3,"seconds":20,"milliseconds":5}`) in seconds.
pub fn seconds(time: &str) -> Option<f64> {
    let n = |k| num_of(time, k).unwrap_or(0.0);
    value_of(time, "seconds")?;
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
    let reply = crate::https::request(&crate::https::Request { url: &url, body: Some(body), digest: None, basic: auth, retry: false, timeout_secs: 4 });
    match reply {
        Ok((200, text)) => Ok(text),
        Ok((401, _)) => failed("Kodi wants a username and password: run 'tveee pair kodi'"),
        Ok((code, _)) => failed(format!("Kodi answered HTTP {code}")),
        Err(_) => Err(off()),
    }
}

/// Call `method` with `params` (a JSON object); returns its `result`.
pub fn call(tv: &Tv, method: &str, params: &str) -> Result<String> {
    let body = format!(r#"{{"jsonrpc":"2.0","id":1,"method":"{method}","params":{params}}}"#);
    let reply = post(tv, &login(tv), &body)?;
    result_of(&reply, method)
}

/// The `result` of a JSON-RPC reply, or its error.
pub fn result_of(reply: &str, method: &str) -> Result<String> {
    if let Some(e) = value_of(reply, "error") {
        let msg = str_of(e, "message").unwrap_or_else(|| e.to_string());
        return failed(format!("Kodi: {method}: {msg}"));
    }
    match value_of(reply, "result") {
        Some(r) => Ok(r.to_string()),
        None => failed(format!("Kodi: {method}: no result")),
    }
}

/// Whether Kodi's API answers now.
pub fn ping(tv: &Tv) -> bool {
    call(tv, "JSONRPC.Ping", "{}").is_ok_and(|r| r.contains("pong"))
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
    Ok(num_of(&r, "playerid").map(|id| (id as u32, str_of(&r, "type").unwrap_or_default())))
}

const ITEM_FIELDS: &str = r#"["title","showtitle","season","episode","artist","album","year","file","art","thumbnail","fanart","streamdetails"]"#;
const PLAYER_FIELDS: &str = r#"["time","totaltime","speed","currentaudiostream","currentsubtitle","subtitleenabled"]"#;

/// What's playing, from Player.GetItem's and Player.GetProperties' results.
pub fn parse_playing(item: &str, props: &str) -> Playing {
    let item = value_of(item, "item").unwrap_or(item);
    let title = str_of(item, "title").or_else(|| str_of(item, "label"));
    let num = |k| num_of(item, k).filter(|n| *n >= 0.0).map(|n| n as u32);
    let subtitle = match (str_of(item, "showtitle"), num("season"), num("episode")) {
        (Some(show), Some(s), Some(e)) => Some(format!("{show} S{s:02}E{e:02}")),
        (Some(show), _, _) => Some(show),
        _ => value_of(item, "artist").and_then(first_string).or_else(|| num("year").filter(|y| *y > 0).map(|y| y.to_string())),
    };
    let time = value_of(props, "time").and_then(seconds);
    let total = value_of(props, "totaltime").and_then(seconds).filter(|t| *t > 0.0);
    let paused = num_of(props, "speed").is_some_and(|s| s == 0.0);
    let progress = time.map(|position| Progress { position, duration: total, paused });
    // a picture: fanart or a poster/thumb
    let art = value_of(item, "art").and_then(|a| ["fanart", "poster", "thumb"].iter().find_map(|k| str_of(a, k))).or_else(|| str_of(item, "fanart")).or_else(|| str_of(item, "thumbnail"));
    let mut extra = Vec::new();
    if let Some(sd) = value_of(item, "streamdetails") {
        let first = |k| value_of(sd, k).and_then(|list| crate::http::objects(list).first().map(|o| o.to_string()));
        if let Some(v) = first("video") {
            let h = num_of(&v, "height").unwrap_or(0.0) as u32;
            let codec = str_of(&v, "codec").unwrap_or_default().to_uppercase();
            if h > 0 {
                extra.push(("video", format!("{h}p {codec}").trim().to_string()));
            }
        }
    }
    let track = |k: &str| {
        let t = value_of(props, k)?;
        let name = str_of(t, "name").or_else(|| str_of(t, "language"))?;
        let codec = str_of(t, "codec").map(|c| format!(" {}", c.to_uppercase())).unwrap_or_default();
        Some(format!("{name}{codec}"))
    };
    if let Some(a) = track("currentaudiostream") {
        extra.push(("audio", a));
    }
    let subs_on = value_of(props, "subtitleenabled").is_some_and(|v| v.trim() == "true");
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
    let file = value_of(&item, "item").and_then(|i| str_of(i, "file"));
    Ok(Some((parse_playing(&item, &props), file)))
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
            value_of(&r, "time").and_then(seconds).map_or_else(|| failed("Kodi didn't say where it seeked to"), Ok)
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
    fn picture(&self, tv: &Tv, art: &str, _at: Option<f64>) -> Outcome<Vec<u8>> {
        self.art_bytes(tv, art).into()
    }
}

impl Kodi {
    /// Kodi's picture `art` (an image:// address), fetched from it.
    #[cfg_attr(not(feature = "ui"), allow(dead_code))]
    fn art_bytes(&self, tv: &Tv, art: &str) -> Result<Vec<u8>> {
        let r = call(tv, "Files.PrepareDownload", &format!(r#"{{"path":{}}}"#, quote(art)))?;
        let Some(path) = value_of(&r, "details").and_then(|d| str_of(d, "path")) else { return failed("Kodi didn't offer the picture") };
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
    fn reads_what_is_playing() {
        let p = parse_playing(ITEM, PROPS);
        assert_eq!(p.title.as_deref(), Some("A Very Nuanced Character"));
        assert_eq!(p.subtitle.as_deref(), Some("Taskmaster S03E04"));
        let pr = p.progress.unwrap();
        assert_eq!((pr.position, pr.duration, pr.paused), (200.5, Some(2700.0), false));
        assert_eq!(p.art.as_deref(), Some("image://smb%3a%2f%2fnas%2ffanart.jpg/"));
        assert_eq!(p.extra, vec![("video", "1080p H264".to_string()), ("audio", "English AC3".to_string())]);
        // paused, subtitles on, a film
        let props = PROPS.replace(r#""speed":1"#, r#""speed":0"#).replace(r#""subtitleenabled":false"#, r#""subtitleenabled":true"#);
        let film = r#"{"item":{"title":"Heat","year":1995,"art":{},"file":"/sdcard/heat.mkv","season":-1,"episode":-1,"showtitle":""}}"#;
        let p = parse_playing(film, &props);
        assert_eq!((p.subtitle.as_deref(), p.progress.unwrap().paused, p.art), (Some("1995"), true, None));
        assert!(p.extra.iter().any(|(k, v)| *k == "subtitles" && v == "English"));
    }

    #[test]
    fn json_values() {
        let j = r#"{"result":{"a":"x\"}","b":[1,{"c":2}],"n":-3.5},"id":1}"#;
        assert_eq!(value_of(j, "b"), Some(r#"[1,{"c":2}]"#));
        assert_eq!(value_of(j, "a"), Some(r#""x\"}""#));
        assert_eq!(num_of(j, "n"), Some(-3.5));
        assert_eq!(result_of(j, "m").unwrap(), r#"{"a":"x\"}","b":[1,{"c":2}],"n":-3.5}"#);
        let err = r#"{"error":{"code":-32601,"message":"Method not found."},"id":1}"#;
        assert!(result_of(err, "X.Y").unwrap_err().to_string().contains("Method not found"));
    }

    #[test]
    fn times_and_seeks() {
        assert_eq!(seconds(r#"{"hours":1,"milliseconds":250,"minutes":2,"seconds":3}"#), Some(3723.25));
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

//! Philips Android TV extras, built on the TV's "JointSpace" REST service
//! (Restlet on ports 1925 http / 1926 https). Vendor-specific, so only
//! enabled when the Philips system service package is installed.

use crate::error::{failed, usage, Result};
use crate::registry::Command;
use crate::tv::Tv;
use crate::Ctx;

const CAT: &str = "philips";
const JOINTSPACE_PKG: &str = "org.droidtv.xtv";

pub fn detect(tv: &Tv) -> Result<bool> {
    tv.connect()?;
    let out = tv.shell(&["pm", "list", "packages", JOINTSPACE_PKG])?;
    Ok(out.lines().any(|l| l.trim() == format!("package:{JOINTSPACE_PKG}")))
}

/// The shared key Philips' own remote app signs pairing with (as documented
/// by community clients such as pylips).
const PAIRING_SECRET: &str = "ZmVay1EQVFOaZhwQ4Kv81ypLAZNczV9sG4KkseXWn1NEk6cXmPKO/MCa9sryslvLCFMnNe4Z4CPXzToowvhHvA==";

/// base64 of the hex HMAC-SHA1 of `<timestamp><pin>`, keyed with the secret.
pub fn pairing_signature(timestamp: i64, pin: &str) -> String {
    let key = crate::crypto::base64_decode(PAIRING_SECRET).unwrap_or_default();
    let mac = crate::crypto::hmac_sha1(&key, format!("{timestamp}{pin}").as_bytes());
    crate::crypto::base64(crate::crypto::hex(&mac).as_bytes())
}

/// How tvbee introduces itself to the TV.
fn device_json(id: &str) -> String {
    let os = if cfg!(windows) { "Windows" } else { "Linux" };
    format!(r#"{{"device_name":"tvbee","device_os":"{os}","app_name":"tvbee","type":"native","app_id":"app.id","id":"{id}"}}"#)
}

/// A random-enough id for this pairing (16 hex digits).
fn new_device_id() -> String {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    crate::crypto::hex(&crate::crypto::sha1(format!("{t}-{}", std::process::id()).as_bytes()))[..16].to_string()
}

fn https_url(ip: &str, path: &str) -> String {
    format!("https://{ip}:1926{path}")
}

const PAIRING: &str = "jointspace";

/// The saved pairing's digest-auth (user, key).
fn credentials(saved: &crate::pairing::Saved) -> Option<(&str, &str)> {
    Some((saved.get("user")?, saved.get("key")?))
}

/// Whether tvbee is paired with this TV's JointSpace.
pub fn paired(tv: &Tv) -> bool {
    crate::pairing::load(PAIRING, &tv.ip).as_ref().and_then(credentials).is_some()
}

fn https_get(tv: &Tv, saved: &crate::pairing::Saved, path: &str) -> Result<(u16, String)> {
    let Some(digest) = credentials(saved) else { return Err(crate::pairing::required_error(PAIRING, path)) };
    crate::https::request(&crate::https::Request { url: &https_url(&tv.ip, path), body: None, digest: Some(digest), timeout_secs: 10 })
}

/// GET a path that needs pairing; `what` names it in the "pairing required"
/// error ("the channel list").
#[allow(dead_code)]
pub fn js_get_paired(tv: &Tv, path: &str, what: &str) -> Result<(u16, String)> {
    let saved = crate::pairing::require(PAIRING, tv, what)?;
    https_get(tv, &saved, path)
}

/// POST to a path that needs pairing.
#[allow(dead_code)]
pub fn js_post(tv: &Tv, path: &str, body: &str, what: &str) -> Result<(u16, String)> {
    let saved = crate::pairing::require(PAIRING, tv, what)?;
    let Some(digest) = credentials(&saved) else { return Err(crate::pairing::required_error(PAIRING, what)) };
    crate::https::request(&crate::https::Request { url: &https_url(&tv.ip, path), body: Some(body), digest: Some(digest), timeout_secs: 10 })
}

fn read_pin() -> Result<String> {
    use std::io::{BufRead, IsTerminal, Write};
    if !std::io::stdin().is_terminal() {
        return usage("pair: run it in a terminal to type the PIN the TV shows");
    }
    print!("Type the PIN shown on the TV: ");
    std::io::stdout().flush()?;
    let mut pin = String::new();
    std::io::stdin().lock().read_line(&mut pin)?;
    let pin = pin.trim().to_string();
    if pin.is_empty() || !pin.chars().all(|c| c.is_ascii_digit()) {
        return usage(format!("pair: '{pin}' isn't a PIN (digits only)"));
    }
    Ok(pin)
}

/// Pairing with Philips' JointSpace API: the TV shows a PIN, the user types
/// it, and the TV grants a key used for digest auth from then on.
pub struct JointSpace;
pub static JOINTSPACE: JointSpace = JointSpace;

impl crate::pairing::Pairing for JointSpace {
    fn id(&self) -> &'static str {
        PAIRING
    }
    fn name(&self) -> &'static str {
        "the Philips TV API (JointSpace)"
    }
    fn unlocks(&self) -> &'static str {
        "channel names and numbers, the current channel, more of 'tvbee jointspace'"
    }
    fn how(&self) -> &'static str {
        "the TV shows a 4-digit PIN for about a minute; type it in"
    }

    fn pair(&self, tv: &Tv) -> Result<crate::pairing::Saved> {
        let id = new_device_id();
        let device = device_json(&id);
        let ask = format!(r#"{{"scope":["read","write","control"],"device":{device}}}"#);
        println!("asking {} to pair...", tv.ip);
        let (status, reply) =
            crate::https::request(&crate::https::Request { url: &https_url(&tv.ip, "/6/pair/request"), body: Some(&ask), digest: None, timeout_secs: 15 })?;
        let key = crate::http::string_field(&reply, "auth_key");
        let timestamp = crate::http::number_field(&reply, "timestamp").map(|t| t as i64);
        let (Some(key), Some(timestamp)) = (key, timestamp) else {
            return failed(format!("pair: the TV didn't start pairing (HTTP {status}): {}", reply.trim()));
        };
        let pin = read_pin()?;
        let grant = format!(
            r#"{{"auth":{{"auth_AppId":"1","pin":"{pin}","auth_timestamp":{timestamp},"auth_signature":"{}"}},"device":{device}}}"#,
            pairing_signature(timestamp, &pin)
        );
        let (status, reply) = crate::https::request(&crate::https::Request {
            url: &https_url(&tv.ip, "/6/pair/grant"),
            body: Some(&grant),
            digest: Some((&id, &key)),
            timeout_secs: 15,
        })?;
        let ok = status == 200 && crate::http::string_field(&reply, "error_id").is_none_or(|e| e == "SUCCESS");
        if !ok {
            return failed(format!("pair: the TV refused (HTTP {status}): {}", reply.trim()));
        }
        Ok(crate::pairing::Saved(vec![("user".into(), id), ("key".into(), key)]))
    }

    /// What the TV is doing now: a read only pairing allows.
    fn check(&self, tv: &Tv, saved: &crate::pairing::Saved) -> Option<String> {
        Some(match https_get(tv, saved, "/6/activities/tv") {
            Ok((200, body)) => format!("check: /6/activities/tv answers: {}", body.trim()),
            Ok((code, _)) => format!("check: /6/activities/tv gave HTTP {code} (the pairing is saved anyway)"),
            Err(e) => format!("check failed: {e}"),
        })
    }
}

pub fn pairings() -> Vec<&'static dyn crate::pairing::Pairing> {
    vec![&JOINTSPACE]
}

/// `jointspace [/6/<path>]`: over HTTPS with the pairing if there is one,
/// else plain HTTP, which only has the public routes.
fn jointspace(c: &Ctx, args: &[String]) -> Result<()> {
    let path = match args.first().map(String::as_str) {
        None => "/6/system".to_string(),
        Some(p) if p.starts_with("/6/") => p.to_string(),
        Some("pair") => return usage("pairing moved: run 'tvbee pair jointspace'"),
        Some(_) => return usage("usage: tvbee jointspace [/6/<path>]"),
    };
    let (status, body) = match crate::pairing::load(PAIRING, &c.tv.ip) {
        Some(saved) => https_get(&c.tv, &saved, &path)?,
        None => crate::http::get(&c.tv.ip, 1925, &path)?,
    };
    match status {
        200 => {
            println!("{body}");
            Ok(())
        }
        // unpaired, the plain-HTTP port answers 404 for anything not public
        _ if !paired(&c.tv) => Err(crate::pairing::required_error(PAIRING, &path)),
        // the TV says 401 for paths it doesn't have, too
        401 => failed(format!(
            "jointspace: the TV refused {path} (HTTP 401): it may not have that path; if every path fails, pair again ('tvbee pair jointspace')"
        )),
        _ => {
            println!("{body}");
            failed(format!("jointspace: HTTP {status} for {path}"))
        }
    }
}

/// Philips' apps: the NetTV web apps (each a small wrapper package that opens
/// org.droidtv.nettvbrowser), the browser itself, and the TV app.
pub fn apps() -> Vec<crate::plugins::apps::App> {
    use crate::plugins::apps::{app, known, Kind};
    vec![
        app("iplayer", "iPlayer", "org.droidtv.nettvapp4340/com.zeasn.app.linux.MainActivity", Kind::Video),
        app("dailymotion", "Dailymotion", "org.droidtv.nettvapp1984/com.zeasn.app.linux.MainActivity", Kind::Video),
        app("arte", "Arte", "org.droidtv.nettvapp1814/com.zeasn.app.linux.MainActivity", Kind::Video),
        // the NetTV web apps (iPlayer, Arte, ...) all run inside this browser
        known("a NetTV app", "org.droidtv.nettvbrowser", Kind::Video),
        app("livetv", "live TV", "org.droidtv.zapster", Kind::LiveTv),
    ]
}

/// Live TV over the air (DVB-T, e.g. Freeview), hosted by Philips' TV app
/// (zapster). The guide comes from the broadcast EPG the TV collects
/// (`org.droidtv.epgdvb`, readable without special permissions).
pub struct Freeview;
pub static FREEVIEW: Freeview = Freeview;

const GUIDE_URI: &str = "content://org.droidtv.epgdvb/antenna/scheduletable";
const GUIDE_COLUMNS: &str = "eventname:presetid:starttime:endtime:genre";

/// Rows of `content query --projection eventname:presetid:starttime:endtime:genre`:
/// `Row: 0 eventname=..., presetid=12, starttime=..., endtime=..., genre=2`.
/// Titles may contain ", ", so the other fields are found from the right.
pub fn parse_events(out: &str) -> Vec<crate::provider::Event> {
    out.lines()
        .filter_map(|l| {
            let l = l.trim().strip_prefix("Row: ")?;
            let rest = &l[l.find(" eventname=")? + " eventname=".len()..];
            let (rest, genre) = rest.rsplit_once(", genre=")?;
            let (rest, end) = rest.rsplit_once(", endtime=")?;
            let (rest, start) = rest.rsplit_once(", starttime=")?;
            let (title, preset) = rest.rsplit_once(", presetid=")?;
            if title == "NULL" {
                return None;
            }
            Some(crate::provider::Event {
                channel: preset.parse().ok()?,
                title: title.to_string(),
                start: start.parse().ok()?,
                end: end.parse().ok()?,
                genre: genre.parse().ok(),
            })
        })
        .collect()
}

/// How long a copy of the guide is good for. The broadcast guide changes
/// slowly, and reading it takes the TV a few seconds.
const GUIDE_FRESH: std::time::Duration = std::time::Duration::from_secs(30 * 60);

fn guide_cache_file(ip: &str) -> Option<std::path::PathBuf> {
    Some(crate::plugin::cache_path(ip)?.with_file_name(format!("{ip}.guide")))
}

/// One event per line: `channel start end genre title` (tab-separated).
pub fn render_events(events: &[crate::provider::Event]) -> String {
    events
        .iter()
        .map(|e| format!("{}\t{}\t{}\t{}\t{}\n", e.channel, e.start, e.end, e.genre.map_or(String::new(), |g| g.to_string()), e.title.replace(['\t', '\n'], " ")))
        .collect()
}

pub fn parse_cached_events(text: &str) -> Vec<crate::provider::Event> {
    text.lines()
        .filter_map(|l| {
            let mut f = l.splitn(5, '\t');
            Some(crate::provider::Event {
                channel: f.next()?.parse().ok()?,
                start: f.next()?.parse().ok()?,
                end: f.next()?.parse().ok()?,
                genre: f.next()?.parse().ok(),
                title: f.next()?.to_string(),
            })
        })
        .collect()
}

type Guide = std::sync::Arc<Vec<crate::provider::Event>>;
static GUIDE: std::sync::Mutex<Option<(String, std::time::Instant, Guide)>> = std::sync::Mutex::new(None);

/// The whole guide, sorted by channel then start: from memory, else the
/// cache file, else the TV (which ignores filters, so it's all or nothing).
fn guide_events(tv: &Tv) -> Result<Guide> {
    let mut mem = GUIDE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((_, _, events)) = mem.as_ref().filter(|(ip, at, _)| *ip == tv.ip && at.elapsed() < GUIDE_FRESH) {
        return Ok(events.clone());
    }
    let file = guide_cache_file(&tv.ip);
    let age = file.as_ref().and_then(|f| std::fs::metadata(f).ok()?.modified().ok()?.elapsed().ok());
    let cached = match (&file, age) {
        (Some(f), Some(age)) if age < GUIDE_FRESH => {
            Some((parse_cached_events(&std::fs::read_to_string(f).unwrap_or_default()), std::time::Instant::now() - age))
        }
        _ => None,
    };
    let (events, at) = match cached.filter(|(e, _)| !e.is_empty()) {
        Some(c) => c,
        None => {
            tv.connect()?;
            let cmd = format!("content query --uri {GUIDE_URI} --projection {GUIDE_COLUMNS}");
            let mut events = parse_events(&tv.shell(&[&cmd])?);
            events.sort_by_key(|e| (e.channel, e.start));
            if let Some(f) = &file {
                if let Some(dir) = f.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let _ = std::fs::write(f, render_events(&events));
            }
            (events, std::time::Instant::now())
        }
    };
    let events: Guide = std::sync::Arc::new(events);
    *mem = Some((tv.ip.clone(), at, events.clone()));
    Ok(events)
}

/// Now and next per channel from a channel-sorted, time-sorted event list.
pub fn now_next_of(events: &[crate::provider::Event], at: i64) -> Vec<crate::provider::NowNext> {
    let mut out: Vec<crate::provider::NowNext> = Vec::new();
    for e in events {
        let slot = match out.last_mut() {
            Some(n) if n.channel == e.channel => n,
            _ => {
                out.push(crate::provider::NowNext { channel: e.channel, now: None, next: None });
                out.last_mut().expect("just pushed")
            }
        };
        if e.start <= at && at < e.end && slot.now.is_none() {
            slot.now = Some(e.clone());
        } else if e.start >= at && slot.next.is_none() {
            slot.next = Some(e.clone());
        }
    }
    out
}

const LIVE_TV: &str = "org.droidtv.zapster";

impl crate::provider::Provider for Freeview {
    fn id(&self) -> &'static str {
        "freeview"
    }
    fn name(&self) -> &'static str {
        "Live TV (over the air)"
    }
    fn hosts(&self) -> &'static [&'static str] {
        &["org.droidtv.zapster"]
    }
    fn guide(&self) -> Option<&dyn crate::provider::Guide> {
        Some(self)
    }
    fn channels(&self) -> Option<&dyn crate::provider::Channels> {
        Some(self)
    }
}

impl crate::provider::Guide for Freeview {
    fn now_next(&self, cx: &crate::provider::Cx, at: i64) -> Result<Vec<crate::provider::NowNext>> {
        // what's on, and anything starting in the next six hours
        let events: Vec<_> = guide_events(cx.tv)?.iter().filter(|e| e.end > at && e.start < at + 6 * 3600).cloned().collect();
        Ok(now_next_of(&events, at))
    }

    /// The guide's `presetid` is the channel's ccid.
    fn schedule(&self, cx: &crate::provider::Cx, channel: u32, from: i64, to: i64) -> Result<Vec<crate::provider::Event>> {
        Ok(guide_events(cx.tv)?.iter().filter(|e| e.channel == channel && e.end > from && e.start < to).cloned().collect())
    }
}

/// The channels in a JointSpace `channelLists/all` reply: broadcast ones
/// only (not the apps' "channels" or analogue inputs). The ccid is what the
/// broadcast guide calls `presetid`.
pub fn parse_channel_list(json: &str) -> Vec<crate::provider::Channel> {
    let Some(i) = json.find("\"Channel\"") else { return Vec::new() };
    let list = &json[i..];
    let Some(open) = list.find('[') else { return Vec::new() };
    let mut out: Vec<crate::provider::Channel> = crate::http::objects(&list[open..])
        .into_iter()
        .filter(|o| crate::http::string_field(o, "type").is_some_and(|t| t.starts_with("DVB")))
        .filter_map(|o| {
            Some(crate::provider::Channel {
                id: crate::http::number_field(o, "ccid")? as u32,
                number: crate::http::string_field(o, "preset"),
                name: crate::http::string_field(o, "name"),
                radio: crate::http::string_field(o, "serviceType").is_some_and(|t| t.contains("radio") || t == "audio"),
            })
        })
        .collect();
    out.sort_by_key(crate::provider::Channel::order);
    out
}

/// The channel in a `/6/activities/tv` reply:
/// `{"channel":{"ccid":143,"preset":"18","name":"More 4"},"channelList":{...}}`.
pub fn parse_current(json: &str) -> Option<crate::provider::Channel> {
    let ch = crate::http::objects(json.split_once("\"channel\"")?.1).into_iter().next()?;
    Some(crate::provider::Channel {
        id: crate::http::number_field(ch, "ccid")? as u32,
        number: crate::http::string_field(ch, "preset"),
        name: crate::http::string_field(ch, "name"),
        radio: false,
    })
}

fn paired_ok(tv: &Tv, path: &str, what: &str) -> Result<String> {
    match js_get_paired(tv, path, what)? {
        (200, body) => Ok(body),
        (401, _) => failed(format!("{what}: the TV refused the saved pairing (HTTP 401); if this keeps happening, pair again: 'tvbee pair jointspace'")),
        (code, _) => failed(format!("{what}: the TV answered HTTP {code} for {path}")),
    }
}

impl crate::provider::Channels for Freeview {
    /// From the TV's channel list (needs JointSpace pairing).
    fn list(&self, cx: &crate::provider::Cx) -> Result<Vec<crate::provider::Channel>> {
        let list = parse_channel_list(&paired_ok(cx.tv, "/6/channeldb/tv/channelLists/all", "the channel list")?);
        if list.is_empty() {
            return failed("the TV's channel list has no broadcast channels (tune them in the TV's settings)");
        }
        Ok(list)
    }

    /// What the TV app is tuned to (it stays tuned while other apps are in front).
    fn current(&self, cx: &crate::provider::Cx) -> Result<Option<crate::provider::Channel>> {
        Ok(parse_current(&paired_ok(cx.tv, "/6/activities/tv", "the current channel")?))
    }

    /// Type the channel number, as the remote does, with live TV in front
    /// first (in another app the digits would mean something else).
    fn tune(&self, cx: &crate::provider::Cx, channel: &crate::provider::Channel) -> Result<String> {
        let Some(number) = channel.number.as_deref().filter(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())) else {
            return failed(format!("watch: {} has no channel number to type", channel.label()));
        };
        cx.tv.connect()?;
        let front = super::info::focus_package(&cx.tv.window_dump()?);
        if front.as_deref() != Some(LIVE_TV) {
            super::apps::start(cx.tv, LIVE_TV)?;
            // give it time to come up and take keys
            std::thread::sleep(std::time::Duration::from_secs(3));
        }
        super::input::channel_keys(number).into_iter().try_for_each(|k| cx.tv.keyevent(k))?;
        Ok(format!("switching to {}", channel.label()))
    }
}

pub fn providers() -> Vec<&'static dyn crate::provider::Provider> {
    vec![&FREEVIEW]
}

pub fn commands() -> Vec<Command> {
    vec![Command::new(CAT, "jointspace", "[/6/<path>]", "read a path of Philips' TV API (JointSpace)", jointspace).long(
        "Reads a JointSpace path (default /6/system: model, API version, features).\n\
         Unpaired, only the public routes work (plain HTTP on :1925); others say\n\
         pairing is required. After 'tvbee pair jointspace' (the TV shows a PIN),\n\
         paths are read over HTTPS on :1926 with the saved key: the current channel\n\
         (/6/activities/tv), channel lists (/6/channeldb/tv) and more.",
    )]
}

#[cfg(test)]
mod guide_tests {
    use super::*;

    #[test]
    fn pairing_bits() {
        // the signature is base64 of a 40-char hex HMAC: 56 base64 chars
        let sig = pairing_signature(1_795_400_000, "1234");
        assert_eq!(sig.len(), 56);
        assert_eq!(crate::crypto::base64_decode(&sig).unwrap().len(), 40);
        assert_ne!(sig, pairing_signature(1_795_400_000, "1235"));
        // the shared secret decodes to 64 bytes
        assert_eq!(crate::crypto::base64_decode(PAIRING_SECRET).unwrap().len(), 64);
        assert_eq!(new_device_id().len(), 16);
        assert!(device_json("abc").contains(r#""id":"abc""#));
    }

    const OUT: &str = "Row: 0 eventname=The Make a Difference Show, presetid=12, starttime=100, endtime=200, genre=2
Row: 1 eventname=Dotun Adebayo, presetid=12, starttime=200, endtime=300, genre=NULL
Row: 2 eventname=News, Weather, and Sport, presetid=1, starttime=150, endtime=250, genre=4
Row: 3 eventname=NULL, presetid=NULL, starttime=NULL, endtime=NULL, genre=NULL
";

    #[test]
    fn reads_guide_rows() {
        let e = parse_events(OUT);
        assert_eq!(e.len(), 3);
        assert_eq!(e[2].title, "News, Weather, and Sport");
        assert_eq!((e[2].channel, e[2].start, e[2].end, e[2].genre), (1, 150, 250, Some(4)));
        assert_eq!(e[1].genre, None);
    }

    #[test]
    fn caches_guide_rows() {
        let e = parse_events(OUT);
        assert_eq!(parse_cached_events(&render_events(&e)), e);
    }

    #[test]
    fn reads_channel_lists() {
        let json = r#"{"version":1353,"id":"all","Channel":[
            {"ccid":3,"preset":"1-1","name":"Play Movies & TV","type":"analog","serviceType":"audio_video"},
            {"ccid":157,"preset":"3","name":"ITV1","onid":9018,"type":"DVB_T","serviceType":"audio_video"},
            {"ccid":159,"preset":"1","name":"BBC ONE Lon","type":"DVB_T","serviceType":"audio_video"},
            {"ccid":12,"preset":"722","name":"BBC Essex","type":"DVB_T","serviceType":"radio"}]}"#;
        let list = parse_channel_list(json);
        let labels: Vec<_> = list.iter().map(|c| c.label()).collect();
        assert_eq!(labels, ["1 BBC ONE Lon", "3 ITV1", "722 BBC Essex"]);
        assert_eq!((list[0].id, list[2].radio), (159, true));
        let now = parse_current(r#"{"channel":{"ccid":143,"preset":"18","name":"More 4"},"channelList":{"id":"allter","version":"1352"}}"#).unwrap();
        assert_eq!((now.id, now.number.as_deref(), now.name.as_deref()), (143, Some("18"), Some("More 4")));
    }

    #[test]
    fn works_out_now_and_next() {
        let mut e = parse_events(OUT);
        e.sort_by_key(|x| (x.channel, x.start));
        let nn = now_next_of(&e, 160);
        assert_eq!(nn.len(), 2);
        assert_eq!(nn[0].channel, 1);
        assert_eq!(nn[0].now.as_ref().map(|x| x.title.as_str()), Some("News, Weather, and Sport"));
        assert_eq!(nn[0].next, None);
        assert_eq!(nn[1].now.as_ref().map(|x| x.title.as_str()), Some("The Make a Difference Show"));
        assert_eq!(nn[1].next.as_ref().map(|x| x.title.as_str()), Some("Dotun Adebayo"));
    }
}

//! Philips Android TV extras, built on the TV's "JointSpace" REST service
//! (Restlet on ports 1925 http / 1926 https). Vendor-specific, so only
//! enabled when the Philips system service package is installed.

use crate::error::{failed, usage, Result};
use crate::actions::ActionDescriptor;
use crate::tv::Tv;
use crate::Ctx;

const CAT: &str = "philips";
const JOINTSPACE_PKG: &str = "org.droidtv.xtv";

pub fn detect(tv: &Tv) -> Result<bool> {
    let out = tv.shell(&["cmd", "package", "list", "packages", JOINTSPACE_PKG])?;
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

/// How tveee introduces itself to the TV.
fn device_json(id: &str) -> String {
    let os = if cfg!(windows) { "Windows" } else { "Linux" };
    format!(r#"{{"device_name":"tveee","device_os":"{os}","app_name":"tveee","type":"native","app_id":"app.id","id":"{id}"}}"#)
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

/// Whether the TV is awake (adb says so): then a refused JointSpace
/// connection is live TV restarting the API, worth waiting for.
pub fn awake(tv: &Tv) -> bool {
    tv.shell(&["dumpsys power | grep mWakefulness="]).is_ok_and(|s| s.contains("mWakefulness=Awake"))
}

/// Whether tveee is paired with this TV's JointSpace.
pub fn paired(tv: &Tv) -> bool {
    crate::pairing::load(PAIRING, &tv.ip).as_ref().and_then(credentials).is_some()
}

fn https_get(tv: &Tv, saved: &crate::pairing::Saved, path: &str) -> Result<(u16, String)> {
    let Some(digest) = credentials(saved) else { return Err(crate::pairing::required_error(PAIRING, path)) };
    crate::https::request(&crate::https::Request { url: &https_url(&tv.ip, path), body: None, digest: Some(digest), basic: None, retry: Some(&|| awake(tv)), timeout_secs: 10 })
}

/// GET a path that needs pairing; `what` names it in the "pairing required"
/// error ("the channel list").
pub fn js_get_paired(tv: &Tv, path: &str, what: &str) -> Result<(u16, String)> {
    let saved = crate::pairing::require(PAIRING, tv, what)?;
    https_get(tv, &saved, path)
}

/// POST to a path that needs pairing.
pub fn js_post(tv: &Tv, path: &str, body: &str, what: &str) -> Result<(u16, String)> {
    let saved = crate::pairing::require(PAIRING, tv, what)?;
    let Some(digest) = credentials(&saved) else { return Err(crate::pairing::required_error(PAIRING, what)) };
    crate::https::request(&crate::https::Request { url: &https_url(&tv.ip, path), body: Some(body), digest: Some(digest), basic: None, retry: Some(&|| awake(tv)), timeout_secs: 10 })
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

/// A JointSpace pairing under way: the TV is showing a PIN.
#[derive(Debug, Clone)]
pub struct PairStart {
    id: String,
    key: String,
    timestamp: i64,
}

/// Ask the TV to pair: it shows a PIN for about a minute (for [`pair_finish`]).
pub fn pair_start(tv: &Tv) -> Result<PairStart> {
    let id = new_device_id();
    let ask = format!(r#"{{"scope":["read","write","control"],"device":{}}}"#, device_json(&id));
    let (status, reply) =
        crate::https::request(&crate::https::Request { url: &https_url(&tv.ip, "/6/pair/request"), body: Some(&ask), digest: None, basic: None, retry: Some(&|| awake(tv)), timeout_secs: 15 })?;
    let v = crate::http::json(&reply).unwrap_or_default();
    let key = crate::http::text(&v, "auth_key");
    let timestamp = crate::http::number(&v, "timestamp").map(|t| t as i64);
    let (Some(key), Some(timestamp)) = (key, timestamp) else {
        return failed(format!("pair: the TV didn't start pairing (HTTP {status}): {}", reply.trim()));
    };
    Ok(PairStart { id, key, timestamp })
}

/// Finish pairing with the PIN the TV showed; returns what to save.
pub fn pair_finish(tv: &Tv, s: &PairStart, pin: &str) -> Result<crate::pairing::Saved> {
    let pin = pin.trim();
    let grant = format!(
        r#"{{"auth":{{"auth_AppId":"1","pin":"{pin}","auth_timestamp":{},"auth_signature":"{}"}},"device":{}}}"#,
        s.timestamp,
        pairing_signature(s.timestamp, pin),
        device_json(&s.id)
    );
    let (status, reply) = crate::https::request(&crate::https::Request {
        url: &https_url(&tv.ip, "/6/pair/grant"),
        body: Some(&grant),
        digest: Some((&s.id, &s.key)),
        basic: None, retry: Some(&|| awake(tv)), timeout_secs: 15,
    })?;
    let ok = status == 200 && crate::http::json(&reply).and_then(|v| crate::http::text(&v, "error_id")).is_none_or(|e| e == "SUCCESS");
    if !ok {
        return failed(format!("pair: the TV refused (HTTP {status}): {}", reply.trim()));
    }
    Ok(crate::pairing::Saved(vec![("user".into(), s.id.clone()), ("key".into(), s.key.clone())]))
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
        "channel names and numbers, the current channel, more of 'tveee jointspace'"
    }
    fn how(&self) -> &'static str {
        "the TV shows a 4-digit PIN for about a minute; type it in"
    }

    fn pair(&self, tv: &Tv) -> Result<crate::pairing::Saved> {
        println!("asking {} to pair...", tv.ip);
        let started = pair_start(tv)?;
        let pin = read_pin()?;
        pair_finish(tv, &started, &pin)
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
        Some("pair") => return usage("pairing moved: run 'tveee pair jointspace'"),
        Some(_) => return usage("usage: tveee jointspace [/6/<path>]"),
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
            "jointspace: the TV refused {path} (HTTP 401): it may not have that path; if every path fails, pair again ('tveee pair jointspace')"
        )),
        _ => {
            println!("{body}");
            failed(format!("jointspace: HTTP {status} for {path}"))
        }
    }
}

/// The browser the NetTV web apps (iPlayer, Arte, ...) run in.
pub const NETTV_BROWSER: &str = "org.droidtv.nettvbrowser";

/// The name of the NetTV app a launch link is for (the browser opens e.g.
/// `https://www.live.bbctvapps.co.uk/tap/iplayer?origin=portal`): one tveee
/// knows, else the link's host.
pub fn nettv_app_name(link: &str) -> String {
    const KNOWN: &[(&str, &str)] = &[("iplayer", "iPlayer"), ("arte", "Arte"), ("dailymotion", "Dailymotion")];
    let lower = link.to_lowercase();
    if let Some((_, name)) = KNOWN.iter().find(|(k, _)| lower.contains(k)) {
        return (*name).to_string();
    }
    let host = lower.split("://").nth(1).unwrap_or(&lower).split(['/', '?', ':']).next().unwrap_or_default();
    host.strip_prefix("www.").unwrap_or(host).to_string()
}

/// The NetTV app the browser is showing, from the link its activity was
/// started with (grepped on the TV: the whole dump is large).
pub fn nettv_app(tv: &Tv) -> Option<String> {
    let cmd = format!("dumpsys activity activities | grep -m1 -o 'dat=[^ ]* .*cmp={NETTV_BROWSER}/'");
    let out = tv.shell(&[&cmd]).ok()?;
    let link = out.split_whitespace().next()?.strip_prefix("dat=")?;
    Some(nettv_app_name(link))
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
        known("a NetTV app", NETTV_BROWSER, Kind::Video),
        app("livetv", "live TV", "org.droidtv.zapster", Kind::LiveTv),
    ]
}

/// Live TV over the air (DVB-T, e.g. Freeview), hosted by Philips' TV app
/// (zapster). The guide comes from the broadcast EPG the TV collects
/// (`org.droidtv.epgdvb`, readable without special permissions).
pub struct Freeview;
pub static FREEVIEW: Freeview = Freeview;

const GUIDE_URI: &str = "content://org.droidtv.epgdvb/antenna/scheduletable";
/// The free-text column last, so it can be split on the column names.
/// Descriptions (`shortinfo`) are read separately, in pages: with them, the
/// whole guide is more than one query can return (Android's 2MB cursor
/// window), and the TV quietly drops the rest.
const GUIDE_COLUMNS: &str = "presetid:starttime:endtime:genre:eventname";

/// Text from the TV without control characters (some guide titles are
/// padded with NULs, which take no room on screen but count as characters).
pub fn clean(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect::<String>().trim().to_string()
}

/// A description: control characters out, but its line breaks kept.
pub fn clean_info(s: &str) -> Option<String> {
    let s: String = s.chars().filter(|c| *c == '\n' || !c.is_control()).collect();
    let s = s.trim();
    (!s.is_empty() && s != "NULL").then(|| s.to_string())
}

/// The rows of `content query` output, a row's continuation lines (from
/// values with line breaks) joined back on.
pub fn query_rows(out: &str) -> Vec<String> {
    let mut rows: Vec<String> = Vec::new();
    for l in out.lines() {
        let l = l.strip_suffix('\r').unwrap_or(l);
        match (l.starts_with("Row: "), rows.last_mut()) {
            (true, _) => rows.push(l.to_string()),
            (false, Some(row)) => {
                row.push('\n');
                row.push_str(l);
            }
            (false, None) => {}
        }
    }
    rows
}

/// Rows of `content query --projection presetid:starttime:endtime:genre:eventname[:shortinfo]`:
/// `Row: 0 presetid=12, starttime=..., endtime=..., genre=2, eventname=..., shortinfo=...`.
/// Titles and descriptions can hold ", ", and descriptions line breaks (the
/// rest of a row then comes on the next lines).
pub fn parse_events(out: &str) -> Vec<crate::provider::Event> {
    query_rows(out)
        .iter()
        .filter_map(|row| {
            let rest = &row[row.find(" presetid=")? + " presetid=".len()..];
            let (preset, rest) = rest.split_once(", starttime=")?;
            let (start, rest) = rest.split_once(", endtime=")?;
            let (end, rest) = rest.split_once(", genre=")?;
            let (genre, rest) = rest.split_once(", eventname=")?;
            let (title, info) = rest.split_once(", shortinfo=").unwrap_or((rest, ""));
            if title == "NULL" {
                return None;
            }
            Some(crate::provider::Event {
                channel: preset.parse().ok()?,
                title: clean(title),
                start: start.parse().ok()?,
                end: end.parse().ok()?,
                genre: genre.parse().ok(),
                info: clean_info(info),
            })
        })
        .collect()
}

/// How long a copy of the guide is good for. The broadcast guide changes
/// slowly, and reading it takes the TV a few seconds.
const GUIDE_FRESH: std::time::Duration = std::time::Duration::from_secs(30 * 60);


/// Rows of `--projection presetid:starttime:shortinfo`: (channel, start, description).
pub fn parse_infos(out: &str) -> (usize, Vec<(u32, i64, String)>) {
    let rows = query_rows(out);
    let infos = rows
        .iter()
        .filter_map(|row| {
            let rest = &row[row.find(" presetid=")? + " presetid=".len()..];
            let (preset, rest) = rest.split_once(", starttime=")?;
            let (start, info) = rest.split_once(", shortinfo=")?;
            Some((preset.parse().ok()?, start.parse().ok()?, clean_info(info)?))
        })
        .collect();
    (rows.len(), infos)
}

type Infos = std::sync::Arc<std::collections::HashMap<(u32, i64), String>>;
static INFOS: std::sync::Mutex<Option<(String, std::time::Instant, Infos)>> = std::sync::Mutex::new(None);
/// Rows a page: about 1MB with descriptions, well inside the 2MB a query can return.
const INFO_PAGE: usize = 4000;

/// How long ago `what` was read from the TV into the cache, if it was.
fn cache_age(c: &mut diesel::sqlite::SqliteConnection, tv: &Tv, what: &str) -> Option<std::time::Duration> {
    let at = crate::store::fetched_at(c, &tv.ip, &guide_source(tv), what).ok()??;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
    Some(std::time::Duration::from_secs(now.saturating_sub(at).max(0) as u64))
}

/// Every description in the guide: from memory, else the cache, else the
/// TV, a page at a time (ordered and paged through the sort clause, which
/// the TV passes to its database as it is).
fn guide_infos(tv: &Tv) -> Result<Infos> {
    let mut mem = INFOS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((_, _, infos)) = mem.as_ref().filter(|(ip, at, _)| *ip == tv.ip && at.elapsed() < GUIDE_FRESH) {
        return Ok(infos.clone());
    }
    // the cache is a help, never a requirement
    let mut db = crate::store::open().ok();
    let cached = db.as_mut().and_then(|c| {
        let age = cache_age(c, tv, "descriptions").filter(|a| *a < GUIDE_FRESH)?;
        Some((crate::store::load_descriptions(c, &tv.ip, &guide_source(tv)).ok()?, std::time::Instant::now() - age))
    });
    let (map, at) = match cached {
        Some(c) => c,
        None => {
            let mut all = Vec::new();
            for page in 0.. {
                let cmd = format!(
                    "content query --uri {GUIDE_URI} --projection presetid:starttime:shortinfo --sort 'presetid, starttime LIMIT {INFO_PAGE} OFFSET {}'",
                    page * INFO_PAGE
                );
                let (rows, infos) = parse_infos(&tv.shell(&[&cmd])?);
                all.extend(infos);
                if rows < INFO_PAGE {
                    break;
                }
            }
            if let Some(c) = db.as_mut() {
                let _ = crate::store::save_descriptions(c, &tv.ip, &guide_source(tv), &all);
            }
            (all.into_iter().map(|(ch, start, info)| ((ch, start), info)).collect(), std::time::Instant::now())
        }
    };
    let infos: Infos = std::sync::Arc::new(map);
    *mem = Some((tv.ip.clone(), at, infos.clone()));
    Ok(infos)
}

type Guide = std::sync::Arc<Vec<crate::provider::Event>>;
static GUIDE: std::sync::Mutex<Option<(String, std::time::Instant, Guide)>> = std::sync::Mutex::new(None);

/// The whole guide, sorted by channel then start: from memory, else the
/// cache, else the TV (which ignores filters, so it's all or nothing).
fn guide_events(tv: &Tv) -> Result<Guide> {
    let mut mem = GUIDE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((_, _, events)) = mem.as_ref().filter(|(ip, at, _)| *ip == tv.ip && at.elapsed() < GUIDE_FRESH) {
        return Ok(events.clone());
    }
    let mut db = crate::store::open().ok();
    let cached = db.as_mut().and_then(|c| {
        let age = cache_age(c, tv, "events").filter(|a| *a < GUIDE_FRESH)?;
        let events = crate::store::load_events(c, &tv.ip, &guide_source(tv)).ok().filter(|e| !e.is_empty())?;
        Some((events, std::time::Instant::now() - age))
    });
    let (events, at) = match cached {
        Some(c) => c,
        None => {
            let cmd = format!("content query --uri {GUIDE_URI} --projection {GUIDE_COLUMNS}");
            let mut events = parse_events(&tv.shell(&[&cmd])?);
            events.sort_by_key(|e| (e.channel, e.start));
            if let Some(c) = db.as_mut() {
                let _ = crate::store::save_events(c, &tv.ip, &guide_source(tv), &events);
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

    fn info(&self, cx: &crate::provider::Cx, channel: u32, start: i64) -> Result<Option<String>> {
        Ok(guide_infos(cx.tv)?.get(&(channel, start)).cloned())
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
    use crate::http::{number, text};
    let Some(v) = crate::http::json(json) else { return Vec::new() };
    let Some(list) = crate::http::find(&v, "Channel").and_then(|l| l.as_array()) else { return Vec::new() };
    let mut out: Vec<crate::provider::Channel> = list
        .iter()
        .filter(|o| text(o, "type").is_some_and(|t| t.starts_with("DVB")))
        .filter_map(|o| {
            Some(crate::provider::Channel {
                id: number(o, "ccid")? as u32,
                number: text(o, "preset"),
                name: text(o, "name").map(|n| clean(&n)),
                radio: text(o, "serviceType").is_some_and(|t| t.contains("radio") || t == "audio"),
                favourite: false,
                delivery: text(o, "type").map(|t| crate::provider::Delivery::parse(&t)),
                dvb: match (number(o, "onid"), number(o, "tsid"), number(o, "sid")) {
                    (Some(on), Some(ts), Some(s)) => Some((on as u16, ts as u16, s as u16)),
                    _ => None,
                },
            })
        })
        .collect();
    out.sort_by_key(crate::provider::Channel::order);
    out
}

/// The channel in a `/6/activities/tv` reply:
/// `{"channel":{"ccid":143,"preset":"18","name":"More 4"},"channelList":{...}}`.
pub fn parse_current(json: &str) -> Option<crate::provider::Channel> {
    let v = crate::http::json(json)?;
    let ch = v.get("channel")?;
    Some(crate::provider::Channel {
        id: crate::http::number(ch, "ccid")? as u32,
        number: crate::http::text(ch, "preset"),
        name: crate::http::text(ch, "name"),
        ..Default::default()
    })
}

/// The country the TV was set up for, from a `channelLists/all` reply.
pub fn parse_list_country(json: &str) -> Option<String> {
    let v = crate::http::json(json)?;
    crate::http::find(&v, "installCountry")?.as_str().filter(|c| !c.is_empty()).map(str::to_string)
}

/// The network the last channel list read showed, per TV.
static NETWORK: std::sync::Mutex<Option<(String, crate::provider::Network)>> = std::sync::Mutex::new(None);

fn remembered_network(tv: &Tv) -> Option<crate::provider::Network> {
    let n = NETWORK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    n.as_ref().filter(|(ip, _)| *ip == tv.ip).map(|(_, net)| net.clone())
}

/// Where this guide's data comes from, for the cache: the TV's broadcast
/// guide for its aerial, and the DVB network the channel list shows
/// (`broadcast:antenna:9018` is Freeview's). Paired, the list is read for
/// that if it hasn't been yet; else it's just `broadcast:antenna`.
fn guide_source(tv: &Tv) -> String {
    let net = remembered_network(tv).or_else(|| paired(tv).then(|| crate::provider::Channels::network(&FREEVIEW, &crate::provider::Cx { tv })).flatten());
    match net.and_then(|n| n.onid) {
        Some(onid) => format!("broadcast:antenna:{onid}"),
        None => "broadcast:antenna".into(),
    }
}

fn paired_ok(tv: &Tv, path: &str, what: &str) -> Result<String> {
    match js_get_paired(tv, path, what)? {
        (200, body) => Ok(body),
        (401, _) => failed(format!("{what}: the TV refused the saved pairing (HTTP 401); if this keeps happening, pair again: 'tveee pair jointspace'")),
        (code, _) => failed(format!("{what}: the TV answered HTTP {code} for {path}")),
    }
}

/// The user's favourite lists in a `/6/channeldb/tv` reply that have
/// anything in them: the modifiable ones ("Favourites 1".."8") whose version
/// isn't "0" (never edited).
pub fn used_favourite_lists(json: &str) -> Vec<String> {
    let Some(v) = crate::http::json(json) else { return Vec::new() };
    let Some(lists) = crate::http::find(&v, "favoriteLists").and_then(|l| l.as_array()) else { return Vec::new() };
    lists
        .iter()
        .filter(|o| o.get("modifiable").and_then(serde_json::Value::as_bool).unwrap_or(false))
        .filter(|o| crate::http::text(o, "version").is_none_or(|v| v != "0"))
        .filter_map(|o| crate::http::text(o, "id"))
        .collect()
}

/// The ccids in a favourite list reply: `{"channels":[{"ccid":159,"preset":"1"},...]}`.
pub fn favourite_ccids(json: &str) -> Vec<u32> {
    let Some(v) = crate::http::json(json) else { return Vec::new() };
    let Some(list) = crate::http::find(&v, "channels").and_then(|l| l.as_array()) else { return Vec::new() };
    list.iter().filter_map(|o| crate::http::number(o, "ccid")).map(|c| c as u32).collect()
}

/// The user's favourite lists that have anything in them, with their channels.
fn favourite_lists(tv: &Tv) -> Result<Vec<(String, Vec<u32>)>> {
    used_favourite_lists(&paired_ok(tv, "/6/channeldb/tv", "the favourite lists")?)
        .into_iter()
        .map(|id| {
            let ccids = favourite_ccids(&paired_ok(tv, &format!("/6/channeldb/tv/favoriteLists/{id}"), "a favourite list")?);
            Ok((id, ccids))
        })
        .collect()
}

/// Every channel in any of the user's favourite lists.
fn tv_favourites(tv: &Tv) -> Result<std::collections::BTreeSet<u32>> {
    Ok(favourite_lists(tv)?.into_iter().flat_map(|(_, c)| c).collect())
}

/// The list new favourites go in ("Favourites 1").
const FAVOURITE_LIST: &str = "1";

/// Add (or remove) a channel in favourite list `list`. Ok(false): not now.
/// The TV only edits favourites while it's showing live TV (else HTTP 503),
/// and for a few seconds after live TV comes up it still says 503: so with
/// live TV in front, keep trying a while.
fn modify_favourite(tv: &Tv, list: &str, ccid: u32, add: bool) -> Result<bool> {
    const LIVE_TV: &str = "org.droidtv.zapster";
    if super::info::focus_package(&tv.window_dump()?).as_deref() != Some(LIVE_TV) {
        return Ok(false);
    }
    let body = format!(r#"{{"{}":{{"id":[{ccid}]}}}}"#, if add { "add" } else { "remove" });
    let start = std::time::Instant::now();
    loop {
        match js_post(tv, &format!("/6/channeldb/tv/modifyfavourite/{list}"), &body, "changing favourites")? {
            (200, _) => return Ok(true),
            (503, _) if start.elapsed() < std::time::Duration::from_secs(8) => std::thread::sleep(std::time::Duration::from_secs(1)),
            (503, _) => return Ok(false),
            (code, reply) => return failed(format!("favourites: the TV refused (HTTP {code}): {}", reply.trim())),
        }
    }
}

impl crate::provider::Channels for Freeview {
    /// From the TV's channel list (needs JointSpace pairing); notes the
    /// network it shows, for [`Channels::network`].
    fn list(&self, cx: &crate::provider::Cx) -> Result<Vec<crate::provider::Channel>> {
        let json = paired_ok(cx.tv, "/6/channeldb/tv/channelLists/all", "the channel list")?;
        let list = parse_channel_list(&json);
        if list.is_empty() {
            return failed("the TV's channel list has no broadcast channels (tune them in the TV's settings)");
        }
        let net = crate::provider::Network::of(&list, parse_list_country(&json));
        *NETWORK.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some((cx.tv.ip.clone(), net));
        Ok(list)
    }

    /// From the channel list (read now if it hasn't been yet).
    fn network(&self, cx: &crate::provider::Cx) -> Option<crate::provider::Network> {
        remembered_network(cx.tv).or_else(|| {
            self.list(cx).ok()?;
            remembered_network(cx.tv)
        })
    }

    /// Every channel in any of the TV's favourite lists.
    fn favourites(&self, cx: &crate::provider::Cx) -> Option<Result<std::collections::BTreeSet<u32>>> {
        Some(tv_favourites(cx.tv))
    }

    /// What the TV app is tuned to (it stays tuned while other apps are in front).
    fn current(&self, cx: &crate::provider::Cx) -> Result<Option<crate::provider::Channel>> {
        Ok(parse_current(&paired_ok(cx.tv, "/6/activities/tv", "the current channel")?))
    }

    /// Adding puts it in Favourites 1; removing takes it out of every list
    /// it's in, so it stops being a favourite.
    fn write_favourite(&self, cx: &crate::provider::Cx, id: u32, on: bool) -> Result<bool> {
        if on {
            return modify_favourite(cx.tv, FAVOURITE_LIST, id, true);
        }
        for (list, ccids) in favourite_lists(cx.tv)? {
            if ccids.contains(&id) && !modify_favourite(cx.tv, &list, id, false)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Through JointSpace: the TV brings live TV to the front on that
    /// channel, whatever app was in front (typing the digits instead would
    /// go to that app, and loses the first digit while live TV starts).
    fn tune(&self, cx: &crate::provider::Cx, channel: &crate::provider::Channel) -> Result<String> {
        // the channel list the TV is using, as it reports it
        let now = paired_ok(cx.tv, "/6/activities/tv", "switching channel")?;
        let list = crate::http::json(&now).and_then(|v| v.get("channelList").map(ToString::to_string)).unwrap_or_else(|| r#"{"id":"allter"}"#.into());
        let body = tune_body(channel, &list);
        match js_post(cx.tv, "/6/activities/tv", &body, "switching channel")? {
            (200, _) => Ok(format!("switching to {}", channel.label())),
            (code, reply) => failed(format!("watch: the TV refused (HTTP {code}): {}", reply.trim())),
        }
    }
}

/// The body for `POST /6/activities/tv`: the channel, and the channel list
/// (a JSON object) it's in.
pub fn tune_body(ch: &crate::provider::Channel, list: &str) -> String {
    let q = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        r#"{{"channel":{{"ccid":{},"preset":"{}","name":"{}"}},"channelList":{list}}}"#,
        ch.id,
        q(ch.number.as_deref().unwrap_or("")),
        q(ch.name.as_deref().unwrap_or(""))
    )
}

pub fn providers() -> Vec<&'static dyn crate::provider::Provider> {
    vec![&FREEVIEW]
}

pub fn actions() -> Vec<ActionDescriptor> {
    let mut v = vec![ActionDescriptor::terminal("philips.jointspace", CAT, "read a path of Philips' TV API (JointSpace)", jointspace).cli("jointspace", "[/6/<path>]", "read a path of Philips' TV API (JointSpace)").long(
        "Reads a JointSpace path (default /6/system: model, API version, features).\n\
         Unpaired, only the public routes work (plain HTTP on :1925); others say\n\
         pairing is required. After 'tveee pair jointspace' (the TV shows a PIN),\n\
         paths are read over HTTPS on :1926 with the saved key: the current channel\n\
         (/6/activities/tv), channel lists (/6/channeldb/tv) and more.",
    )];
    v.extend(super::ambilight::actions());
    v
}

#[cfg(test)]
mod guide_tests {
    use super::*;

    #[test]
    fn nettv_app_names() {
        assert_eq!(nettv_app_name("https://www.live.bbctvapps.co.uk/tap/iplayer?origin=portal"), "iPlayer");
        assert_eq!(nettv_app_name("https://www.arte.tv/hbbtv"), "Arte");
        assert_eq!(nettv_app_name("http://www.example.com:80/app?x=1"), "example.com");
    }

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

    const OUT: &str = "Row: 0 presetid=12, starttime=100, endtime=200, genre=2, eventname=The Make a Difference Show, shortinfo=Stories of people, places, and change.
Row: 1 presetid=12, starttime=200, endtime=300, genre=NULL, eventname=Dotun Adebayo, shortinfo=NULL
Row: 2 presetid=1, starttime=150, endtime=250, genre=4, eventname=News, Weather, and Sport, shortinfo=The headlines.
Then the weather.
Row: 3 presetid=NULL, starttime=NULL, endtime=NULL, genre=NULL, eventname=NULL, shortinfo=NULL
";

    #[test]
    fn reads_guide_rows() {
        let e = parse_events(OUT);
        assert_eq!(e.len(), 3);
        assert_eq!(e[2].title, "News, Weather, and Sport");
        assert_eq!((e[2].channel, e[2].start, e[2].end, e[2].genre), (1, 150, 250, Some(4)));
        assert_eq!(e[1].genre, None);
        assert_eq!(e[0].info.as_deref(), Some("Stories of people, places, and change."));
        assert_eq!(e[1].info, None);
        // a description over two lines
        assert_eq!(e[2].info.as_deref(), Some("The headlines.\nThen the weather."));
    }

    #[test]
    fn builds_tune_requests() {
        let ch = crate::provider::Channel { id: 143, number: Some("18".into()), name: Some("More \"4\"".into()), radio: false, favourite: false, delivery: None, dvb: None };
        assert_eq!(
            tune_body(&ch, r#"{"id":"allter","version":"1352"}"#),
            r#"{"channel":{"ccid":143,"preset":"18","name":"More \"4\""},"channelList":{"id":"allter","version":"1352"}}"#
        );
    }

    #[test]
    fn cleans_titles() {
        assert_eq!(clean("Pokémon the Movie...\u{0}\u{0}"), "Pokémon the Movie...");
        let e = parse_events("Row: 0 presetid=101, starttime=1, endtime=2, genre=NULL, eventname=Countryfile\u{0}, shortinfo=NULL\n");
        assert_eq!(e[0].title, "Countryfile");

    }

    #[test]
    fn reads_description_pages() {
        let out = "Row: 0 presetid=12, starttime=100, shortinfo=Stories.\r\nRow: 1 presetid=12, starttime=200, shortinfo=NULL\nRow: 2 presetid=1, starttime=150, shortinfo=Line one,\nline two.\n";
        let (rows, infos) = parse_infos(out);
        assert_eq!(rows, 3);
        assert_eq!(infos, [(12, 100, "Stories.".to_string()), (1, 150, "Line one,\nline two.".to_string())]);
    }


    #[test]
    fn reads_favourite_lists() {
        let overview = r#"{"channelLists":[{"id":"all","version":1353}],"favoriteLists":[
            {"id":"allter","version":"1352","modifiable":false},
            {"id":"1","version":"0","name":"Favourites  1","modifiable":true},
            {"id":"2","version":"7","name":"Favourites  2","modifiable":true}]}"#;
        assert_eq!(used_favourite_lists(overview), ["2"]);
        assert_eq!(favourite_ccids(r#"{"version":"7","id":"2","channels":[{"ccid":159,"preset":"1"},{"ccid":143,"preset":"18"}]}"#), [159, 143]);
        assert!(favourite_ccids(r#"{"id":"1","channels":[]}"#).is_empty());
    }

    #[test]
    fn reads_channel_lists() {
        let json = r#"{"version":1353,"id":"all","Channel":[
            {"ccid":3,"preset":"1-1","name":"Play Movies & TV","type":"analog","serviceType":"audio_video"},
            {"ccid":157,"preset":"3","name":"ITV1","onid":9018,"tsid":8197,"sid":8261,"type":"DVB_T","serviceType":"audio_video"},
            {"ccid":159,"preset":"1","name":"BBC ONE Lon","type":"DVB_T","serviceType":"audio_video"},
            {"ccid":12,"preset":"722","name":"BBC Essex","type":"DVB_T","serviceType":"radio"}]}"#;
        let list = parse_channel_list(json);
        assert_eq!((list[1].delivery, list[1].dvb), (Some(crate::provider::Delivery::DvbT), Some((9018, 8197, 8261))));
        assert_eq!(parse_list_country(r#"{"version":1353,"installCountry":"United Kingdom","Channel":[]}"#).as_deref(), Some("United Kingdom"));
        let net = crate::provider::Network::of(&list, Some("United Kingdom".into()));
        assert_eq!((net.onid, net.deliveries), (Some(9018), vec![crate::provider::Delivery::DvbT]));
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

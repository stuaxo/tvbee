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
        401 => failed(format!("jointspace: the TV rejected the saved pairing for {path}; pair again: 'tvbee pair jointspace'")),
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
                preset: preset.parse().ok()?,
                title: title.to_string(),
                start: start.parse().ok()?,
                end: end.parse().ok()?,
                genre: genre.parse().ok(),
            })
        })
        .collect()
}

fn guide_query(tv: &Tv, filter: &str) -> Result<Vec<crate::provider::Event>> {
    tv.connect()?;
    let cmd = format!("content query --uri {GUIDE_URI} --projection {GUIDE_COLUMNS} --where '{filter}'");
    let mut events = parse_events(&tv.shell(&[&cmd])?);
    events.sort_by_key(|e| (e.preset, e.start));
    Ok(events)
}

/// Now and next per channel from a channel-sorted, time-sorted event list.
pub fn now_next_of(events: &[crate::provider::Event], at: i64) -> Vec<crate::provider::NowNext> {
    let mut out: Vec<crate::provider::NowNext> = Vec::new();
    for e in events {
        let slot = match out.last_mut() {
            Some(n) if n.preset == e.preset => n,
            _ => {
                out.push(crate::provider::NowNext { preset: e.preset, now: None, next: None });
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
        let events = guide_query(cx.tv, &format!("endtime>{at} AND starttime<{}", at + 6 * 3600))?;
        Ok(now_next_of(&events, at))
    }

    fn schedule(&self, cx: &crate::provider::Cx, preset: u32, from: i64, to: i64) -> Result<Vec<crate::provider::Event>> {
        guide_query(cx.tv, &format!("presetid={preset} AND endtime>{from} AND starttime<{to}"))
    }
}

impl crate::provider::Channels for Freeview {
    /// The channels with something in the guide in the next hour.
    fn list(&self, cx: &crate::provider::Cx) -> Result<Vec<crate::provider::Channel>> {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
        let mut presets: Vec<u32> = guide_query(cx.tv, &format!("endtime>{now} AND starttime<{}", now + 3600))?.iter().map(|e| e.preset).collect();
        presets.dedup();
        Ok(presets.into_iter().map(|preset| crate::provider::Channel { preset, name: None }).collect())
    }

    /// Not yet: the guide's preset numbers are the TV's internal service
    /// numbers, not the channel numbers typed on the remote, so typing one
    /// would land on the wrong channel. Needs the preset -> channel mapping
    /// (JointSpace's channel list, after pairing).
    fn tune(&self, _cx: &crate::provider::Cx, preset: u32) -> Result<String> {
        failed(format!(
            "watch: can't switch to preset {preset} yet: the guide's presets are the TV's internal numbers, not \
             channel numbers. Use 'tvbee channel <number>' with the number from the TV's own channel list."
        ))
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
        assert_eq!((e[2].preset, e[2].start, e[2].end, e[2].genre), (1, 150, 250, Some(4)));
        assert_eq!(e[1].genre, None);
    }

    #[test]
    fn works_out_now_and_next() {
        let mut e = parse_events(OUT);
        e.sort_by_key(|x| (x.preset, x.start));
        let nn = now_next_of(&e, 160);
        assert_eq!(nn.len(), 2);
        assert_eq!(nn[0].preset, 1);
        assert_eq!(nn[0].now.as_ref().map(|x| x.title.as_str()), Some("News, Weather, and Sport"));
        assert_eq!(nn[0].next, None);
        assert_eq!(nn[1].now.as_ref().map(|x| x.title.as_str()), Some("The Make a Difference Show"));
        assert_eq!(nn[1].next.as_ref().map(|x| x.title.as_str()), Some("Dotun Adebayo"));
    }
}

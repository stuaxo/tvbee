//! Finding the TV on the network, and checking that tvbee's setup works.

use crate::error::{failed, usage, Result};
use crate::mdns;
use crate::registry::Command;
use crate::tv;
use crate::Ctx;
use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

const CAT: &str = "diagnose";
const ADB_PORT: u16 = 5555;

/// The few service types an Android / Google TV advertises that matter here.
const KINDS: &[&str] = &[
    // Cast (most Android TVs): friendly name, model, the app casting
    "_googlecast._tcp.local",
    // the Android TV Remote service (newer TVs)
    "_androidtvremote2._tcp.local",
    // ADB itself: Android 11+ wireless debugging, and some TVs' adb
    "_adb-tls-connect._tcp.local",
    "_adb._tcp.local",
];

/// One device that answered, with what it said about itself.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Found {
    pub ip: Option<Ipv4Addr>,
    pub name: String,
    pub model: Option<String>,
    /// Short names of what it advertises: "cast", "tv-remote", "adb-wifi", "adb".
    pub offers: Vec<&'static str>,
    /// The app casting right now (Cast's "rs" field), e.g. "YouTube".
    pub casting: Option<String>,
}

fn short(kind: &str) -> &'static str {
    match kind.trim_end_matches(".local") {
        "_googlecast._tcp" => "cast",
        "_androidtvremote2._tcp" => "tv-remote",
        "_adb-tls-connect._tcp" => "adb-wifi",
        "_adb._tcp" => "adb",
        _ => "?",
    }
}

/// Group services by address into devices. Cast's TXT gives the friendly
/// name (`fn`) and model (`md`); otherwise the instance name is used.
/// How much a service's name/model should be trusted for its device: the
/// TV's own Cast receiver is on the standard port 8009; apps that imitate
/// Cast (AirReceiver) use other ports and add an `rmodel` field.
fn trust(s: &mdns::Service) -> u8 {
    match (short(&s.kind), s.port, s.txt.contains_key("rmodel")) {
        ("cast", Some(8009), false) => 3,
        ("cast", _, false) => 2,
        ("cast", _, true) => 1,
        _ => 0,
    }
}

/// Group services by address into devices. Cast's TXT gives the friendly
/// name (`fn`) and model (`md`); otherwise the instance name is used.
pub fn devices(services: &[mdns::Service]) -> Vec<Found> {
    let mut by_ip: BTreeMap<Option<Ipv4Addr>, Found> = BTreeMap::new();
    // most trusted first, so imitations can't overwrite the real name
    let mut services: Vec<&mdns::Service> = services.iter().collect();
    services.sort_by_key(|s| std::cmp::Reverse(trust(s)));
    let mut fallback: BTreeMap<Option<Ipv4Addr>, String> = BTreeMap::new();
    for s in services {
        fallback.entry(s.ip).or_insert_with(|| s.instance.split('.').next().unwrap_or(&s.instance).to_string());
        let d = by_ip.entry(s.ip).or_insert_with(|| Found { ip: s.ip, ..Default::default() });
        let tag = short(&s.kind);
        if !d.offers.contains(&tag) {
            d.offers.push(tag);
        }
        // the first (most trusted) source of each field wins
        if d.name.is_empty() {
            d.name = s.txt.get("fn").filter(|n| !n.is_empty()).cloned().unwrap_or_default();
        }
        if d.model.is_none() {
            d.model = s.txt.get("md").filter(|m| !m.is_empty()).cloned();
        }
        if d.casting.is_none() {
            d.casting = s.txt.get("rs").filter(|a| !a.is_empty()).cloned();
        }
    }
    // nothing gave a friendly name: use the most trusted instance name
    for d in by_ip.values_mut().filter(|d| d.name.is_empty()) {
        d.name = fallback.get(&d.ip).cloned().unwrap_or_default();
    }
    by_ip.into_values().collect()
}

/// Whether something accepts TCP connections on `ip:port`.
pub fn port_open(ip: IpAddr, port: u16, timeout: Duration) -> bool {
    TcpStream::connect_timeout(&SocketAddr::new(ip, port), timeout).is_ok()
}

/// `adb devices` -> {"192.168.1.19:5555": "device"}.
pub fn parse_adb_devices(out: &str) -> BTreeMap<String, String> {
    out.lines()
        .skip_while(|l| !l.starts_with("List of devices"))
        .skip(1)
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            Some((f.next()?.to_string(), f.next()?.to_string()))
        })
        .collect()
}

/// What adb already knows about each device (without connecting to any).
fn adb_states() -> BTreeMap<String, String> {
    let Some(adb) = tv::find_adb(|k| std::env::var_os(k)) else { return BTreeMap::new() };
    std::process::Command::new(adb.path)
        .arg("devices")
        .output()
        .map(|o| parse_adb_devices(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default()
}

/// This machine's address on the LAN (the one it would use to reach `towards`).
fn local_ip() -> Option<Ipv4Addr> {
    let s = std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    // connect() on UDP sends nothing; it just picks the route
    s.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).ok()?;
    match s.local_addr().ok()?.ip() {
        IpAddr::V4(v4) => Some(v4),
        IpAddr::V6(_) => None,
    }
}

/// Every address on this machine's /24 with the adb port open.
fn scan(me: Ipv4Addr) -> Vec<Ipv4Addr> {
    let [a, b, c, _] = me.octets();
    let mut hits: Vec<Ipv4Addr> = Vec::new();
    // 254 short connects, 64 at a time
    let all: Vec<Ipv4Addr> = (1..=254).map(|d| Ipv4Addr::new(a, b, c, d)).filter(|ip| *ip != me).collect();
    for chunk in all.chunks(64) {
        let open: Vec<Ipv4Addr> = std::thread::scope(|sc| {
            let hs: Vec<_> = chunk
                .iter()
                .map(|ip| sc.spawn(move || port_open(IpAddr::V4(*ip), ADB_PORT, Duration::from_millis(400)).then_some(*ip)))
                .collect();
            hs.into_iter().filter_map(|h| h.join().ok().flatten()).collect()
        });
        hits.extend(open);
    }
    hits
}

fn findtv(c: &Ctx, args: &[String]) -> Result<()> {
    let (mut wait, mut do_scan, mut raw) = (2.0, false, false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--scan" => do_scan = true,
            "--raw" => raw = true,
            "--wait" => {
                wait = it
                    .next()
                    .and_then(|v| super::media::parse_duration(v))
                    .filter(|s| *s > 0.0)
                    .map_or_else(|| usage("findtv: --wait needs a duration, e.g. 5s"), Ok)?;
            }
            o => return usage(format!("findtv: unknown option '{o}'")),
        }
    }
    eprintln!("asking the network for Android / Google TVs ({wait}s)...");
    let services = mdns::browse(KINDS, Duration::from_secs_f64(wait))?;
    if raw {
        for s in &services {
            let ip = s.ip.map_or("?".into(), |i| i.to_string());
            let port = s.port.map_or("?".into(), |p| p.to_string());
            println!("{ip}:{port}  {}  [{}]", s.instance, s.kind);
            for (k, v) in &s.txt {
                println!("    {k}={v}");
            }
        }
        return Ok(());
    }
    let found = devices(&services);
    let mut found: Vec<Found> = found.into_iter().filter(|d| d.ip.is_some()).collect();
    if let Some(me) = local_ip().filter(|_| do_scan) {
        eprintln!("checking port {ADB_PORT} on {}.0/24...", me.octets()[..3].iter().map(u8::to_string).collect::<Vec<_>>().join("."));
        for ip in scan(me) {
            if !found.iter().any(|d| d.ip == Some(ip)) {
                found.push(Found { ip: Some(ip), name: "(no name: found by --scan)".into(), ..Default::default() });
            }
        }
    }
    if found.is_empty() {
        println!("no TVs answered.");
        println!("Is the TV on and on this network? Some TVs advertise nothing: try 'tvbee findtv --scan'.");
        return failed("findtv: nothing found");
    }
    let states = adb_states();
    let current = c.tv.ip.clone();
    for d in &found {
        let Some(ip) = d.ip else { continue };
        let adb = if port_open(IpAddr::V4(ip), ADB_PORT, Duration::from_millis(800)) {
            match states.get(&format!("{ip}:{ADB_PORT}")).map(String::as_str) {
                Some("device") => "adb connected".to_string(),
                Some("unauthorized") => "adb waiting: accept the prompt on the TV".to_string(),
                Some(other) => format!("adb {other}"),
                None => format!("adb port {ADB_PORT} open"),
            }
        } else {
            "adb off (turn on network debugging)".to_string()
        };
        let model = d.model.as_deref().map_or(String::new(), |m| format!(" ({m})"));
        let casting = d.casting.as_deref().map_or(String::new(), |a| format!(", casting {a}"));
        let mark = if ip.to_string() == current { "   <- TV_IP" } else { "" };
        println!("{ip:<15} {}{model}", d.name);
        println!("{:<15} offers: {}; {adb}{casting}{mark}", "", if d.offers.is_empty() { "-".into() } else { d.offers.join(", ") });
    }
    if !found.iter().any(|d| d.ip.map(|i| i.to_string()) == Some(current.clone())) {
        if let Some(ip) = found.first().and_then(|d| d.ip) {
            println!("\nTV_IP is {current}. To use {ip}, put TV_IP={ip} in ~/.config/tvbee/env (or ./.env).");
        }
    }
    Ok(())
}

/// One line of the diagnose report.
fn report(ok: Option<bool>, what: &str, detail: &str) {
    let mark = match ok {
        Some(true) => "ok ",
        Some(false) => "!! ",
        None => "-- ",
    };
    println!("{mark} {what:<10} {detail}");
}

/// First line of `prog --flag`'s output, e.g. a version.
fn first_line(prog: &std::path::Path, flag: &str) -> Option<String> {
    let out = std::process::Command::new(prog).arg(flag).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines().map(str::trim).find(|l| !l.is_empty()).map(str::to_string)
}

fn diagnose(c: &Ctx, _: &[String]) -> Result<()> {
    let mut problems = 0;
    let env = |k: &str| std::env::var_os(k);

    // adb
    let adb = tv::find_adb(env);
    match &adb {
        Some(f) => {
            let ver = first_line(&f.path, "version").unwrap_or_else(|| "version unknown".into());
            report(Some(true), "adb", &format!("{} (via {}) - {ver}", f.path.display(), f.how));
        }
        None => {
            problems += 1;
            let how = if cfg!(windows) {
                "install Android platform-tools and put its folder on PATH (or set ANDROID_HOME)"
            } else {
                "install it (Debian/Ubuntu: sudo apt install adb), or set ADB / ANDROID_HOME"
            };
            report(Some(false), "adb", &format!("not found in $ADB, PATH, $ANDROID_HOME or $ANDROID_SDK_ROOT; {how}"));
        }
    }

    // scrcpy (only needed for mirror)
    match tv::search_path("scrcpy", env("PATH").as_deref()) {
        Some(p) => {
            let ver = first_line(&p, "--version").unwrap_or_else(|| "version unknown".into());
            report(Some(true), "scrcpy", &format!("{} - {ver}", p.display()));
        }
        None => report(None, "scrcpy", "not found (only needed for 'tvbee mirror'; Debian/Ubuntu: sudo apt install scrcpy)"),
    }

    // yt-dlp (only for YouTube details)
    match tv::search_path("yt-dlp", env("PATH").as_deref()) {
        Some(p) => {
            let ver = first_line(&p, "--version").unwrap_or_else(|| "version unknown".into());
            report(Some(true), "yt-dlp", &format!("{} - {ver}", p.display()));
        }
        None => report(None, "yt-dlp", "not found (only needed for 'tvbee details' on YouTube videos)"),
    }

    // curl (only for Philips' HTTPS API: jointspace pairing)
    match tv::search_path("curl", env("PATH").as_deref()) {
        Some(p) => {
            let ver = first_line(&p, "--version").map_or_else(|| "version unknown".into(), |v| v.split_whitespace().take(2).collect::<Vec<_>>().join(" "));
            report(Some(true), "curl", &format!("{} - {ver}", p.display()));
        }
        None => report(None, "curl", "not found (only needed for 'tvbee pair jointspace' on Philips TVs)"),
    }

    // which TV, and where that setting came from
    let ip = c.tv.ip.clone();
    let from = match crate::dotenv::source_of("TV_IP") {
        Some(f) => format!("from {}", f.display()),
        None if env("TV_IP").is_some() => "from the environment".into(),
        None => "the built-in default; set TV_IP in .env or ~/.config/tvbee/env (try 'tvbee findtv')".to_string(),
    };
    report(Some(true), "TV_IP", &format!("{ip} ({from})"));
    for p in crate::pairing::all() {
        match crate::pairing::load(p.id(), &ip) {
            Some(_) => report(Some(true), p.id(), &format!("paired with {} ({})", p.name(), p.unlocks())),
            None => report(None, p.id(), &format!("not paired: 'tvbee pair {}' for {}", p.id(), p.unlocks())),
        }
    }

    // can we reach it?
    let addr: Option<IpAddr> = ip.parse().ok().or_else(|| {
        use std::net::ToSocketAddrs;
        (ip.as_str(), ADB_PORT).to_socket_addrs().ok()?.next().map(|a| a.ip())
    });
    let Some(addr) = addr else {
        report(Some(false), "network", &format!("can't resolve '{ip}'"));
        return failed("diagnose: problems found");
    };
    if !port_open(addr, ADB_PORT, Duration::from_secs(2)) {
        problems += 1;
        report(
            Some(false),
            "network",
            &format!("nothing on {ip}:{ADB_PORT}: is the TV on, on this network, with network debugging on? ('tvbee findtv' looks for it)"),
        );
        println!("\n{problems} problem(s) found");
        return failed("diagnose: problems found");
    }
    report(Some(true), "network", &format!("{ip}:{ADB_PORT} answers"));

    // adb's view of it
    if adb.is_some() {
        let _ = c.tv.connect();
        let state = std::process::Command::new(adb.as_ref().map(|f| f.path.clone()).unwrap_or_default())
            .arg("devices")
            .output()
            .ok()
            .and_then(|o| parse_adb_devices(&String::from_utf8_lossy(&o.stdout)).remove(&c.tv.serial()));
        match state.as_deref() {
            Some("device") => {
                let props = c
                    .tv
                    .shell(&["getprop ro.product.manufacturer; getprop ro.product.model; getprop ro.build.version.release"])
                    .unwrap_or_default();
                let p: Vec<&str> = props.lines().map(str::trim).collect();
                let about = match p.as_slice() {
                    [make, model, android, ..] => format!(" - {make} {model}, Android {android}"),
                    _ => String::new(),
                };
                report(Some(true), "adb link", &format!("connected{about}"));
            }
            Some("unauthorized") => {
                problems += 1;
                report(Some(false), "adb link", "not allowed yet: accept the 'Allow USB debugging?' prompt on the TV");
            }
            Some(other) => {
                problems += 1;
                report(Some(false), "adb link", &format!("{other} (try 'adb disconnect {}' and run this again)", c.tv.serial()));
            }
            None => {
                problems += 1;
                report(Some(false), "adb link", "adb couldn't connect");
            }
        }
    }

    // plugins and optional parts
    let plugins: Vec<String> = c.plugins.iter().filter(|p| !matches!(p.source, crate::plugin::Source::Builtin)).map(|p| format!("{} {}", p.name, if p.enabled { "on" } else { "off" })).collect();
    if !plugins.is_empty() {
        report(None, "plugins", &format!("{} (see 'tvbee plugins')", plugins.join(", ")));
    }
    report(None, "ui", if cfg!(feature = "ui") { "built in ('tvbee ui')" } else { "not in this build (cargo install --features ui)" });

    if problems > 0 {
        println!("\n{problems} problem(s) found");
        return failed("diagnose: problems found");
    }
    println!("\nall good");
    Ok(())
}

pub fn commands() -> Vec<Command> {
    vec![
        Command::new(CAT, "findtv", "[--scan] [--wait <secs>] [--raw]", "look for Android / Google TVs on the network", findtv).long(
            "Asks the local network (mDNS / DNS-SD) for the few services an Android or\n\
             Google TV advertises: Cast, the Android TV Remote service, and ADB. For each\n\
             TV it shows the name, model, what it offers, and whether adb can reach it.\n\
             \n\
             \x20 --scan          also try the adb port (5555) on every address in this\n\
             \x20                 machine's /24, for TVs that advertise nothing\n\
             \x20 --wait <secs>   how long to listen for answers (default 2s)\n\
             \x20 --raw           list every service as advertised, with its TXT fields",
        ),
        Command::new(CAT, "diagnose", "", "check adb, scrcpy, TV_IP and the connection to the TV", diagnose).long(
            "Checks, in order: that adb can be found ($ADB, PATH, $ANDROID_HOME,\n\
             $ANDROID_SDK_ROOT) and its version; scrcpy (for 'mirror'); which TV_IP is in\n\
             use and where it was set; that the TV answers on the adb port; and whether\n\
             adb is connected and allowed. Exits 1 if anything needs fixing.",
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn svc(kind: &str, inst: &str, ip: [u8; 4], txt: &[(&str, &str)]) -> mdns::Service {
        mdns::Service {
            kind: kind.into(),
            instance: inst.into(),
            port: Some(8009),
            txt: txt.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            ip: Some(Ipv4Addr::from(ip)),
        }
    }

    #[test]
    fn groups_services_into_tvs() {
        let tv = [192, 168, 1, 19];
        let found = devices(&[
            svc("_googlecast._tcp.local", "Chromecast-x._googlecast._tcp.local", tv, &[("fn", "Living Room TV"), ("md", "QM163E"), ("rs", "YouTube")]),
            svc("_androidtvremote2._tcp.local", "Living Room TV._androidtvremote2._tcp.local", tv, &[]),
            svc("_adb-tls-connect._tcp.local", "adb-ABC123._adb-tls-connect._tcp.local", [192, 168, 1, 50], &[]),
        ]);
        assert_eq!(found.len(), 2);
        let t = found.iter().find(|d| d.ip == Some(Ipv4Addr::from(tv))).unwrap();
        assert_eq!(t.name, "Living Room TV");
        assert_eq!(t.model.as_deref(), Some("QM163E"));
        assert_eq!(t.offers, vec!["cast", "tv-remote"]);
        assert_eq!(t.casting.as_deref(), Some("YouTube"));
        let other = found.iter().find(|d| d.ip == Some(Ipv4Addr::new(192, 168, 1, 50))).unwrap();
        assert_eq!((other.name.as_str(), other.offers.clone()), ("adb-ABC123", vec!["adb-wifi"]));
    }

    #[test]
    fn the_tvs_own_cast_beats_imitations() {
        let tv = [192, 168, 1, 19];
        let mut real = svc("_googlecast._tcp.local", "QM163E-fbaf._googlecast._tcp.local", tv, &[("fn", "Living Room TV"), ("md", "QM163E"), ("rs", "YouTube")]);
        real.port = Some(8009);
        let mut fake = svc("_googlecast._tcp.local", "QM163E-19[Cast]._googlecast._tcp.local", tv, &[("fn", "QM163E-19[Cast]"), ("md", "AirReceiver"), ("rmodel", "AirReceiver")]);
        fake.port = Some(8010);
        let adb = svc("_adb._tcp.local", "adb-00001c5a6be6abf9._adb._tcp.local", tv, &[]);
        // whatever order the answers arrive in
        for order in [vec![fake.clone(), real.clone(), adb.clone()], vec![adb.clone(), real.clone(), fake.clone()]] {
            let d = &devices(&order)[0];
            assert_eq!((d.name.as_str(), d.model.as_deref()), ("Living Room TV", Some("QM163E")), "{order:?}");
            assert_eq!(d.casting.as_deref(), Some("YouTube"));
        }
        // with only adb advertised, the instance name is used
        assert_eq!(devices(&[adb])[0].name, "adb-00001c5a6be6abf9");
    }

    #[test]
    fn reads_adb_devices() {
        let out = "* daemon started successfully\nList of devices attached\n192.168.1.19:5555\tdevice\nemulator-5554\tunauthorized\n\n";
        let m = parse_adb_devices(out);
        assert_eq!(m.get("192.168.1.19:5555").map(String::as_str), Some("device"));
        assert_eq!(m.get("emulator-5554").map(String::as_str), Some("unauthorized"));
        assert_eq!(m.len(), 2);
    }
}

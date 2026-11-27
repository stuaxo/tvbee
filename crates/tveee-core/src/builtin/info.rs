use crate::error::{failed, usage, Result};
use crate::nowplaying::{self, clock, Activity};
use crate::tv::Tv;
use crate::actions::ActionDescriptor;
use crate::Ctx;

const CAT: &str = "info";

fn focus_line(dump: &str) -> Option<&str> {
    dump.lines().find(|l| l.contains("mCurrentFocus")).map(str::trim)
}

/// The uid owning `pkg`'s windows (`mOwnerUid=10039 ... package=org.x`).
pub fn window_uid(dump: &str, pkg: &str) -> Option<u32> {
    dump.lines()
        .filter(|l| l.split_whitespace().any(|w| w.strip_prefix("package=") == Some(pkg)))
        .find_map(|l| l.split("mOwnerUid=").nth(1)?.split_whitespace().next()?.parse().ok())
}

/// `Window{eeb8591 u0 org.x/org.x.Activity}` -> `org.x/org.x.Activity`
pub fn focus_window(dump: &str) -> Option<String> {
    let line = focus_line(dump)?;
    let rest = &line[line.rfind(" u0 ")? + 4..];
    Some(rest.trim_end_matches('}').to_string())
}

pub fn focus_package(dump: &str) -> Option<String> {
    let win = focus_window(dump)?;
    let pkg = win.split(['/', ' ']).next()?;
    (!pkg.is_empty()).then(|| pkg.to_string())
}

/// Memory figures from `/proc/meminfo`, in kB.
#[derive(Debug, PartialEq, Clone, Copy, Default)]
pub struct Mem {
    pub total: u64,
    pub free: u64,
    pub available: u64,
    pub swap_total: u64,
    pub swap_free: u64,
}

impl Mem {
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.available)
    }

    pub fn swap_used(&self) -> u64 {
        self.swap_total.saturating_sub(self.swap_free)
    }
}

pub fn parse_meminfo(meminfo: &str) -> Option<Mem> {
    let kb = |key: &str| -> Option<u64> {
        let line = meminfo.lines().find(|l| l.starts_with(key))?;
        line.split_whitespace().nth(1)?.parse().ok()
    };
    let (total, free) = (kb("MemTotal:")?, kb("MemFree:")?);
    Some(Mem {
        total,
        free,
        available: kb("MemAvailable:").unwrap_or(free),
        swap_total: kb("SwapTotal:").unwrap_or(0),
        swap_free: kb("SwapFree:").unwrap_or(0),
    })
}

/// One-line RAM summary from `/proc/meminfo` (instant, unlike `dumpsys meminfo`
/// which takes ~8s on this TV).
pub fn ram_summary(meminfo: &str) -> Option<String> {
    let m = parse_meminfo(meminfo)?;
    let mb = |k: u64| k / 1024;
    Some(format!(
        "{} MB free, {} MB available of {} MB; swap used {} MB",
        mb(m.free),
        mb(m.available),
        mb(m.total),
        mb(m.swap_used())
    ))
}

/// One filesystem from `df -k`.
#[derive(Debug, PartialEq, Clone)]
pub struct Disk {
    pub mount: String,
    pub total_kb: u64,
    pub used_kb: u64,
    /// What apps can still use (less than total - used: some is reserved).
    pub avail_kb: u64,
}

/// `df -k` rows: `Filesystem 1K-blocks Used Available Use% Mounted on`.
pub fn parse_df(df: &str) -> Vec<Disk> {
    df.lines()
        .skip(1)
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            let n = f.len();
            if n < 6 {
                return None;
            }
            Some(Disk {
                mount: f[n - 1].to_string(),
                total_kb: f[n - 5].parse().ok()?,
                used_kb: f[n - 4].parse().ok()?,
                avail_kb: f[n - 3].parse().ok()?,
            })
        })
        .collect()
}

/// The storage worth showing: internal `/data`, and adopted (USB/SD) storage
/// under `/mnt/expand`, labelled.
pub fn interesting_disks(disks: &[Disk]) -> Vec<(&'static str, &Disk)> {
    disks
        .iter()
        .filter_map(|d| match d.mount.as_str() {
            "/data" => Some(("internal", d)),
            m if m.starts_with("/mnt/expand/") => Some(("adopted", d)),
            _ => None,
        })
        .collect()
}

/// `/proc/loadavg` -> the 1, 5 and 15 minute load averages.
pub fn parse_loadavg(s: &str) -> Option<(f64, f64, f64)> {
    let mut f = s.split_whitespace().map(str::parse::<f64>);
    Some((f.next()?.ok()?, f.next()?.ok()?, f.next()?.ok()?))
}

/// `3d 4h`, `5h 12m` or `7m`.
pub fn human_uptime(secs: f64) -> String {
    let t = secs.max(0.0) as u64;
    let (d, h, m) = (t / 86_400, t / 3600 % 24, t / 60 % 60);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m")
    }
}

/// Everything the resource figures need, in one adb round trip.
pub const SYSTEM_CMD: &str = "cat /proc/meminfo; echo @@; cat /proc/loadavg; echo @@; cat /proc/uptime; \
echo @@; df -k 2>/dev/null; echo @@; grep -c ^processor /proc/cpuinfo";

/// Memory, load, uptime and storage, from [`SYSTEM_CMD`].
#[derive(Debug, PartialEq, Clone, Default)]
pub struct System {
    pub mem: Option<Mem>,
    pub load: Option<(f64, f64, f64)>,
    pub uptime_secs: Option<f64>,
    pub disks: Vec<Disk>,
    pub cores: Option<u32>,
}

pub fn parse_system(out: &str) -> System {
    let parts: Vec<&str> = out.split("@@").collect();
    let part = |i: usize| parts.get(i).copied().unwrap_or("");
    System {
        mem: parse_meminfo(part(0)),
        load: parse_loadavg(part(1)),
        uptime_secs: part(2).split_whitespace().next().and_then(|v| v.parse().ok()),
        disks: parse_df(part(3).trim_start_matches('\n')),
        cores: part(4).trim().parse().ok(),
    }
}

pub fn read_system(tv: &crate::tv::Tv) -> Result<System> {
    Ok(parse_system(&tv.shell(&[SYSTEM_CMD])?))
}

/// kB as "78 MB" or "3.6 GB".
pub fn size(kb: u64) -> String {
    if kb >= 1024 * 1024 {
        format!("{:.1} GB", kb as f64 / (1024.0 * 1024.0))
    } else {
        format!("{} MB", kb / 1024)
    }
}

/// `internal 3.5/3.6 GB (98%, 78 MB free)`
pub fn disk_summary(label: &str, d: &Disk) -> String {
    let pct = (d.used_kb * 100).checked_div(d.total_kb).unwrap_or(0);
    format!(
        "{label} {}/{} ({pct}%, {} free)",
        size(d.used_kb).trim_end_matches(" GB").trim_end_matches(" MB"),
        size(d.total_kb),
        size(d.avail_kb)
    )
}

pub fn read_ram(tv: &crate::tv::Tv) -> Result<Option<String>> {
    Ok(ram_summary(&tv.shell(&["cat", "/proc/meminfo"])?))
}

fn now(c: &Ctx, _: &[String]) -> Result<()> {
    if let Some(l) = focus_line(&c.tv.window_dump()?) {
        println!("{l}");
    }
    Ok(())
}

/// `[--watch] [--interval <secs>]` -> watch interval in seconds, if watching.
pub fn parse_watch_args(name: &str, args: &[String]) -> Result<Option<f64>> {
    let (mut watch, mut every) = (false, 2.0);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--watch" | "-w" => watch = true,
            "--interval" | "-n" => {
                every = it
                    .next()
                    .and_then(|v| super::media::parse_duration(v))
                    .filter(|s| *s >= 0.5)
                    .map_or_else(|| usage(format!("{name}: --interval needs a duration of at least 0.5s")), Ok)?;
                watch = true;
            }
            _ => return usage(format!("usage: tveee {name} {WATCH_USAGE}")),
        }
    }
    Ok(watch.then_some(every))
}

const WATCH_USAGE: &str = "[--watch] [--interval <secs>]";

macro_rules! watch_long {
    () => {
        "  --watch, -w            keep the report on screen, refreshing it; q quits, r refreshes now\n  --interval, -n <secs>  seconds between refreshes (default 2; implies --watch)"
    };
}

/// Print `report` once, or keep it on screen with --watch.
fn run_report(c: &Ctx, name: &'static str, args: &[String], report: fn(&Tv) -> Result<String>) -> Result<()> {
    match parse_watch_args(name, args)? {
        None => {
            print!("{}", report(&c.tv)?);
            Ok(())
        }
        Some(every) => {
            let tv = Tv { ip: c.tv.ip.clone() };
            crate::watch::run(name, every, move || {
                report(&tv)
            })
        }
    }
}

fn status(c: &Ctx, args: &[String]) -> Result<()> {
    run_report(c, "status", args, status_text)
}

/// Two `dumpsys` queries side by side, to hide the TV's latency.
fn join2<A: Send, B: Send>(a: impl FnOnce() -> A + Send, b: impl FnOnce() -> B + Send) -> (A, B) {
    std::thread::scope(|sc| {
        let b = sc.spawn(b);
        let a = a();
        (a, b.join().expect("worker thread panicked"))
    })
}

/// What the TV is doing, from the power, window and media_session dumps.
/// `audio`: `dumpsys audio`, for apps that play without a media session.
fn activity_from(tv: &Tv, wake: Option<&str>, window: &str, media: &str, audio: &str) -> Option<Activity> {
    match (wake, focus_package(window)) {
        (Some(w), _) if w != "Awake" => {
            Some(Activity::just("In standby"))
        }
        (_, Some(pkg)) => {
            let sessions = super::media::parse_media_sessions(media);
            let players = super::media::parse_audio_players(audio);
            let sessions = super::media::with_audio_players(sessions, &pkg, window_uid(window, &pkg), &players);
            nowplaying::activity(tv, &pkg, &sessions)
        }
        _ => None,
    }
}

fn push_line(out: &mut String, label: &str, value: &str) {
    use std::fmt::Write;
    let _ = writeln!(out, "{:<10}{value}", format!("{label}:"));
}

/// The what / title / progress / url lines (those that are known).
fn push_activity(out: &mut String, a: &Activity) {
    push_line(out, "what", &a.what);
    for f in a.fields.iter().filter(|f| f.place == nowplaying::Place::Top) {
        push_line(out, f.label, &f.value);
    }
    if let Some(t) = &a.title {
        push_line(out, "title", t);
    }
    if let Some(p) = &a.progress {
        push_line(out, "progress", &nowplaying::progress_bar(p, 30));
    }
    if let Some(u) = &a.url {
        push_line(out, "url", u);
    }
    for f in a.fields.iter().filter(|f| f.place == nowplaying::Place::Bottom) {
        push_line(out, f.label, &f.value);
    }
}

/// The status report, one `label:   value` line each.
fn status_text(tv: &Tv) -> Result<String> {
    let ((power, window), ((audio, system), media)) = join2(
        || join2(|| tv.read_whole::<crate::readings::Power>(), || tv.read::<crate::readings::Window>()),
        || join2(|| join2(|| tv.shell(&["dumpsys", "audio"]), || read_system(tv)), || tv.shell(&["dumpsys", "media_session"])),
    );
    let (power, window, audio, system) = (power?, window?, audio?, system?);
    // media sessions only feed the optional what/title lines, so don't fail on them
    let media = media.unwrap_or_default();
    // power not said (asked twice): no power line, rather than "standby"
    let wake = power.map(|p| p.wakefulness);
    let activity = activity_from(tv, wake.as_deref(), &window.text, &media, &audio);

    let mut out = String::new();
    push_line(&mut out, "TV", &tv.ip);
    if let Some(a) = &activity {
        push_activity(&mut out, a);
    }
    if let Some(w) = &wake {
        push_line(&mut out, "power", w);
    }
    push_line(&mut out, "focus", window.focus.as_deref().unwrap_or_default());
    let muted = super::media::parse_muted(&audio).map_or_else(|| "unknown".into(), |m| m.to_string());
    if let Some((v, max)) = super::media::parse_volume(&audio) {
        push_line(&mut out, "volume", &format!("{v}/{max}"));
    }
    push_line(&mut out, "muted", &muted);
    if let Some(m) = system.mem {
        let mb = |k: u64| k / 1024;
        let ram = format!(
            "{} MB free, {} MB available of {} MB; swap used {} MB",
            mb(m.free),
            mb(m.available),
            mb(m.total),
            mb(m.swap_used())
        );
        push_line(&mut out, "ram", &ram);
    }
    let disks: Vec<String> = interesting_disks(&system.disks).iter().map(|(l, d)| disk_summary(l, d)).collect();
    if !disks.is_empty() {
        push_line(&mut out, "storage", &disks.join("; "));
    }
    if let Some((a, b, c)) = system.load {
        let cores = system.cores.map_or(String::new(), |n| format!(" ({n} cores)"));
        push_line(&mut out, "load", &format!("{a:.2} {b:.2} {c:.2}{cores}"));
    }
    if let Some(u) = system.uptime_secs {
        push_line(&mut out, "uptime", &human_uptime(u));
    }
    Ok(out)
}

const NOTHING: &str = "nothing playing that tveee recognises\n";

/// Just the what / title / progress / url lines.
fn nowplaying_text(tv: &Tv) -> Result<String> {
    let ((power, window), (media, audio)) = join2(
        || join2(|| tv.read_whole::<crate::readings::Power>(), || tv.read::<crate::readings::Window>()),
        || join2(|| tv.shell(&["dumpsys", "media_session"]), || tv.shell(&["dumpsys", "audio"])),
    );
    let (power, window, media) = (power?, window?, media?);
    // only for apps without a media session: don't fail on it
    let audio = audio.unwrap_or_default();
    let mut out = String::new();
    match activity_from(tv, power.map(|p| p.wakefulness).as_deref(), &window.text, &media, &audio) {
        Some(a) => push_activity(&mut out, &a),
        None => out.push_str(NOTHING),
    }
    Ok(out)
}

fn nowplaying_cmd(c: &Ctx, args: &[String]) -> Result<()> {
    // one-shot: say "nothing" on stderr and exit 1, so scripts can tell
    if parse_watch_args("nowplaying", args)?.is_none() {
        let text = nowplaying_text(&c.tv)?;
        if text == NOTHING {
            return failed(format!("nowplaying: {}", NOTHING.trim_end()));
        }
        print!("{text}");
        return Ok(());
    }
    run_report(c, "nowplaying", args, nowplaying_text)
}

/// What the TV reports about its clock.
#[derive(Debug, PartialEq)]
pub struct TvClock {
    pub epoch: i64,
    /// e.g. "+0100"
    pub offset: String,
    /// e.g. "2026-10-30 22:13:11 GMT"
    pub text: String,
    pub auto_time: Option<bool>,
    pub auto_zone: Option<bool>,
    pub zone: String,
}

fn flag(s: Option<&str>) -> Option<bool> {
    match s?.trim() {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    }
}

/// Parse the output of [`CLOCK_CMD`]: the date line, auto_time, auto_time_zone, timezone.
pub fn parse_clock(out: &str) -> Option<TvClock> {
    let mut lines = out.lines();
    let mut f = lines.next()?.split_whitespace();
    let epoch = f.next()?.parse().ok()?;
    let offset = f.next()?.to_string();
    let text = f.collect::<Vec<_>>().join(" ");
    Some(TvClock {
        epoch,
        offset,
        text,
        auto_time: flag(lines.next()),
        auto_zone: flag(lines.next()),
        zone: lines.next().unwrap_or_default().trim().to_string(),
    })
}

const CLOCK_CMD: &str = "date '+%s %z %F %T %Z'; settings get global auto_time; settings get global auto_time_zone; getprop persist.sys.timezone";

fn onoff(v: Option<bool>) -> &'static str {
    match v {
        Some(true) => "on",
        Some(false) => "off",
        None => "unknown",
    }
}

/// This PC's (epoch seconds, "+0100", "date text") from `date`.
/// This PC's (epoch seconds, "+0100", "date text"). The offset and text come
/// from `date`; where there's no `date` (Windows) they're empty and only the
/// clock difference is shown.
fn pc_clock() -> Result<(f64, String, String)> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64());
    let Ok(out) = std::process::Command::new("date").arg("+%z|%F %T %Z").output() else {
        return Ok((now, String::new(), String::new()));
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let (off, rest) = text.trim().split_once('|').unwrap_or(("", ""));
    Ok((now, off.to_string(), rest.to_string()))
}

fn time(c: &Ctx, _: &[String]) -> Result<()> {
    let before = pc_clock()?.0;
    let raw = c.tv.shell(&[CLOCK_CMD])?;
    let after = pc_clock()?;
    let Some(tv) = parse_clock(&raw) else {
        return failed("time: couldn't read the clock from the TV");
    };
    // compare against the middle of the request to cancel adb latency
    let pc_mid = (before + after.0) / 2.0;
    let diff = tv.epoch as f64 - pc_mid;

    println!("TV time:    {}", tv.text);
    println!("TV zone:    {} (UTC{})", if tv.zone.is_empty() { "unknown" } else { &tv.zone }, tv.offset);
    println!("auto time:  {}   auto zone: {}", onoff(tv.auto_time), onoff(tv.auto_zone));
    if after.1.is_empty() {
        println!("this PC:    (no 'date' command here, so only the difference is shown)");
    } else {
        println!("this PC:    {} (UTC{})", after.2, after.1);
    }
    println!("difference: TV is {diff:+.1}s vs this PC");
    if diff.abs() > 5.0 {
        println!("WARNING: the TV clock is off by more than 5s");
    }
    if !after.1.is_empty() && tv.offset != after.1 {
        println!("WARNING: the TV's UTC offset ({}) differs from this PC's ({}) - daylight saving or timezone mismatch?", tv.offset, after.1);
    }
    if tv.auto_time == Some(false) {
        println!("note: automatic date & time is off, so the TV won't correct itself");
    }
    Ok(())
}

fn logs(c: &Ctx, args: &[String]) -> Result<()> {
    match args.first() {
        None => print!("{}", c.tv.adb(&["logcat", "-d", "-v", "time", "-t", "200"])?),
        Some(pat) => {
            let text = c.tv.adb(&["logcat", "-d", "-v", "time"])?;
            let hits = super::tools::grep_i(&text, pat)?;
            let lines: Vec<&str> = hits.lines().collect();
            for l in &lines[lines.len().saturating_sub(200)..] {
                println!("{l}");
            }
        }
    }
    Ok(())
}

/// The app in front, its table entry, and the link to what it's showing.
fn current(tv: &Tv) -> Result<(String, Option<&'static super::apps::App>, Option<String>)> {
    let window = tv.window_dump()?;
    let Some(pkg) = focus_package(&window) else { return failed("can't tell which app is in front") };
    let app = super::apps::by_package(&pkg);
    // only apps whose player can say (YouTube) have a link to what's on
    let url = if crate::provider::for_package(&pkg).is_some_and(|p| p.player().is_some()) {
        let sessions = super::media::parse_media_sessions(&tv.shell(&["dumpsys", "media_session"]).unwrap_or_default());
        nowplaying::activity(tv, &pkg, &sessions).and_then(|a| a.url)
    } else {
        None
    };
    Ok((pkg, app, url))
}

fn details_cmd(c: &Ctx, args: &[String]) -> Result<()> {
    let full = match args.first().map(String::as_str) {
        None => false,
        Some("--full") => true,
        Some(x) => return usage(format!("details: unknown option '{x}' (try --full)")),
    };
    let (pkg, app, url) = current(&c.tv)?;
    let name = app.map_or(pkg.clone(), |a| super::apps::list_name(a.name));
    let Some(hook) = crate::provider::for_package(&pkg).and_then(|p| p.details()) else {
        return failed(format!("details: tveee has no details for {name}"));
    };
    let Some(url) = url else { return failed(format!("details: nothing playing in {name} that tveee can look up")) };
    let d = hook.details(&c.tv, &url)?;
    println!("{:<10}{url}", "link:");
    for (k, v) in &d.fields {
        println!("{:<10}{v}", format!("{k}:"));
    }
    if !d.chapters.is_empty() {
        println!("chapters:");
        for (i, ch) in d.chapters.iter().enumerate() {
            println!("  {:>2}  {:>7}  {}", i + 1, clock(ch.start), ch.title);
        }
    }
    if let Some(desc) = &d.description {
        let lines: Vec<&str> = desc.lines().collect();
        let shown = if full { lines.len() } else { lines.len().min(8) };
        println!("description:");
        for l in &lines[..shown] {
            println!("  {l}");
        }
        if shown < lines.len() {
            println!("  ... ({} more lines: --full)", lines.len() - shown);
        }
    }
    Ok(())
}

/// Carry out a `goto`: open a link in the app, or seek the video.
/// Do what a [`crate::more::Go`] says, for the app `pkg`: seek its video,
/// or open a link (in it, or wherever the link belongs).
pub fn go(tv: &Tv, pkg: &str, to: &crate::more::Go) -> Result<String> {
    match to {
        crate::more::Go::Seek(t) => {
            let player = crate::provider::for_package(pkg).and_then(|p| p.player());
            match player.map(|p| p.seek(tv, crate::devtools::SeekTo::At(*t)).or_fallback()) {
                Some(Ok(Some(at))) => Ok(format!("seeked to {}", clock(at))),
                Some(Err(e)) => Err(e),
                _ => failed("goto: can't reach the app's video to seek"),
            }
        }
        crate::more::Go::Link(url) => {
            // the app's own links go to it; anything else goes wherever it belongs
            let own = crate::provider::for_package(pkg)
                .is_some_and(|p| p.places().iter().any(|pl| host(pl.link) == host(url)) || p.search_link().is_some_and(|s| host(s) == host(url)));
            if own {
                if !super::apps::link_here_or_prepare(tv, pkg, url)? {
                    let cmd = format!("am start -a android.intent.action.VIEW -d {} -p {}", super::open::sh_quote(url), pkg);
                    tv.shell(&[&cmd])?;
                }
                Ok(format!("opened {url}"))
            } else {
                super::open::open_link(tv, url)
            }
        }
    }
}

fn host(url: &str) -> &str {
    let rest = url.split("://").nth(1).unwrap_or(url);
    rest.split('/').next().unwrap_or(rest).trim_start_matches("www.")
}

/// The lines `tveee list` prints for `l`: its heading, then each item
/// numbered from 1, the highlighted one marked, folders ending in "/".
pub fn list_lines(l: &crate::provider::ContextList, app: &str) -> Vec<String> {
    let w = l.items.iter().map(|i| i.label.chars().count() + usize::from(i.folder)).max().unwrap_or(0).min(48);
    let mut out = vec![format!("{} ({app}, {} {})", l.title, l.items.len(), if l.items.len() == 1 { "item" } else { "items" })];
    let mut part = None;
    for (n, i) in l.items.iter().enumerate() {
        // a part's subheading where it starts
        if i.section.is_some() && i.section != part {
            out.push(format!("─ {}", i.section.as_deref().unwrap_or_default()));
        }
        part.clone_from(&i.section);
        let mark = if l.at == Some(n) { if l.kind == "folder" { "›" } else { "▶" } } else { " " };
        let name = format!("{}{}", i.label, if i.folder { "/" } else { "" });
        let meta = i.meta.as_deref().map(|m| format!("  {m}")).unwrap_or_default();
        out.push(format!("{mark} {:>3}  {name:<w$}{meta}", n + 1).trim_end().to_string());
    }
    out
}

/// `tveee list [app] [N]`: the list the app shows (Kodi's folder), or
/// choose item N from it.
fn list_cmd(c: &crate::actions::Cx, args: &[String]) -> Result<crate::actions::Output> {
    let tv = c.tv;
    // `list kodi` names an app (its list can be read behind another);
    // otherwise it's the app in front
    let named = args.first().and_then(|a| {
        super::apps::all().iter().find(|x| x.alias == Some(a.as_str()) && crate::provider::for_package(x.package()).is_some_and(|p| p.player().is_some()))
    });
    let (pkg, rest) = match named {
        Some(a) => (a.package().to_string(), &args[1..]),
        None => (current(tv)?.0, args),
    };
    let name = super::apps::by_package(&pkg).map_or(pkg.clone(), |a| super::apps::list_name(a.name));
    let Some(player) = crate::provider::for_package(&pkg).and_then(|p| p.player()) else {
        return failed(format!("list: tveee can't read lists in {name}"));
    };
    let Some(l) = player.list(tv).or_fallback()? else {
        return Ok(format!("{name} isn't showing a list").into());
    };
    match rest {
        [] => {
            let mut lines = list_lines(&l, &name);
            lines.push("(tveee list N: choose one)".into());
            Ok(lines.join("\n").into())
        }
        [n] => {
            let Some(item) = n.parse::<usize>().ok().and_then(|n| n.checked_sub(1)).and_then(|i| l.items.get(i)) else {
                return crate::error::usage(format!("list: choose 1 to {}, not {n}", l.items.len()));
            };
            let said = match player.go(tv, &item.go).or_fallback()? {
                Some(said) => said,
                None => go(tv, &pkg, &item.go)?,
            };
            Ok(format!("{}: {said}", item.label).into())
        }
        _ => crate::error::usage("usage: tveee list [app] [N]"),
    }
}

fn goto_cmd(c: &Ctx, args: &[String]) -> Result<()> {
    let tv = &c.tv;
    // `goto <app> ...` names an app; otherwise it's the app in front
    let named = args.first().and_then(|a| {
        super::apps::all()
            .iter()
            .find(|x| x.alias == Some(a.as_str()) && crate::provider::for_package(x.package()).is_some_and(|p| !p.places().is_empty()))
    });
    let (pkg, app, url, rest) = match named {
        Some(a) => (a.package().to_string(), Some(a), None, &args[1..]),
        None => {
            let (pkg, app, url) = current(tv)?;
            (pkg, app, url, args)
        }
    };
    let name = app.map_or(pkg.clone(), |a| super::apps::list_name(a.name));
    let provider = crate::provider::for_package(&pkg);
    let places = provider.map_or(&[][..], |p| p.places());
    // links from what's on (only for the app in front)
    let links = || -> Vec<crate::more::Link> {
        match (provider.and_then(|p| p.details()), &url) {
            (Some(hook), Some(u)) => hook.details(tv, u).map(|d| d.links).unwrap_or_default(),
            _ => Vec::new(),
        }
    };
    let words: Vec<&str> = rest.iter().map(String::as_str).collect();
    match words.as_slice() {
        [] => {
            if places.is_empty() && provider.and_then(|p| p.details()).is_none() {
                return failed(format!("goto: tveee knows no places in {name}"));
            }
            println!("{name}:");
            for p in places {
                println!("  {:<14} {}", p.id, p.label);
            }
            let ls = links();
            if !ls.is_empty() {
                println!("from what's on:");
                for (i, l) in ls.iter().enumerate() {
                    println!("  {:>3}  {}", i + 1, l.label);
                }
            }
            Ok(())
        }
        [w] if places.iter().any(|p| p.id == *w) => {
            let p = places.iter().find(|p| p.id == *w).expect("checked");
            println!("{}", go(tv, &pkg, &crate::more::Go::Link(p.link.to_string()))?);
            Ok(())
        }
        ["channel"] => match links().into_iter().find(|l| l.label.starts_with("channel")) {
            Some(l) => {
                println!("{}", go(tv, &pkg, &l.go)?);
                Ok(())
            }
            None => failed(format!("goto: no channel for what's on in {name}")),
        },
        ["chapter", n] | [n] if n.parse::<usize>().is_ok() => {
            let n: usize = n.parse().unwrap_or(0);
            let chapter = rest.first().is_some_and(|w| w == "chapter");
            let ls = links();
            let pick = if chapter {
                ls.iter().filter(|l| matches!(l.go, crate::more::Go::Seek(_))).nth(n.saturating_sub(1))
            } else {
                ls.get(n.saturating_sub(1))
            };
            match pick {
                Some(l) => {
                    println!("{}", go(tv, &pkg, &l.go)?);
                    Ok(())
                }
                None => failed(format!("goto: no {}{n} (see 'tveee goto')", if chapter { "chapter " } else { "link " })),
            }
        }
        _ => usage(format!(
            "goto: where? one of: {}{}channel, chapter N, or a number from 'tveee goto'",
            places.iter().map(|p| p.id).collect::<Vec<_>>().join(", "),
            if places.is_empty() { "" } else { ", " }
        )),
    }
}

pub fn actions() -> Vec<ActionDescriptor> {
    vec![
        ActionDescriptor::terminal("info.status", CAT, "power state, foreground app, mute and free RAM", status).cli("status", WATCH_USAGE, "power state, foreground app, mute and free RAM").long(concat!(
            "When tveee recognises what is on, it says so in words, e.g.\n  what:     Watching YouTube\n  title:    <title> (<channel>)\n  progress: ━━━━━━━━━━━━━━━━━━━━━━━━━━━┃── 25:16 / 27:31 (91%)\n  url:      https://www.youtube.com/watch?v=...\nTitles come from the app's media session; the progress bar and link from its\ndevtools page, for apps that expose one (currently YouTube). Lines are left out\nwhen there is nothing to say. 'tveee nowplaying' shows just those lines.\n\n", watch_long!()),
        ),
        ActionDescriptor::terminal("info.nowplaying", CAT, "what's on: app, title, progress bar and link", nowplaying_cmd).cli("nowplaying", WATCH_USAGE, "what's on: app, title, progress bar and link")
            .long(concat!(
                "The what/title/progress/url lines from 'tveee status', on their own. Prints\n\"nothing playing ...\" and exits 1 when tveee doesn't recognise what is on.\n\n",
                watch_long!()
            ))
            .aliases(&["np"]),
        ActionDescriptor::terminal("info.time", CAT, "show the TV's clock and compare it with this PC", time).cli("time", "", "show the TV's clock and compare it with this PC").long(
            "Prints the TV's local time, timezone and automatic-time settings, and how far\nits clock is from this PC's. Warns when the clock is off by more than 5s or the\nUTC offset differs (the usual symptom after a daylight-saving change).",
        ),
        ActionDescriptor::terminal("info.now", CAT, "print the foreground window only", now).cli("now", "", "print the foreground window only"),
        ActionDescriptor::terminal("info.details", CAT, "more about what's on: channel, chapters, description", details_cmd).cli("details", "[--full]", "more about what's on: channel, chapters, description").long(
            "For apps whose plugin knows how (YouTube, using yt-dlp; cached a day per video):\n\
             the channel, upload date, views, likes, length, chapters and description.\n\
             \x20 --full   the whole description, not just its first lines",
        ),
        ActionDescriptor::new("info.list", CAT, "the list the app shows (Kodi's folder), or choose from it", list_cmd)
            .cli("list", "[app] [N]", "the list the app in front shows, or choose item N from it")
            .long(
                "Prints the list the app in front is showing, numbered, the highlighted one\n\
                 marked (› or ▶): in Kodi, the folder or add-on listing on its screen, as\n\
                 Kodi shows it; in YouTube, the video's chapters (choosing one seeks there)\n\
                 and links. Reading it doesn't change what's on the screen.\n\
                 \n\
                 \x20 tveee list          the app in front's list\n\
                 \x20 tveee list kodi     Kodi's, even behind another app\n\
                 \x20 tveee list 3        choose item 3: a folder opens, anything else plays",
            )
            .args("[app] [N]"),
        ActionDescriptor::terminal("info.goto", CAT, "go to a place in an app, or a link from what's on", goto_cmd).cli("goto", "[app] [place|channel|chapter N|N]", "go to a place in an app, or a link from what's on").long(
            "With nothing more, lists where you can go: the app's places (YouTube: home,\n\
             subscriptions, history, library, watchlater) and, for what's on, its channel,\n\
             chapters and the links in its description, numbered.\n\
             \x20 tveee goto history          a place in the app in front\n\
             \x20 tveee goto youtube history  in a named app (it opens it)\n\
             \x20 tveee goto channel          the channel of what's on\n\
             \x20 tveee goto chapter 3        seek to chapter 3\n\
             \x20 tveee goto 7                link 7 from the list",
        ),
        ActionDescriptor::terminal("info.logs", CAT, "recent logcat, optionally filtered", logs).cli("logs", "[regex]", "recent logcat, optionally filtered")
            .long("Case-insensitive extended regex; shows the last 200 matching lines."),
    ]
}

#[cfg(test)]
mod tests {
    #[test]
    fn finds_window_uid() {
        let dump = "    mOwnerUid=1000 mShowToOwnerOnly=true package=android appop=NONE\n\
                    \x20   mOwnerUid=10039 mShowToOwnerOnly=true package=org.droidtv.nettvbrowser appop=NONE\n";
        assert_eq!(super::window_uid(dump, "org.droidtv.nettvbrowser"), Some(10039));
        assert_eq!(super::window_uid(dump, "org.droidtv"), None);
    }

    use super::*;

    const DUMP: &str = "  mCurrentFocus=Window{eeb8591 u0 org.droidtv.nettvbrowser/org.droidtv.nettvbrowser.BrowserNetTvActivity}\n";

    #[test]
    fn list_printed() {
        use crate::provider::{ContextList, ListItem};
        let item = |label: &str, folder, meta: Option<&str>| ListItem { section: None, id: String::new(), label: label.into(), meta: meta.map(str::to_string), folder, go: crate::more::Go::Link(String::new()) };
        let l = ContextList { title: "Artists".into(), kind: "folder", key: String::new(), items: vec![item("Portishead", true, None), item("Radiohead", true, Some("9 albums")), item("Exit Music.flac", false, None)], at: Some(1) };
        let lines = list_lines(&l, "Kodi");
        assert_eq!(lines[0], "Artists (Kodi, 3 items)");
        assert_eq!(lines[1], "    1  Portishead/");
        assert_eq!(lines[2], "›   2  Radiohead/       9 albums");
        assert_eq!(lines[3], "    3  Exit Music.flac");
        // parts: a subheading where each starts
        let mut l = l;
        l.items[0].section = Some("Folders".into());
        l.items[1].section = Some("Folders".into());
        l.items[2].section = Some("Files".into());
        let lines = list_lines(&l, "Kodi");
        assert_eq!(lines[1], "─ Folders");
        assert_eq!(lines[4], "─ Files");
        assert_eq!(lines[5], "    3  Exit Music.flac");
    }

    #[test]
    fn parses_focus() {
        assert_eq!(
            focus_window(DUMP).unwrap(),
            "org.droidtv.nettvbrowser/org.droidtv.nettvbrowser.BrowserNetTvActivity"
        );
        assert_eq!(focus_package(DUMP).unwrap(), "org.droidtv.nettvbrowser");
        assert_eq!(focus_package("no focus here"), None);
    }

    #[test]
    fn watch_args() {
        let a = |v: &[&str]| parse_watch_args("status", &v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(a(&[]).unwrap(), None);
        assert_eq!(a(&["--watch"]).unwrap(), Some(2.0));
        assert_eq!(a(&["-n", "5"]).unwrap(), Some(5.0));
        assert_eq!(a(&["--watch", "--interval", "1m"]).unwrap(), Some(60.0));
        assert!(a(&["-n", "0.1"]).is_err());
        assert!(a(&["--bogus"]).is_err());
    }

    #[test]
    fn parses_clock() {
        let out = "1793397600 +0000 2026-10-30 22:00:00 GMT\n1\n0\nEurope/London\n";
        let c = parse_clock(out).unwrap();
        assert_eq!(c.epoch, 1_793_397_600);
        assert_eq!(c.offset, "+0000");
        assert_eq!(c.text, "2026-10-30 22:00:00 GMT");
        assert_eq!(c.auto_time, Some(true));
        assert_eq!(c.auto_zone, Some(false));
        assert_eq!(c.zone, "Europe/London");
        // settings missing/null on some firmwares
        let c = parse_clock("1 +0100 2026-07-01 00:00:01 BST\nnull\nnull\n\n").unwrap();
        assert_eq!((c.auto_time, c.auto_zone), (None, None));
        assert_eq!(parse_clock("garbage"), None);
    }

    #[test]
    fn parses_system_figures() {
        let df = "Filesystem                                             1K-blocks     Used Available Use% Mounted on
tmpfs                                                     687516      516    687000   1% /dev
/dev/block/mmcblk0p7                                     3791072  3695136     79552  98% /data
/dev/block/dm-1                                        732199464 39472900 692726564   6% /mnt/expand/76ea532c-6c26
/mnt/expand/76ea532c-6c26/media 732199464 39472900 692726564   6% /storage/emulated
";
        let disks = parse_df(df);
        assert_eq!(disks.len(), 4);
        assert_eq!(disks[1], Disk { mount: "/data".into(), total_kb: 3_791_072, used_kb: 3_695_136, avail_kb: 79_552 });
        let shown: Vec<_> = interesting_disks(&disks).iter().map(|(l, d)| (*l, d.mount.clone())).collect();
        assert_eq!(shown, vec![("internal", "/data".to_string()), ("adopted", "/mnt/expand/76ea532c-6c26".to_string())]);
        assert_eq!(parse_loadavg("87.91 86.73 86.06 19/2452 28527"), Some((87.91, 86.73, 86.06)));
        assert_eq!(human_uptime(101_149.0), "1d 4h");
        assert_eq!(human_uptime(4_000.0), "1h 6m");
        assert_eq!(human_uptime(90.0), "1m");
        assert_eq!(disk_summary("internal", &disks[1]), "internal 3.5/3.6 GB (97%, 77 MB free)");
        let sys = parse_system("MemTotal: 100 kB\nMemFree: 10 kB\n@@\n1.5 2 3 1/2 3\n@@\n60.5 1\n@@\nFilesystem 1K-blocks Used Available Use% Mounted on\n/dev/x 10 5 5 50% /data\n@@\n4\n");
        assert_eq!(sys.mem.unwrap().total, 100);
        assert_eq!(sys.load, Some((1.5, 2.0, 3.0)));
        assert_eq!(sys.uptime_secs, Some(60.5));
        assert_eq!(sys.disks.len(), 1);
        assert_eq!(sys.cores, Some(4));
    }

    #[test]
    fn summarises_ram() {
        let m = "MemTotal:        1374000 kB\nMemFree:           41000 kB\nMemAvailable:     210000 kB\nSwapTotal:        430000 kB\nSwapFree:         205000 kB\n";
        assert_eq!(
            ram_summary(m).unwrap(),
            "40 MB free, 205 MB available of 1341 MB; swap used 219 MB"
        );
        assert_eq!(ram_summary("nothing"), None);
    }
}

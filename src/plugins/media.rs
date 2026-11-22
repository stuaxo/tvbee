use crate::error::{failed, usage, Result};
use crate::registry::Command;
use crate::tv::Tv;
use crate::Ctx;

const CAT: &str = "media";

const SKIP_LONG: &str = "Skips by a duration (90s, 2m, 1h30m, 1:30:00) or by a percentage of the total\nlength (10%). Android can't seek by position over ADB, so this sends\nfast-forward/rewind key presses, each assumed to skip --step (default 10s; apps\ndiffer). Percentages need the app to publish a media session with a duration.";

/// Mute state of the music stream from `dumpsys audio`, if it can be found.
pub fn parse_muted(dump: &str) -> Option<bool> {
    let mut lines = dump.lines().skip_while(|l| !l.contains("- STREAM_MUSIC:"));
    lines.next()?;
    let line = lines.find(|l| l.contains("Muted:"))?;
    match line.split("Muted:").nth(1)?.split_whitespace().next()? {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// (current, max) music volume for the output in use, from `dumpsys audio`:
/// `Max: 60`, `Current: 2 (speaker): 30, 40000 (hmdi_arc): 23`, `Devices: hmdi_arc`.
pub fn parse_volume(dump: &str) -> Option<(u32, u32)> {
    let mut lines = dump.lines().skip_while(|l| !l.contains("- STREAM_MUSIC:"));
    lines.next()?;
    let section: Vec<&str> = lines.take_while(|l| !l.trim_start().starts_with("- STREAM_")).collect();
    let field = |k: &str| section.iter().find_map(|l| l.trim().strip_prefix(k)).map(str::trim);
    let max: u32 = field("Max:")?.parse().ok()?;
    let current = field("Current:")?;
    let device = field("Devices:").and_then(|d| d.split_whitespace().next()).unwrap_or("");
    // "2 (speaker): 30" pairs; prefer the device in use, else the first
    let pairs: Vec<(&str, u32)> = current
        .split(", ")
        .filter_map(|p| {
            let (name, v) = p.rsplit_once("): ")?;
            Some((name.rsplit_once('(')?.1, v.trim().parse().ok()?))
        })
        .collect();
    let level = pairs.iter().find(|(n, _)| *n == device).or(pairs.first())?.1;
    Some((level, max))
}

pub fn is_muted(tv: &Tv) -> Result<Option<bool>> {
    Ok(parse_muted(&tv.shell(&["dumpsys", "audio"])?))
}

/// KEYCODE_VOLUME_MUTE only toggles, so check the state first.
pub fn set_mute(tv: &Tv, want: bool) -> Result<()> {
    if is_muted(tv)? != Some(want) {
        tv.keyevent(164)?;
    }
    Ok(())
}

fn mute(c: &Ctx, _: &[String]) -> Result<()> {
    c.tv.connect()?;
    set_mute(&c.tv, true)
}

fn unmute(c: &Ctx, _: &[String]) -> Result<()> {
    c.tv.connect()?;
    set_mute(&c.tv, false)
}

fn vol(c: &Ctx, args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("up") => c.tv.key(24),
        Some("down") => c.tv.key(25),
        Some(n) if !n.is_empty() && n.chars().all(|ch| ch.is_ascii_digit()) => {
            c.tv.connect()?;
            c.tv.shell(&["media", "volume", "--stream", "3", "--set", n]).map(drop)
        }
        _ => usage("usage: tvbee vol up|down|<level>"),
    }
}

/// "1h30m", "90s", "2 min", "1:30:00", "1:30", or plain seconds -> seconds.
pub fn parse_duration(s: &str) -> Option<f64> {
    let s = s.trim().to_lowercase();
    if s.is_empty() {
        return None;
    }
    if s.contains(':') {
        let parts: Vec<f64> = s.split(':').map(|p| p.trim().parse().ok()).collect::<Option<_>>()?;
        return match parts[..] {
            [m, sec] => Some(m * 60.0 + sec),
            [h, m, sec] => Some(h * 3600.0 + m * 60.0 + sec),
            _ => None,
        };
    }
    if let Ok(n) = s.parse::<f64>() {
        return (n >= 0.0).then_some(n);
    }
    let (mut total, mut rest) = (0.0, s.as_str());
    while !rest.is_empty() {
        let num_end = rest.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(rest.len());
        let n: f64 = rest[..num_end].parse().ok()?;
        rest = rest[num_end..].trim_start();
        let unit_end = rest.find(|c: char| !c.is_ascii_alphabetic()).unwrap_or(rest.len());
        total += n * match &rest[..unit_end] {
            "h" | "hr" | "hrs" | "hour" | "hours" => 3600.0,
            "m" | "min" | "mins" | "minute" | "minutes" => 60.0,
            "s" | "sec" | "secs" | "second" | "seconds" => 1.0,
            _ => return None,
        };
        rest = rest[unit_end..].trim_start();
    }
    Some(total)
}

#[derive(Debug, PartialEq)]
pub enum Amount {
    Seconds(f64),
    /// Fraction of the total duration, 0.0..=1.0
    Fraction(f64),
}

pub fn parse_amount(s: &str) -> Option<Amount> {
    match s.trim().strip_suffix('%') {
        Some(p) => {
            let pct: f64 = p.trim().parse().ok()?;
            (0.0..=100.0).contains(&pct).then_some(Amount::Fraction(pct / 100.0))
        }
        None => parse_duration(s).map(Amount::Seconds),
    }
}

/// Number of fast-forward key presses to cover `secs` at `step` seconds each.
pub fn presses(secs: f64, step: f64) -> u32 {
    (secs / step).round().max(1.0) as u32
}

/// Duration (ms) of the active media session from `dumpsys media_session`, if present.
pub fn parse_media_duration_ms(dump: &str) -> Option<u64> {
    let after = &dump[dump.find("metadata.DURATION")? + "metadata.DURATION".len()..];
    let digits: String = after
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok().filter(|ms| *ms > 0)
}

/// One session from `dumpsys media_session`: who owns it, its playback state and
/// what the app says is playing.
#[derive(Debug, PartialEq, Default)]
pub struct MediaInfo {
    pub package: String,
    /// PlaybackState: 1 stopped, 2 paused, 3 playing, 6 buffering, ...
    pub state: Option<u32>,
    pub title: Option<String>,
    pub subtitle: Option<String>,
}

impl MediaInfo {
    pub fn playing(&self) -> bool {
        matches!(self.state, Some(3 | 6))
    }

    pub fn paused(&self) -> bool {
        self.state == Some(2)
    }
}

fn non_null(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty() && s != "null").then(|| s.to_string())
}

/// `MediaDescription.toString()` is "title, subtitle, description". Split from the
/// right so commas in the title survive.
pub fn split_description(d: &str) -> (Option<String>, Option<String>) {
    let mut parts: Vec<&str> = d.rsplitn(3, ", ").collect();
    parts.reverse();
    match parts.as_slice() {
        [title, sub, _desc] => (non_null(title), non_null(sub)),
        _ => (non_null(d), None),
    }
}

pub fn parse_media_sessions(dump: &str) -> Vec<MediaInfo> {
    let mut out: Vec<MediaInfo> = Vec::new();
    for line in dump.lines().map(str::trim) {
        if let Some(pkg) = line.strip_prefix("package=") {
            out.push(MediaInfo { package: pkg.trim().to_string(), ..Default::default() });
            continue;
        }
        let Some(cur) = out.last_mut() else { continue };
        if line.starts_with("state=PlaybackState {") {
            cur.state = number_after(line, "{state=");
        } else if line.starts_with("metadata:")
            && let Some(d) = line.split_once("description=").map(|(_, d)| d) {
                (cur.title, cur.subtitle) = split_description(d);
            }
    }
    out
}

const MAX_PRESSES: u32 = 120;
const FFW_KEY: u32 = 90;
const REW_KEY: u32 = 89;

/// What `dumpsys media_session` says about the active playback.
#[derive(Debug, PartialEq)]
pub struct Session {
    pub state: u32,
    pub position_ms: u64,
    pub speed: f64,
    pub updated_ms: u64,
    pub duration_ms: Option<u64>,
}

fn number_after<T: std::str::FromStr>(text: &str, key: &str) -> Option<T> {
    let rest = &text[text.find(key)? + key.len()..];
    let end = rest.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-')).unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// First `PlaybackState {state=N, position=P, ..., speed=S, updated=U ...}` that is
/// playing (3) or paused (2).
pub fn parse_session(dump: &str) -> Option<Session> {
    dump.match_indices("PlaybackState {").find_map(|(i, _)| {
        let st = &dump[i..];
        let st = &st[..st.find('}').unwrap_or(st.len())];
        let state: u32 = number_after(st, "state=")?;
        if state != 2 && state != 3 {
            return None;
        }
        Some(Session {
            state,
            position_ms: number_after(st, ", position=")?,
            speed: number_after(st, "speed=").unwrap_or(1.0),
            updated_ms: number_after(st, "updated=").unwrap_or(0),
            duration_ms: parse_media_duration_ms(dump),
        })
    })
}

impl Session {
    /// Position now: while playing, the reported position is stale by
    /// (uptime - updated) at the playback speed.
    pub fn position_now_ms(&self, uptime_ms: u64) -> f64 {
        let mut pos = self.position_ms as f64;
        if self.state == 3 && uptime_ms > self.updated_ms && self.updated_ms > 0 {
            pos += (uptime_ms - self.updated_ms) as f64 * self.speed;
        }
        pos
    }
}

fn uptime_ms(tv: &crate::tv::Tv) -> Result<u64> {
    let out = tv.shell(&["cat", "/proc/uptime"])?;
    let secs: f64 = out.split_whitespace().next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
    Ok((secs * 1000.0) as u64)
}

const SKIP_USAGE: &str = "<time|percent> [--step <duration>]";

/// Parse `<amount> [--step <duration>]`.
fn parse_skip_args(name: &str, args: &[String]) -> Result<(Amount, f64)> {
    let (mut amount, mut step) = (None, 10.0);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--step" {
            step = it
                .next()
                .and_then(|v| parse_duration(v))
                .filter(|s| *s > 0.0)
                .map_or_else(|| usage(format!("{name}: --step needs a duration")), Ok)?;
        } else if amount.is_none() {
            amount = Some(a.as_str());
        } else {
            return usage(format!("usage: tvbee {name} {SKIP_USAGE}"));
        }
    }
    match amount.and_then(parse_amount) {
        Some(a) => Ok((a, step)),
        None => usage(format!("usage: tvbee {name} {SKIP_USAGE}  (e.g. 90s, 2m, 1h30m, 10%)")),
    }
}

/// Send enough fast-forward (secs > 0) or rewind (secs < 0) presses.
fn skip_by(c: &Ctx, secs: f64, step: f64) -> Result<()> {
    let n = presses(secs.abs(), step);
    if n > MAX_PRESSES {
        return usage(format!("that is {n} presses at {step}s each; use a larger --step"));
    }
    let key = if secs >= 0.0 { FFW_KEY } else { REW_KEY }.to_string();
    let codes = vec![key; n as usize];
    let mut cmd = vec!["input", "keyevent"];
    cmd.extend(codes.iter().map(String::as_str));
    c.tv.shell(&cmd)?;
    let dir = if secs >= 0.0 { "forward" } else { "back" };
    println!("skipped {dir} ~{}s ({n} x {step}s)", (n as f64 * step).round());
    Ok(())
}

fn session(c: &Ctx) -> Result<Session> {
    match parse_session(&c.tv.shell(&["dumpsys", "media_session"])?) {
        Some(s) => Ok(s),
        None => failed("no active media session on the TV (the app may not publish one)"),
    }
}

fn skip(c: &Ctx, name: &str, args: &[String], sign: f64) -> Result<()> {
    let (amount, step) = parse_skip_args(name, args)?;
    c.tv.connect()?;
    let secs = match amount {
        Amount::Seconds(s) => s,
        Amount::Fraction(f) => match session(c)?.duration_ms {
            Some(ms) => ms as f64 / 1000.0 * f,
            None => return failed(format!("{name}: can't read the media duration; give a time instead")),
        },
    };
    skip_by(c, sign * secs, step)
}

fn ffw(c: &Ctx, args: &[String]) -> Result<()> {
    skip(c, "ffw", args, 1.0)
}

fn rew(c: &Ctx, args: &[String]) -> Result<()> {
    skip(c, "rew", args, -1.0)
}

/// Jump to an absolute position by skipping the difference from where we are.
fn seek(c: &Ctx, args: &[String]) -> Result<()> {
    let (amount, step) = parse_skip_args("seek", args)?;
    c.tv.connect()?;
    let sess = session(c)?;
    let target = match amount {
        Amount::Seconds(s) => s,
        Amount::Fraction(f) => match sess.duration_ms {
            Some(ms) => ms as f64 / 1000.0 * f,
            None => return failed("seek: can't read the media duration; give a time instead"),
        },
    };
    let now = sess.position_now_ms(uptime_ms(&c.tv)?) / 1000.0;
    println!("position ~{now:.0}s -> {target:.0}s");
    let delta = target - now;
    if delta.abs() < step / 2.0 {
        println!("already there (within {step}s)");
        return Ok(());
    }
    skip_by(c, delta, step)
}

pub fn commands() -> Vec<Command> {
    vec![
        Command::new(CAT, "play", "", "media play", |c, _| c.tv.key(126)),
        Command::new(CAT, "pause", "", "media pause", |c, _| c.tv.key(127)),
        Command::new(CAT, "stop", "", "media stop", |c, _| c.tv.key(86)),
        Command::new(CAT, "playpause", "", "toggle play/pause", |c, _| c.tv.key(85)),
        Command::new(CAT, "mute", "", "mute (no-op if already muted)", mute),
        Command::new(CAT, "unmute", "", "unmute (no-op if not muted)", unmute),
        Command::new(CAT, "ffw", SKIP_USAGE, "fast-forward by an amount", ffw).long(SKIP_LONG),
        Command::new(CAT, "rew", SKIP_USAGE, "rewind by an amount", rew).long(SKIP_LONG),
        Command::new(CAT, "seek", SKIP_USAGE, "jump to a position (time or percent of the way through)", seek).long(
            "Like ffw/rew but absolute: reads the current position from the TV's media\nsession, then skips the difference. Needs an app that publishes a media session\nwith a position (and a duration for percentages). Approximate: accuracy is\nlimited by --step, so seeking lands within about one step of the target.",
        ),
        Command::new(CAT, "vol", "up|down|<level>", "change volume", vol)
            .long("A number sets an absolute level with 'media volume'."),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const DUMP: &str = "- STREAM_RING:\n   Muted: true\n- STREAM_MUSIC:\n   Muted: false\n   Min: 0\n- STREAM_ALARM:\n   Muted: true\n";

    #[test]
    fn reads_music_stream_only() {
        assert_eq!(parse_muted(DUMP), Some(false));
        assert_eq!(parse_muted(&DUMP.replace("Muted: false", "Muted: true")), Some(true));
    }

    #[test]
    fn reads_volume() {
        let d = "- STREAM_MUSIC:\n   Muted: false\n   Min: 0\n   Max: 60\n   Current: 2 (speaker): 30, 4 (headset): 10, 40000 (hmdi_arc): 23, 40000000 (default): 28\n   Devices: hmdi_arc\n- STREAM_ALARM:\n   Max: 7\n";
        assert_eq!(parse_volume(d), Some((23, 60)));
        let d = "- STREAM_MUSIC:\n   Max: 15\n   Current: 2 (speaker): 9\n   Devices: speaker\n";
        assert_eq!(parse_volume(d), Some((9, 15)));
        assert_eq!(parse_volume("nothing"), None);
    }

    #[test]
    fn reads_session_titles() {
        let dump = "  Sessions Stack - have 2 sessions:
    starboard com.google.android.youtube.tv/starboard (userId=0)
      package=com.google.android.youtube.tv
      active=true
      state=PlaybackState {state=3, position=0, buffered position=0, speed=1.0, updated=80120374, actions=382}
      metadata:size=5, description=Diane Morgan and Charlie Brooker on Cunk on Cinema | BFI Q&A, BFI, null
    HeadsetMediaButton com.android.server.telecom/HeadsetMediaButton (userId=0)
      package=com.android.server.telecom
      state=null
      metadata:size=0, description=null
Audio playback (lastly played comes first)
  uid=10075 packages=com.google.android.youtube.tv
";
        let s = parse_media_sessions(dump);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].package, "com.google.android.youtube.tv");
        assert!(s[0].playing());
        assert_eq!(s[0].title.as_deref(), Some("Diane Morgan and Charlie Brooker on Cunk on Cinema | BFI Q&A"));
        assert_eq!(s[0].subtitle.as_deref(), Some("BFI"));
        assert_eq!((s[1].state, s[1].title.as_deref()), (None, None));
    }

    #[test]
    fn descriptions_keep_commas_in_titles() {
        assert_eq!(
            split_description("Rock, Paper, Scissors, Some Channel, null"),
            (Some("Rock, Paper, Scissors".into()), Some("Some Channel".into()))
        );
        assert_eq!(split_description("null"), (None, None));
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("90s"), Some(90.0));
        assert_eq!(parse_duration("2m"), Some(120.0));
        assert_eq!(parse_duration("1h30m"), Some(5400.0));
        assert_eq!(parse_duration("1h 30 min 5s"), Some(5405.0));
        assert_eq!(parse_duration("1:30:00"), Some(5400.0));
        assert_eq!(parse_duration("2:15"), Some(135.0));
        assert_eq!(parse_duration("45"), Some(45.0));
        assert_eq!(parse_duration("1.5m"), Some(90.0));
        assert_eq!(parse_duration(""), None);
        assert_eq!(parse_duration("5x"), None);
        assert_eq!(parse_duration("m5"), None);
    }

    #[test]
    fn amounts() {
        assert_eq!(parse_amount("10%"), Some(Amount::Fraction(0.1)));
        assert_eq!(parse_amount("2m"), Some(Amount::Seconds(120.0)));
        assert_eq!(parse_amount("150%"), None);
        assert_eq!(parse_amount("abc%"), None);
    }

    #[test]
    fn press_counts() {
        assert_eq!(presses(60.0, 10.0), 6);
        assert_eq!(presses(3.0, 10.0), 1);
        assert_eq!(presses(0.0, 10.0), 1);
    }

    #[test]
    fn reads_media_duration() {
        let d = "metadata: size=3\n  android.media.metadata.DURATION=5400000\n";
        assert_eq!(parse_media_duration_ms(d), Some(5_400_000));
        assert_eq!(parse_media_duration_ms("no metadata"), None);
    }

    const SESSION: &str = "  state=PlaybackState {state=3, position=60000, buffered position=0, speed=1.0, updated=100000, actions=0}\n  android.media.metadata.DURATION=3600000\n";

    #[test]
    fn parses_session() {
        let s = parse_session(SESSION).unwrap();
        assert_eq!(s.state, 3);
        assert_eq!(s.position_ms, 60_000);
        assert_eq!(s.updated_ms, 100_000);
        assert_eq!(s.duration_ms, Some(3_600_000));
        // 5s of playback since the state was reported
        assert_eq!(s.position_now_ms(105_000), 65_000.0);
    }

    #[test]
    fn paused_position_is_not_extrapolated() {
        let s = parse_session(&SESSION.replace("state=3", "state=2")).unwrap();
        assert_eq!(s.position_now_ms(500_000), 60_000.0);
    }

    #[test]
    fn stopped_sessions_are_ignored() {
        assert_eq!(parse_session(&SESSION.replace("state=3", "state=1")), None);
        assert_eq!(parse_session("nothing"), None);
    }

    #[test]
    fn skip_args() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(parse_skip_args("ffw", &a(&["30s"])).unwrap(), (Amount::Seconds(30.0), 10.0));
        assert_eq!(parse_skip_args("seek", &a(&["50%", "--step", "30s"])).unwrap(), (Amount::Fraction(0.5), 30.0));
        assert!(parse_skip_args("ffw", &a(&[])).is_err());
        assert!(parse_skip_args("ffw", &a(&["1m", "2m"])).is_err());
        assert!(parse_skip_args("ffw", &a(&["1m", "--step"])).is_err());
    }

    #[test]
    fn missing_is_unknown() {
        assert_eq!(parse_muted("nothing here"), None);
    }
}

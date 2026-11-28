use crate::actions::{ActionDescriptor as A, Cx, Output};
use crate::error::{Result, failed, usage};
use crate::tv::Tv;

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
    Ok(tv.read_whole::<crate::readings::Audio>()?.and_then(|a| a.muted()))
}

/// KEYCODE_VOLUME_MUTE only toggles, so check the state first, and after:
/// the key can be lost (in standby; just after waking, seen live), so it's
/// sent once more if the TV doesn't say it took. Returns what the TV says
/// after (None: it doesn't say).
pub fn set_mute(tv: &Tv, want: bool) -> Result<Option<bool>> {
    let now = is_muted(tv)?;
    if now == Some(want) {
        return Ok(now);
    }
    if now.is_none() {
        tv.keyevent(164)?;
        return Ok(None);
    }
    for _ in 0..2 {
        tv.keyevent(164)?;
        for _ in 0..6 {
            std::thread::sleep(std::time::Duration::from_millis(300));
            if is_muted(tv)? == Some(want) {
                return Ok(Some(want));
            }
        }
        crate::tlog!("mute: the key didn't take; again");
    }
    is_muted(tv)
}

/// What [`set_mute`] did, said.
pub fn mute_said(want: bool, after: Option<bool>) -> Result<Output> {
    let (done, not) = if want { ("muted", "not muted") } else { ("unmuted", "muted") };
    match after {
        Some(a) if a == want => Ok(done.into()),
        Some(_) => crate::error::failed(format!("sent the mute key twice, but the TV still says {not} (in standby it ignores it)")),
        None => Ok(format!("sent the mute key (the TV doesn't say whether it's {done})").into()),
    }
}

fn mute(c: &Cx, _: &[String]) -> Result<Output> {
    mute_said(true, set_mute(c.tv, true)?)
}

fn unmute(c: &Cx, _: &[String]) -> Result<Output> {
    mute_said(false, set_mute(c.tv, false)?)
}

fn vol(c: &Cx, args: &[String]) -> Result<Output> {
    match args.first().map(String::as_str) {
        Some("up") => c.tv.key(24).map(|()| "volume up".into()),
        Some("down") => c.tv.key(25).map(|()| "volume down".into()),
        Some(n) if !n.is_empty() && n.chars().all(|ch| ch.is_ascii_digit()) => {
            c.tv.shell(&["media", "volume", "--stream", "3", "--set", n])?;
            Ok(format!("volume {n}").into())
        }
        _ => usage("usage: tveee vol up|down|<level>"),
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
    let digits: String = after.chars().skip_while(|c| !c.is_ascii_digit()).take_while(char::is_ascii_digit).collect();
    digits.parse().ok().filter(|ms| *ms > 0)
}

/// One session from `dumpsys media_session`: who owns it, its playback state and
/// what the app says is playing.
#[derive(Debug, Clone, PartialEq, Default)]
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
        } else if line.starts_with("metadata:") {
            if let Some((_, d)) = line.split_once("description=") {
                (cur.title, cur.subtitle) = split_description(d);
            }
        }
    }
    out
}

/// Android's audio players (`dumpsys audio`, "playback activity"): the
/// uid of each and its state ("started", "paused", "idle", ...).
pub fn parse_audio_players(dump: &str) -> Vec<(u32, String)> {
    dump.lines()
        .filter_map(|l| {
            let uid = l.split("u/pid:").nth(1)?.split('/').next()?.parse().ok()?;
            let state = l.split("state:").nth(1)?.split_whitespace().next()?;
            Some((uid, state.to_string()))
        })
        .collect()
}

/// `sessions`, plus one standing in for `pkg` (whose windows belong to
/// `uid`) when no session of its says it's playing but its audio player
/// does: apps like the NetTV browser (iPlayer) play without a media session.
/// Only for apps' own uids: system uids are shared with other processes.
pub fn with_audio_players(mut sessions: Vec<MediaInfo>, pkg: &str, uid: Option<u32>, players: &[(u32, String)]) -> Vec<MediaInfo> {
    let Some(uid) = uid.filter(|u| *u >= 10_000) else { return sessions };
    if sessions.iter().any(|s| s.package == pkg && (s.playing() || s.paused())) {
        return sessions;
    }
    let mine = |state: &str| players.iter().any(|(u, s)| *u == uid && s == state);
    let state = if mine("started") {
        3
    } else if mine("paused") {
        2
    } else {
        return sessions;
    };
    sessions.push(MediaInfo { package: pkg.to_string(), state: Some(state), ..Default::default() });
    sessions
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
            step = it.next().and_then(|v| parse_duration(v)).filter(|s| *s > 0.0).map_or_else(|| usage(format!("{name}: --step needs a duration")), Ok)?;
        } else if amount.is_none() {
            amount = Some(a.as_str());
        } else {
            return usage(format!("usage: tveee {name} {SKIP_USAGE}"));
        }
    }
    match amount.and_then(parse_amount) {
        Some(a) => Ok((a, step)),
        None => usage(format!("usage: tveee {name} {SKIP_USAGE}  (e.g. 90s, 2m, 1h30m, 10%)")),
    }
}

/// Send enough fast-forward (secs > 0) or rewind (secs < 0) presses.
fn skip_by(c: &Cx, secs: f64, step: f64) -> Result<String> {
    let n = presses(secs.abs(), step);
    if n > MAX_PRESSES {
        return usage(format!("that is {n} presses at {step}s each; use a larger --step"));
    }
    let key = if secs >= 0.0 { FFW_KEY } else { REW_KEY };
    c.tv.keys(&vec![key; n as usize])?;
    let dir = if secs >= 0.0 { "forward" } else { "back" };
    Ok(format!("skipped {dir} ~{}s ({n} x {step}s)", (n as f64 * step).round()))
}

fn session(c: &Cx) -> Result<Session> {
    match parse_session(&c.tv.media_text()?) {
        Some(s) => Ok(s),
        None => failed("no active media session on the TV (the app may not publish one)"),
    }
}

/// The app in front's own player API (Kodi's), if it has one that answers.
fn player_in_front(tv: &crate::tv::Tv) -> Option<&'static dyn crate::provider::Player> {
    let pkg = super::info::focus_package(&tv.window_dump().ok()?)?;
    let player = crate::provider::for_package(&pkg)?.player()?;
    player.playing(tv).handled().map(|_| player)
}

/// Seek through the app's own player when it has one ("now at ..."); None:
/// do it with keys.
fn player_seek(c: &Cx, to: crate::devtools::SeekTo) -> Result<Option<String>> {
    let Some(player) = player_in_front(c.tv) else { return Ok(None) };
    Ok(player.seek(c.tv, to).or_fallback()?.map(|at| format!("now at {}", crate::nowplaying::clock(at))))
}

fn playpause(c: &Cx, _: &[String]) -> Result<Output> {
    if let Some(player) = player_in_front(c.tv) {
        if player.play_pause(c.tv).or_fallback()?.is_some() {
            return Ok("play/pause".into());
        }
    }
    c.tv.keyevent(85)?;
    Ok("play/pause".into())
}

fn skip(c: &Cx, name: &str, args: &[String], sign: f64) -> Result<Output> {
    let (amount, step) = parse_skip_args(name, args)?;
    if let Amount::Seconds(s) = amount {
        if let Some(said) = player_seek(c, crate::devtools::SeekTo::By(sign * s))? {
            return Ok(said.into());
        }
    }
    let secs = match amount {
        Amount::Seconds(s) => s,
        Amount::Fraction(f) => match session(c)?.duration_ms {
            Some(ms) => ms as f64 / 1000.0 * f,
            None => return failed(format!("{name}: can't read the media duration; give a time instead")),
        },
    };
    skip_by(c, sign * secs, step).map(Into::into)
}

fn ffw(c: &Cx, args: &[String]) -> Result<Output> {
    skip(c, "ff", args, 1.0)
}

fn rew(c: &Cx, args: &[String]) -> Result<Output> {
    skip(c, "rw", args, -1.0)
}

/// Jump to an absolute position by skipping the difference from where we are.
fn seek(c: &Cx, args: &[String]) -> Result<Output> {
    let (amount, step) = parse_skip_args("seek", args)?;
    let exact = match amount {
        Amount::Seconds(s) => crate::devtools::SeekTo::At(s),
        Amount::Fraction(f) => crate::devtools::SeekTo::Fraction(f),
    };
    if let Some(said) = player_seek(c, exact)? {
        return Ok(said.into());
    }
    let sess = session(c)?;
    let target = match amount {
        Amount::Seconds(s) => s,
        Amount::Fraction(f) => match sess.duration_ms {
            Some(ms) => ms as f64 / 1000.0 * f,
            None => return failed("seek: can't read the media duration; give a time instead"),
        },
    };
    let now = sess.position_now_ms(uptime_ms(c.tv)?) / 1000.0;
    let from = format!("position ~{now:.0}s -> {target:.0}s");
    let delta = target - now;
    if delta.abs() < step / 2.0 {
        return Ok(format!("{from}\nalready there (within {step}s)").into());
    }
    Ok(format!("{from}\n{}", skip_by(c, delta, step)?).into())
}

/// Show or hide the music visualiser of the app in front (`want`: on, off,
/// or None to switch). Says what it did.
pub fn visualiser_of(tv: &crate::tv::Tv, want: Option<bool>) -> Result<String> {
    let window = tv.window_dump()?;
    let pkg = crate::builtin::info::focus_window(&window).map(|w| w.split('/').next().unwrap_or(&w).to_string()).unwrap_or_default();
    let name = super::apps::by_package(&pkg).map_or(pkg.clone(), |a| a.name.to_string());
    let Some(player) = crate::provider::for_package(&pkg).and_then(|p| p.player()).filter(|p| p.has_visualiser()) else {
        return failed(format!("visualiser: {name} hasn't got one tveee can show (Kodi has, while it plays music)"));
    };
    match player.visualiser(tv, want).or_fallback()? {
        Some(true) => Ok(format!("{name}'s music visualiser is showing")),
        Some(false) => Ok(format!("back from {name}'s music visualiser")),
        None => failed(format!("visualiser: {name} can't show it now")),
    }
}

/// `on`, `off` or nothing (switch), as a visualiser wants it.
pub fn visualiser_want(args: &[String]) -> Result<Option<bool>> {
    match args.first().map(String::as_str) {
        None | Some("toggle") => Ok(None),
        Some("on") => Ok(Some(true)),
        Some("off") => Ok(Some(false)),
        Some(other) => usage(format!("visualiser: on, off or nothing (switch), not '{other}'")),
    }
}

fn visualiser(c: &Cx, args: &[String]) -> Result<Output> {
    visualiser_of(c.tv, visualiser_want(args)?).map(Into::into)
}

pub fn actions() -> Vec<A> {
    vec![
        A::new("media.play", CAT, "play", |c, _| c.tv.key(126).map(|()| "play".into())).cli("play", "", "media play"),
        A::new("media.pause", CAT, "pause", |c, _| c.tv.key(127).map(|()| "pause".into())).cli("pause", "", "media pause"),
        A::new("media.stop", CAT, "stop", |c, _| c.tv.key(86).map(|()| "stop".into())).cli("stop", "", "media stop"),
        A::new("media.playpause", CAT, "play/pause", playpause).cli("playpause", "", "toggle play/pause"),
        A::new("media.mute", CAT, "mute", mute).cli("mute", "", "mute (no-op if already muted)"),
        A::new("media.visualiser", CAT, "music visualiser: show or hide it", visualiser)
            .cli("visualiser", "[on|off]", "show or hide the music visualiser (Kodi, while it plays music)")
            .long(
                "Shows the music visualiser of the app in front, or goes back from it; with\n\
                 nothing, switches. Kodi has one while it plays music (its visualisation:\n\
                 projectM, Goom, ... whichever it's set to use).",
            )
            .args("[on|off]")
            .while_playing("audio"),
        A::new("media.unmute", CAT, "unmute", unmute).cli("unmute", "", "unmute (no-op if not muted)"),
        A::new("media.ffw", CAT, "fast-forward", ffw).cli("ff", SKIP_USAGE, "fast-forward by an amount").aliases(&["ffw"]).long(SKIP_LONG),
        A::new("media.rew", CAT, "rewind", rew).cli("rw", SKIP_USAGE, "rewind by an amount").aliases(&["rew"]).long(SKIP_LONG),
        A::new("media.seek", CAT, "seek", seek).cli("seek", SKIP_USAGE, "jump to a position (time or percent of the way through)").long(
            "Like ff/rw but absolute. Apps tveee can control directly (Kodi, with its\nremote control on) seek exactly. Otherwise it reads the current position from\nthe TV's media session, then skips the difference. Needs an app that publishes a media session\nwith a position (and a duration for percentages). Approximate: accuracy is\nlimited by --step, so seeking lands within about one step of the target.",
        ),
        A::new("media.volume", CAT, "volume", vol)
            .cli("vol", "up|down|<level>", "change volume")
            .long("A number sets an absolute level with 'media volume'."),
    ]
}

#[cfg(test)]
mod tests {
    #[test]
    fn audio_players_stand_in_for_a_session() {
        use super::*;
        let dump = "  ID:23 -- type:android.media.SoundPool -- u/pid:1000/1733 -- state:started -- attr:AudioAttributes: usage=13\n\
                    \x20 ID:119 -- type:android.media.MediaPlayer -- u/pid:10039/2741 -- state:started -- attr:AudioAttributes: usage=1\n\
                    \x20 ID:15 -- type:android.media.MediaPlayer -- u/pid:10043/1907 -- state:paused -- attr:AudioAttributes: usage=0\n";
        let players = parse_audio_players(dump);
        assert_eq!(players, vec![(1000, "started".into()), (10039, "started".into()), (10043, "paused".into())]);
        let pkg = "org.droidtv.nettvbrowser";
        let s = with_audio_players(vec![], pkg, Some(10039), &players);
        assert!(s.len() == 1 && s[0].package == pkg && s[0].playing());
        assert!(with_audio_players(vec![], "x", Some(10043), &players)[0].paused());
        // nothing playing, unknown uid, or a system uid: no stand-in
        assert!(with_audio_players(vec![], "x", Some(10050), &players).is_empty());
        assert!(with_audio_players(vec![], "x", None, &players).is_empty());
        assert!(with_audio_players(vec![], "x", Some(1000), &players).is_empty());
        // a session that says so already: left as it is
        let own = MediaInfo { package: pkg.into(), state: Some(2), title: Some("t".into()), subtitle: None };
        assert_eq!(with_audio_players(vec![own.clone()], pkg, Some(10039), &players), vec![own]);
    }

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
        assert_eq!(split_description("Rock, Paper, Scissors, Some Channel, null"), (Some("Rock, Paper, Scissors".into()), Some("Some Channel".into())));
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
        let a = |v: &[&str]| v.iter().map(std::string::ToString::to_string).collect::<Vec<_>>();
        assert_eq!(parse_skip_args("ff", &a(&["30s"])).unwrap(), (Amount::Seconds(30.0), 10.0));
        assert_eq!(parse_skip_args("seek", &a(&["50%", "--step", "30s"])).unwrap(), (Amount::Fraction(0.5), 30.0));
        assert!(parse_skip_args("ff", &a(&[])).is_err());
        assert!(parse_skip_args("ff", &a(&["1m", "2m"])).is_err());
        assert!(parse_skip_args("ff", &a(&["1m", "--step"])).is_err());
    }

    #[test]
    fn missing_is_unknown() {
        assert_eq!(parse_muted("nothing here"), None);
    }
}

#[cfg(test)]
mod mute_tests {

    #[test]
    fn visualiser_asked() {
        let a = |v: &[&str]| v.iter().map(std::string::ToString::to_string).collect::<Vec<_>>();
        assert_eq!(super::visualiser_want(&a(&[])).unwrap(), None);
        assert_eq!(super::visualiser_want(&a(&["on"])).unwrap(), Some(true));
        assert_eq!(super::visualiser_want(&a(&["off"])).unwrap(), Some(false));
        assert!(super::visualiser_want(&a(&["loud"])).is_err());
    }

    use super::*;

    #[test]
    fn mute_says_what_the_tv_says() {
        assert_eq!(mute_said(true, Some(true)).unwrap().text, "muted");
        assert_eq!(mute_said(false, Some(false)).unwrap().text, "unmuted");
        // the key lost (standby): not "muted"
        assert!(mute_said(true, Some(false)).is_err());
        assert!(mute_said(true, None).unwrap().text.starts_with("sent the mute key"));
    }
}

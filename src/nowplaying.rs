//! A human description of what the TV is doing ("Watching YouTube: ..."),
//! pieced together from the foreground app, its media session and, for apps
//! that have one, the page their devtools socket reports.
//!
//! What an app is called and how to read a link out of its page lives with the
//! app itself (`plugins::apps::all`); this module only combines the clues.

use crate::devtools::{self, Progress};
use crate::plugins::apps::{self, Kind};
use crate::plugins::media::MediaInfo;
use crate::tv::Tv;

#[derive(Debug, PartialEq)]
pub struct Activity {
    /// e.g. "Watching YouTube"
    pub what: String,
    /// e.g. "Cunk on Cinema | BFI Q&A (BFI)": the title, then the channel/artist
    pub title: Option<String>,
    pub url: Option<String>,
    pub progress: Option<Progress>,
}

/// The session belonging to `pkg`, preferring one that is playing or paused.
pub fn session_for<'a>(pkg: &str, sessions: &'a [MediaInfo]) -> Option<&'a MediaInfo> {
    let mine = || sessions.iter().filter(|s| s.package == pkg);
    mine().find(|s| s.playing() || s.paused()).or_else(|| mine().next())
}

fn title_of(s: &MediaInfo) -> Option<String> {
    let title = s.title.as_deref()?;
    Some(match &s.subtitle {
        Some(sub) => format!("{title} ({sub})"),
        None => title.to_string(),
    })
}

/// The `what` and `title` lines, or None when there is nothing useful to say.
/// `app` is the app's name and kind ([`apps::identify`]), if known or guessed.
pub fn describe(pkg: &str, app: Option<(&str, Kind)>, session: Option<&MediaInfo>) -> Option<(String, Option<String>)> {
    let active = session.filter(|s| s.playing() || s.paused());
    let title = active.and_then(title_of);
    let Some((name, kind)) = app else {
        // unknown app: only worth saying if it says what it is playing
        title.as_ref()?;
        let verb = if active?.paused() { "Paused" } else { "Playing" };
        return Some((format!("{verb} in {pkg}"), title));
    };
    let what = match (kind, active) {
        (Kind::Launcher, _) => return Some((format!("On {name}"), None)),
        (Kind::LiveTv, _) => format!("Watching {name}"),
        (Kind::Video | Kind::Player, Some(s)) if s.paused() => format!("Paused in {name}"),
        (Kind::Video | Kind::Player, Some(_)) => format!("Watching {name}"),
        (Kind::Music, Some(s)) if s.paused() => format!("Paused in {name}"),
        (Kind::Music, Some(_)) => format!("Listening to {name}"),
        (Kind::Video | Kind::Music, None) => format!("Browsing {name}"),
        // any app that is playing something
        (_, Some(s)) if s.paused() => format!("Paused in {name}"),
        (_, Some(_)) => format!("Playing in {name}"),
        (Kind::Player | Kind::Store | Kind::Other, None) => format!("Using {name}"),
    };
    Some((what, title))
}

/// `m:ss`, or `h:mm:ss` from an hour up.
pub fn clock(secs: f64) -> String {
    let t = secs.max(0.0) as u64;
    let (h, m, s) = (t / 3600, t / 60 % 60, t % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

/// `━━━━┃─────`: `width` cells with the marker at `frac` of the way along.
pub fn bar(frac: f64, width: usize) -> String {
    let at = ((frac.clamp(0.0, 1.0) * width as f64) as usize).min(width.saturating_sub(1));
    format!("{}┃{}", "━".repeat(at), "─".repeat(width.saturating_sub(at + 1)))
}

/// `━━━━━━━━┃───────── 25:16 / 27:31 (92%)`, or `25:16 (live)` without a duration.
pub fn progress_bar(p: &Progress, width: usize) -> String {
    let Some(dur) = p.duration else { return format!("{} (live)", clock(p.position)) };
    let frac = (p.position / dur).clamp(0.0, 1.0);
    format!("{} {} / {} ({}%)", bar(frac, width), clock(p.position), clock(dur), (frac * 100.0).floor())
}

/// Everything `status` knows how to say about the foreground app `pkg`.
pub fn activity(tv: &Tv, pkg: &str, sessions: &[MediaInfo]) -> Option<Activity> {
    // only look the app up on the TV (cached a day) when tvbee doesn't know it
    let who = match apps::by_package(pkg) {
        Some(a) => Some((a.name.to_string(), a.kind)),
        None => apps::identify(pkg, &apps::guesses(tv)),
    };
    activity_with(pkg, who, sessions, || devtools::pages(tv, pkg))
}

/// The app's devtools hook, if it has one (only then is `pages` worth calling).
#[cfg_attr(not(feature = "ui"), allow(dead_code))]
pub fn has_devtools(pkg: &str) -> bool {
    apps::by_package(pkg).is_some_and(|a| a.page_url.is_some())
}

/// As [`activity`], with the devtools pages coming from `pages` (called only
/// when the app has a devtools hook and something is playing or paused).
pub fn activity_with(
    pkg: &str,
    who: Option<(String, Kind)>,
    sessions: &[MediaInfo],
    pages: impl FnOnce() -> Vec<devtools::Page>,
) -> Option<Activity> {
    let app = apps::by_package(pkg);
    let session = session_for(pkg, sessions);
    let (what, title) = describe(pkg, who.as_ref().map(|(n, k)| (n.as_str(), *k)), session)?;
    // the devtools round trip is only worth it while something is on
    let (mut url, mut progress) = (None, None);
    let on = session.filter(|s| s.playing() || s.paused());
    if let (Some(link), Some(_)) = (app.and_then(|a| a.page_url), on) {
        let pages = pages();
        url = pages.iter().find_map(|p| link(&p.url));
        progress = pages.iter().find_map(|p| p.progress);
    }
    Some(Activity { what, title, url, progress })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn yt() -> (&'static str, Kind) {
        ("YouTube", Kind::Video)
    }

    fn session(state: u32, title: Option<&str>, sub: Option<&str>) -> MediaInfo {
        MediaInfo {
            package: "com.google.android.youtube.tv".into(),
            state: Some(state),
            title: title.map(Into::into),
            subtitle: sub.map(Into::into),
        }
    }

    const PKG: &str = "com.google.android.youtube.tv";

    #[test]
    fn video_apps() {
        let s = session(3, Some("Cunk on Cinema"), Some("BFI"));
        assert_eq!(describe(PKG, Some(yt()), Some(&s)).unwrap(), ("Watching YouTube".into(), Some("Cunk on Cinema (BFI)".into())));
        let s = session(2, Some("Cunk on Cinema"), None);
        assert_eq!(describe(PKG, Some(yt()), Some(&s)).unwrap(), ("Paused in YouTube".into(), Some("Cunk on Cinema".into())));
        let s = session(1, Some("old"), None);
        assert_eq!(describe(PKG, Some(yt()), Some(&s)).unwrap(), ("Browsing YouTube".into(), None));
        assert_eq!(describe(PKG, Some(yt()), None).unwrap(), ("Browsing YouTube".into(), None));
    }

    #[test]
    fn home_and_unknown_apps() {
        let home = Some(("the home screen", Kind::Launcher));
        assert_eq!(describe("com.spocky.projengmenu", home, None).unwrap(), ("On the home screen".into(), None));
        // guessed kinds
        assert_eq!(describe("nextapp.fx", Some(("nextapp.fx", Kind::Player)), None).unwrap().0, "Using nextapp.fx");
        assert_eq!(describe("com.g", Some(("com.g", Kind::Other)), None).unwrap().0, "Using com.g");
        let mut m = session(3, Some("Song"), Some("Band"));
        m.package = "com.spotify.tv.android".into();
        assert_eq!(
            describe("com.spotify.tv.android", Some(("Spotify", Kind::Music)), Some(&m)).unwrap(),
            ("Listening to Spotify".into(), Some("Song (Band)".into()))
        );
        let mut s = session(3, Some("Song"), None);
        s.package = "com.g".into();
        assert_eq!(describe("com.g", Some(("com.g", Kind::Other)), Some(&s)).unwrap().0, "Playing in com.g");
        assert_eq!(describe("com.example.x", None, None), None);
        let mut s = session(3, Some("Song"), Some("Band"));
        s.package = "com.example.x".into();
        assert_eq!(
            describe("com.example.x", None, Some(&s)).unwrap(),
            ("Playing in com.example.x".into(), Some("Song (Band)".into()))
        );
    }

    #[test]
    fn prefers_the_active_session() {
        let ss = [session(1, Some("stale"), None), session(3, Some("live"), None)];
        assert_eq!(session_for(PKG, &ss).unwrap().title.as_deref(), Some("live"));
        assert!(session_for("other", &ss).is_none());
    }

    #[test]
    fn bars() {
        let p = |pos, dur| Progress { position: pos, duration: dur, paused: false };
        assert_eq!(progress_bar(&p(1516.3, Some(1651.0)), 10), "━━━━━━━━━┃ 25:16 / 27:31 (91%)");
        assert_eq!(progress_bar(&p(0.0, Some(60.0)), 4), "┃─── 0:00 / 1:00 (0%)");
        assert_eq!(progress_bar(&p(30.0, Some(60.0)), 4), "━━┃─ 0:30 / 1:00 (50%)");
        assert_eq!(progress_bar(&p(3725.0, None), 4), "1:02:05 (live)");
    }
}

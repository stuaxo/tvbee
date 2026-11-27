//! Snapshot tests: the screen drawn from fixed states, at a desktop size
//! (120x35) and a corner pane (60x16), kept in `snapshots/` and checked with
//! `insta` (`cargo insta review` to look at a change).

use super::*;
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use std::sync::Mutex;
use tveee_core::devtools::Progress;
use tveee_core::nowplaying::{Field, Place};

const DESKTOP: (u16, u16) = (120, 35);
const CORNER: (u16, u16) = (60, 16);

/// A screen on this state, with no engine behind it (what's asked for goes
/// nowhere).
fn app(st: TvState) -> App {
    let (tx, _) = std::sync::mpsc::channel();
    let handle = Handle::new(tx, Arc::new(Mutex::new(Arc::new(st))));
    App::new("192.168.1.19".into(), handle, Vec::new(), true)
}

/// The screen as text, one line per row (trailing spaces trimmed).
fn screen(app: &mut App, (w, h): (u16, u16)) -> String {
    let mut term = Terminal::new(TestBackend::new(w, h)).expect("a test terminal");
    term.draw(|f| app.draw(f)).expect("drawn");
    let buf = term.backend().buffer();
    (0..h)
        .map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect::<String>().trim_end().to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Kodi, paused part way through an episode (paused: the bar doesn't move
/// with the clock).
fn kodi_paused() -> TvState {
    let mut st = TvState::seeded(None);
    let mut a = Activity::just("Paused in Kodi");
    a.title = Some("Episode 3 (Off-Road)".into());
    a.progress = Some(Progress { position: 27.0 * 60.0 + 56.0, duration: Some(48.0 * 60.0 + 47.0), paused: true });
    a.fields.push(Field { label: "audio", value: "AAC-LC  AAC".into(), place: Place::Bottom, open: None });
    st.snap = Some(Snapshot {
        activity: Some(a),
        volume: Some((24, 60)),
        muted: Some(false),
        exact: true,
        power: Some("Awake".into()),
        focus: Some("org.xbmc.kodi/org.xbmc.kodi.Main".into()),
        kind: Some("player".into()),
        ..Snapshot::default()
    });
    st
}

/// In standby.
fn standby() -> TvState {
    let mut st = TvState::seeded(None);
    st.snap = Some(Snapshot { activity: Some(Activity::just("In standby")), standby: true, volume: Some((24, 60)), ..Snapshot::default() });
    st
}

#[test]
fn kodi_paused_desktop() {
    insta::assert_snapshot!(screen(&mut app(kodi_paused()), DESKTOP));
}

#[test]
fn kodi_paused_corner() {
    insta::assert_snapshot!(screen(&mut app(kodi_paused()), CORNER));
}

#[test]
fn standby_desktop() {
    insta::assert_snapshot!(screen(&mut app(standby()), DESKTOP));
}

#[test]
fn nothing_read_yet_corner() {
    insta::assert_snapshot!(screen(&mut app(TvState::seeded(None)), CORNER));
}

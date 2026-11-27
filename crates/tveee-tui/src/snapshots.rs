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
    app_with(st, Vec::new())
}

fn app_with(st: TvState, actions: Vec<ActionDescriptor>) -> App {
    // the plugins' providers, as the binary has them (always: tests run in
    // any order)
    super::tests::install_all();
    let (tx, _) = std::sync::mpsc::channel();
    let handle = Handle::new(tx, Arc::new(Mutex::new(Arc::new(st))));
    App::new("192.168.1.19".into(), handle, actions, true)
}

/// Some actions as plugins describe them, for the palette and menu.
fn actions() -> Vec<ActionDescriptor> {
    use tveee_core::actions::{Cx, Output};
    fn ok(_: &Cx, _: &[String]) -> tveee_core::error::Result<Output> {
        Ok(Output::default())
    }
    vec![
        ActionDescriptor::new("kodi.subtitles", "kodi", "kodi: next subtitles", ok).key('k'),
        ActionDescriptor::new("kodi.audio", "kodi", "kodi: next audio track", ok).key('a'),
        ActionDescriptor::new("kodi.find", "kodi", "kodi: find in the library", ok).text("Find in Kodi:", "a title"),
        ActionDescriptor::new("media.volume", "media", "volume", ok).cli("vol", "up|down|<level>", "change volume"),
        ActionDescriptor::new("power.sleep", "power", "sleep (standby)", ok).key('s'),
        ActionDescriptor::new("power.off", "power", "power off", ok).key('X').confirm("Power off the TV?"),
    ]
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
    a.media = Some("video");
    st.snap = Some(Snapshot {
        activity: Some(a),
        volume: Some((24, 60)),
        muted: Some(false),
        exact: true,
        power: Some("Awake".into()),
        focus: Some("org.xbmc.kodi/org.xbmc.kodi.Main".into()),
        kind: Some("player".into()),
        system: tveee_core::builtin::info::System {
            load: Some((4.82, 3.10, 2.40)),
            mem: Some(tveee_core::builtin::info::Mem { total: 1_374_208, free: 60_000, available: 317_440, swap_total: 524_288, swap_free: 200_000 }),
            ..Default::default()
        },
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

#[test]
fn palette_desktop() {
    let mut a = app_with(kodi_paused(), actions());
    a.open_palette();
    insta::assert_snapshot!("palette_all", screen(&mut a, DESKTOP));
    a.prompt = "kodi".into();
    insta::assert_snapshot!("palette_kodi", screen(&mut a, DESKTOP));
}

#[test]
fn palette_asks_for_arguments() {
    let mut a = app_with(kodi_paused(), actions());
    a.overlay = Overlay::Prompt("media.volume");
    insta::assert_snapshot!(screen(&mut a, CORNER));
}

#[test]
fn palette_lists_the_ui_too() {
    let mut a = app_with(kodi_paused(), actions());
    a.open_palette();
    a.prompt = "log".into();
    insta::assert_snapshot!(screen(&mut a, CORNER));
}

#[test]
fn guide_filter_ctrl_u_clears() {
    let mut a = app(standby());
    let (tx, _) = std::sync::mpsc::channel();
    let h = Handle::new(tx, Arc::new(Mutex::new(Arc::new(TvState::seeded(None)))));
    let tab = GuideTab::All;
    for c in "bbc".chars() {
        a.guide_key(tab, 0, KeyCode::Char(c), KeyModifiers::NONE, &h);
    }
    assert_eq!(a.prompt, "bbc");
    // ctrl+u clears it, and no "u" is typed
    a.guide_key(tab, 0, KeyCode::Char('u'), KeyModifiers::CONTROL, &h);
    assert_eq!(a.prompt, "");
}

#[test]
fn guide_ctrl_p_opens_the_commands() {
    let mut a = app(standby());
    let (tx, _) = std::sync::mpsc::channel();
    let h = Handle::new(tx, Arc::new(Mutex::new(Arc::new(TvState::seeded(None)))));
    a.overlay = Overlay::Guide { tab: GuideTab::All, at: 0 };
    // ":" filters (programme names have them)
    a.guide_key(GuideTab::All, 0, KeyCode::Char(':'), KeyModifiers::NONE, &h);
    assert_eq!(a.prompt, ":");
    a.guide_key(GuideTab::All, 0, KeyCode::Char('p'), KeyModifiers::CONTROL, &h);
    assert!(matches!(a.overlay, Overlay::Palette(_)), "{:?}", a.overlay);
}

/// Kodi playing from a catch-up add-on: a very long link.
fn kodi_catchup() -> TvState {
    let mut st = kodi_paused();
    if let Some(a) = st.snap.as_mut().and_then(|s| s.activity.as_mut()) {
        a.url = Some(
            "plugin://plugin.video.catchuptvandmore/resources/lib/channels/uk/my5/get_video_url/?item_id=%22episode-12345678%22&video_id=%22C5-2026-11-27-the-long-programme-title%22&download_mode=false&token=abcdef0123456789"
                .into(),
        );
    }
    st
}

#[test]
fn long_link_shortened_corner() {
    let mut a = app(kodi_catchup());
    let s = screen(&mut a, CORNER);
    insta::assert_snapshot!(s);
    // one row, cut in the middle, made a link to the whole thing
    assert_eq!(a.links.len(), 1);
    assert!(a.links[0].text.contains('…'));
    assert!(a.links[0].url.ends_with("token=abcdef0123456789"));
}

#[test]
fn long_link_whole_desktop() {
    let mut a = app(kodi_catchup());
    a.url_full = true;
    let s = screen(&mut a, DESKTOP);
    insta::assert_snapshot!(s);
    // every row of it, and the open button, are links to it
    assert_eq!(a.links.iter().filter(|l| l.text != "[open ↗]").map(|l| l.text.as_str()).collect::<String>(), a.links[0].url);
    assert!(a.links.iter().any(|l| l.text == "[open ↗]"));
    // its buttons answer clicks
    assert!(a.hits.iter().any(|(_, h)| *h == Hit::Input(Input::CopyUrl)));
    assert!(a.hits.iter().any(|(_, h)| *h == Hit::Input(Input::OpenUrl)));
}

#[test]
fn help_fits_the_corner() {
    let mut a = app(standby());
    a.overlay = Overlay::Help(0);
    let s = screen(&mut a, CORNER);
    insta::assert_snapshot!(s);
    // nothing runs past the box, and it says it scrolls
    assert!(s.lines().all(|l| l.chars().count() <= usize::from(CORNER.0)));
    assert!(s.contains("↑/↓ scroll"));
    // scrolled to the end: the last of it shows, and ↑ moves straight away
    a.overlay = Overlay::Help(999);
    let end = screen(&mut a, CORNER);
    assert!(end.contains("Shift+drag selects text"), "{end}");
    let Overlay::Help(top) = a.overlay else { panic!("help closed") };
    let (up, _) = overlays::overlay_key(a.overlay, KeyCode::Up, &[]);
    assert_eq!(up, Overlay::Help(top - 1));
}

#[test]
fn help_whole_on_the_desktop() {
    // wide enough: the two columns as written
    let lines = overlays::help_lines(116);
    assert_eq!(lines.len(), overlays::HELP.lines().count());
    // narrow: no row too long, and no words lost
    let narrow = overlays::help_lines(56);
    assert!(narrow.iter().all(|l| l.chars().count() <= 56), "{narrow:#?}");
    let words = |t: &str| t.split_whitespace().map(str::to_string).collect::<Vec<_>>();
    assert_eq!(words(&narrow.join(" ")), words(overlays::HELP));
}

#[test]
fn apps_hints_fit_the_corner() {
    let mut a = app(standby());
    a.overlay = Overlay::Apps { tab: AppTab::Known, sel: 0 };
    let s = screen(&mut a, CORNER);
    insta::assert_snapshot!(s);
    assert!(s.contains("F3 remote"), "the last hint is cut off:\n{s}");
}

#[test]
fn guide_error_said_plainly_corner() {
    let mut st = standby();
    st.guide = Some(Arc::new(Err(
        "https https://192.168.1.19:1926/6/channeldb/tv/channelLists/all: curl: (7) Failed to connect to 192.168.1.19 port 1926 after 26 ms: Could not connect to server".into(),
    )));
    let mut a = app(st);
    a.overlay = Overlay::Guide { tab: GuideTab::All, at: 0 };
    let s = screen(&mut a, CORNER);
    insta::assert_snapshot!(s);
    assert!(!s.contains("curl") && s.contains("didn't answer"), "{s}");
}

#[test]
fn memory_graph_zooms_when_it_stays_high() {
    use super::drawing::graph_floor;
    // up and down from low: from 0
    assert_eq!(graph_floor(&[300, 900, 600], 1342), 0);
    // stuck near the top: just under the lowest
    let f = graph_floor(&[1290, 1309, 1300], 1342);
    assert!(f > 1000 && f < 1290 && f.is_multiple_of(50), "{f}");
    assert_eq!(graph_floor(&[], 1342), 0);
}

#[test]
fn first_frame_from_last_time_desktop() {
    use tveee_core::engine::last::{self, LastSeen};
    let seen = LastSeen { what: "Watching YouTube".into(), title: Some("A video from last time".into()), volume: Some((24, 60)), at: last::now() - 300 };
    let s = screen(&mut app(TvState::seeded(Some(&seen))), DESKTOP);
    insta::assert_snapshot!(s);
    assert!(s.contains("(5 min ago; reading the TV...)"), "{s}");
    assert!(!s.contains("waiting for the TV"));
}

#[test]
fn standby_is_kept_for_next_time() {
    use tveee_core::engine::last;
    let st = standby();
    let l = last::of(st.snap.as_ref().expect("a snapshot")).expect("kept");
    assert!(l.what.to_lowercase().contains("standby"), "{l:?}");
    assert_eq!(l.title, None);
}

/// Kodi paused, with everything the system panel shows (disks, a minute
/// of memory readings stuck near full).
fn kodi_busy() -> TvState {
    use tveee_core::builtin::info::Disk;
    let mut st = kodi_paused();
    if let Some(s) = st.snap.as_mut() {
        s.system.uptime_secs = Some(3.0 * 3600.0 + 40.0 * 60.0);
        s.system.disks = vec![
            Disk { mount: "/data".into(), total_kb: 3_774_873, used_kb: 3_670_016, avail_kb: 103_424 },
            Disk { mount: "/mnt/expand/1234".into(), total_kb: 732_200_000, used_kb: 39_400_000, avail_kb: 692_800_000 },
        ];
    }
    st.mem_history = (0..60).map(|i| 1290 + (i * 7) % 20).collect();
    st
}

#[test]
fn side_column_desktop() {
    let mut a = app(kodi_busy());
    let s = screen(&mut a, DESKTOP);
    insta::assert_snapshot!(s);
    // the system panel shows by itself on a wide screen, beside what's playing
    let row = s.lines().find(|l| l.contains("Paused in Kodi")).expect("what's playing");
    assert!(row.contains("system - i hides"), "{s}");
    // and i hides it
    let h = Handle::new(std::sync::mpsc::channel().0, Arc::new(Mutex::new(Arc::new(TvState::seeded(None)))));
    a.apply(Input::ToggleInfo, &h);
    assert!(!screen(&mut a, DESKTOP).contains("system - i hides"));
}

#[test]
fn side_column_remote_desktop() {
    let mut a = app(kodi_busy());
    a.mode = Mode::Remote;
    let s = screen(&mut a, DESKTOP);
    insta::assert_snapshot!(s);
    // the D-pad's keys answer clicks
    assert!(a.hits.iter().any(|(_, h)| *h == Hit::Input(Input::Do(Action::Key(19)))));
    assert!(a.hits.iter().any(|(_, h)| *h == Hit::Input(Input::Do(Action::Volume(1)))));
}

#[test]
fn no_side_column_in_the_corner() {
    // a small pane: the system panel only when asked for, below
    let s = screen(&mut app(kodi_busy()), CORNER);
    assert!(!s.contains("system - i hides"), "{s}");
}

#[test]
fn kodi_buttons_desktop() {
    let mut a = app_with(kodi_paused(), actions());
    let s = screen(&mut a, DESKTOP);
    insta::assert_snapshot!(s);
    // Kodi's own actions (menu keys), then search; the power ones aren't Kodi's
    let row = s.lines().find(|l| l.contains(" do ") || l.starts_with("│do ")).expect("a row of buttons");
    assert!(row.contains(".k") && row.contains("next subtitles") && row.contains("search"), "{row}");
    assert!(!row.contains("sleep"), "{row}");
    // clicking one runs it, as the menu would
    let i = a.menu.iter().position(|d| d.id == "kodi.subtitles").expect("in the menu");
    assert!(a.hits.iter().any(|(_, h)| *h == Hit::Chip(i)));
}

#[test]
fn live_tv_buttons_corner() {
    let mut st = TvState::seeded(None);
    let mut act = Activity::just("Watching live TV");
    act.title = Some("This is BBC Three".into());
    st.snap = Some(Snapshot {
        activity: Some(act),
        volume: Some((24, 60)),
        focus: Some("org.droidtv.zapster/org.droidtv.zapster.playtv.activity.PlayTvActivity".into()),
        kind: Some("live-tv".into()),
        ..Snapshot::default()
    });
    let s = screen(&mut app(st), CORNER);
    insta::assert_snapshot!(s);
    assert!(s.contains(" g ") && s.contains("guide"), "{s}");
}

#[test]
fn no_buttons_in_standby() {
    let s = screen(&mut app(standby()), DESKTOP);
    assert!(!s.contains("│do "), "{s}");
}

#[test]
fn kodi_music_has_no_subtitles() {
    // Kodi's music view: the same app, audio playing
    let mut st = kodi_paused();
    if let Some(a) = st.snap.as_mut().and_then(|s| s.activity.as_mut()) {
        a.what = "Listening in Kodi".into();
        a.media = Some("audio");
        a.fields.clear();
    }
    // Kodi's own actions, as the plugin describes them
    let mut a = app_with(st.clone(), tveee_plugin_kodi::actions());
    let s = screen(&mut a, DESKTOP);
    let row = s.lines().find(|l| l.starts_with("│do ")).expect("a row of buttons");
    assert!(!row.contains("subtitles") && !row.contains("audio track"), "{row}");
    // music: its visualiser is offered
    assert!(row.contains(" z ") && row.contains("visualiser"), "{row}");
    // a video: they're back
    if let Some(x) = st.snap.as_mut().and_then(|s| s.activity.as_mut()) {
        x.media = Some("video");
    }
    let s = screen(&mut app_with(st, tveee_plugin_kodi::actions()), DESKTOP);
    assert!(s.lines().any(|l| l.starts_with("│do ") && l.contains("next subtitles")), "{s}");
    // and no visualiser: it's for music
    assert!(!s.contains("visualiser"), "{s}");
    assert!(row.contains("search"), "{row}");
}

/// The plugins as `tveee plugins` would have them: some built in, the
/// vendor ones on or off.
fn plugin_states() -> Vec<tveee_core::plugin::PluginState> {
    use tveee_core::plugin::{PluginState, Source};
    let p = |name, enabled, source, desc| PluginState { name, desc, enabled, source };
    vec![
        p("apps", true, Source::Builtin, "launching and managing Android apps"),
        p("media", true, Source::Builtin, "playback keys, volume and mute"),
        p("power", true, Source::Builtin, "sleep, wake, reboot, shutdown"),
        p("philips", true, Source::Cache, "Philips TV features via the JointSpace API"),
        p("kodi", true, Source::Override, "Kodi: what it's playing, exact seeking"),
        p("youtube", false, Source::Cache, "the YouTube app: what's on, search"),
    ]
}

#[test]
fn options_plugins_tab_desktop() {
    // what's saved: kodi chosen on; nothing read from the real file
    tveee_core::options::preset(vec![("plugin.kodi".to_string(), true)]);
    let mut a = app(kodi_paused());
    a.plugins = plugin_states();
    a.overlay = Overlay::Options(0);
    let opts = screen(&mut a, DESKTOP);
    assert!(opts.contains("scroll animations") && opts.contains(" Plugins "), "{opts}");
    // to the plugins tab
    a.options_key(0, KeyCode::Tab);
    let s = screen(&mut a, DESKTOP);
    insta::assert_snapshot!(s);
    let row = |name: &str| s.lines().find(|l| l.contains(&format!(" {name} "))).unwrap_or_default().to_string();
    // ticked as the options are: kodi chosen on, philips on (auto), youtube off (auto)
    assert!(row("kodi").contains("[x] kodi") && !row("kodi").contains("(auto)"), "{s}");
    assert!(row("philips").contains("[x] philips") && row("philips").contains("(auto)"), "{s}");
    assert!(row("youtube").contains("[ ] youtube") && row("youtube").contains("(auto)"), "{s}");
    // the built-in ones are listed ticked, not chosen
    assert!(s.contains("built in (always on)") && row("media").contains("[x] media"), "{s}");
    let rows = a.hits.iter().filter(|(_, h)| matches!(h, Hit::OptionItem(_))).count();
    assert_eq!(rows, 3, "only the vendor plugins can be chosen");
    // ↓ moves among them; Tab goes back
    a.options_key(0, KeyCode::Down);
    assert_eq!(a.overlay, Overlay::Options(1));
    a.options_key(1, KeyCode::Tab);
    assert!(!a.options_plugins);
}

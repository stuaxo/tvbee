//! Snapshot tests: the screen drawn from fixed states, at a desktop size
//! (120x35) and a corner pane (60x16), kept in `snapshots/` and checked with
//! `insta` (`cargo insta review` to look at a change).

use super::*;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
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
    let mut a = App::new("192.168.1.19".into(), handle, actions, true);
    // the options as they come: not the real file's
    a.opts = Some(Vec::new());
    a
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
    (0..h).map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect::<String>().trim_end().to_string()).collect::<Vec<_>>().join("\n")
}

/// Kodi, paused part way through an episode (paused: the bar doesn't move
/// with the clock).
fn kodi_paused() -> TvState {
    let mut st = TvState::seeded(None);
    let mut a = Activity::just("Paused in Kodi");
    a.title = Some("Episode 3 (Off-Road)".into());
    a.progress = Some(Progress { position: 27.0 * 60.0 + 56.0, duration: Some(48.0 * 60.0 + 47.0), paused: true, rate: 1.0 });
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
    // Esc: the filter first, the guide staying open; then the guide
    a.overlay = Overlay::Guide { tab, at: 0 };
    a.guide_key(tab, 0, KeyCode::Char('b'), KeyModifiers::NONE, &h);
    a.guide_key(tab, 0, KeyCode::Esc, KeyModifiers::NONE, &h);
    assert!(a.prompt.is_empty() && matches!(a.overlay, Overlay::Guide { .. }));
    a.guide_key(tab, 0, KeyCode::Esc, KeyModifiers::NONE, &h);
    assert_eq!(a.overlay, Overlay::None);
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
    // nothing runs past the box, and it says how far down it is
    assert!(s.lines().all(|l| l.chars().count() <= usize::from(CORNER.0)));
    assert!(s.contains("1-14 of"), "{s}");
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
    let mut a = app(kodi_paused());
    // what's saved: kodi chosen on (this app's own options)
    a.opts = Some(vec![("plugin.kodi".to_string(), true)]);
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
    assert!(!a.options.plugins);
}

#[test]
fn palette_pages_and_ends() {
    let mut a = app_with(kodi_paused(), actions());
    let h = Handle::new(std::sync::mpsc::channel().0, Arc::new(Mutex::new(Arc::new(TvState::seeded(None)))));
    let n = a.palette_list().len();
    assert!(n > 10, "enough to page through: {n}");
    a.overlay = Overlay::Palette(0);
    a.palette_key(0, KeyCode::End, KeyModifiers::NONE, &h);
    assert_eq!(a.overlay, Overlay::Palette(n - 1));
    a.palette_key(n - 1, KeyCode::PageUp, KeyModifiers::NONE, &h);
    assert_eq!(a.overlay, Overlay::Palette(n - 1 - super::listnav::PAGE));
    a.palette_key(3, KeyCode::Home, KeyModifiers::NONE, &h);
    assert_eq!(a.overlay, Overlay::Palette(0));
}

#[test]
fn load_hidden_and_system_watched() {
    // load & memory off: not on the top line
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = Handle::new(tx, Arc::new(Mutex::new(Arc::new(kodi_paused()))));
    let mut a = App::new("192.168.1.19".into(), handle, Vec::new(), true);
    a.opts = Some(vec![(tveee_core::options::SHOW_LOAD.to_string(), false)]);
    let s = screen(&mut a, CORNER);
    let top = s.lines().nth(1).unwrap_or_default();
    assert!(!top.contains("load") && !top.contains("310 MB") && top.contains("[F]ree memory"), "{top}");
    // the system panel opened: the engine is told to read the figures; closed: told to stop
    a.info = Some(true);
    let _ = screen(&mut a, CORNER);
    assert!(rx.try_iter().any(|x| x == Action::WatchSystem(true)));
    a.info = Some(false);
    let _ = screen(&mut a, CORNER);
    assert!(rx.try_iter().any(|x| x == Action::WatchSystem(false)));
    // toggled back on in the options: shown at once (from the figures it
    // has), and a read asked for straight away
    let row = tveee_core::options::OPTIONS.iter().position(|o| o.name == tveee_core::options::SHOW_LOAD).expect("the option");
    a.toggle_option(row);
    assert!(rx.try_iter().any(|x| x == Action::Refresh));
    // (the options closed: they'd cover the top line)
    a.overlay = Overlay::None;
    let s = screen(&mut a, CORNER);
    assert!(s.lines().nth(1).unwrap_or_default().contains("load 4.82"), "{s}");
    // and off again: gone at once
    a.toggle_option(row);
    a.overlay = Overlay::None;
    let s = screen(&mut a, CORNER);
    assert!(!s.lines().nth(1).unwrap_or_default().contains("load"), "{s}");
}

#[test]
fn palette_over_the_log() {
    let (tx, _rx) = std::sync::mpsc::channel();
    let h = Handle::new(tx, Arc::new(Mutex::new(Arc::new(kodi_paused()))));
    let mut a = app_with(kodi_paused(), actions());
    a.overlay = Overlay::Logs { at: None, from: None };
    // : opens the commands over the log, which stays drawn under them
    assert!(a.palette_wanted(KeyCode::Char(':'), KeyModifiers::NONE));
    a.palette_over();
    assert_eq!(a.overlay, Overlay::Palette(0));
    let s = screen(&mut a, DESKTOP);
    assert!(s.contains("commands") && s.contains("log"), "{s}");
    // Esc: back to the log
    a.palette_key(0, KeyCode::Esc, KeyModifiers::NONE, &h);
    assert_eq!(a.overlay, Overlay::Logs { at: None, from: None });
    // over the guide, its filter kept: : is typed there, ^P opens them
    a.overlay = Overlay::Guide { tab: GuideTab::All, at: 0 };
    a.prompt = "bbc".into();
    assert!(!a.palette_wanted(KeyCode::Char(':'), KeyModifiers::NONE));
    assert!(a.palette_wanted(KeyCode::Char('p'), KeyModifiers::CONTROL));
    a.palette_over();
    assert!(a.prompt.is_empty());
    a.palette_key(0, KeyCode::Esc, KeyModifiers::NONE, &h);
    assert_eq!((a.overlay, a.prompt.as_str()), (Overlay::Guide { tab: GuideTab::All, at: 0 }, "bbc"));
    // the palette on its own, closed: nothing comes back
    a.overlay = Overlay::None;
    a.open_palette();
    a.palette_key(0, KeyCode::Esc, KeyModifiers::NONE, &h);
    assert_eq!(a.overlay, Overlay::None);
}

#[test]
fn log_select_and_save() {
    tveee_core::log::keep_recent();
    let mark = format!("log-test-{}", std::process::id());
    for i in 0..5 {
        tveee_core::tlog!("{mark} line {i}");
    }
    let (first, lines) = tveee_core::log::recent_numbered();
    let n0 = first + lines.iter().position(|l| l.contains(&format!("{mark} line 0"))).expect("logged");
    let mut a = app(kodi_paused());
    a.overlay = Overlay::Logs { at: Some(n0), from: None };
    // shift+↓ twice: three lines selected, from line 0
    a.logs_key(Some(n0), None, KeyCode::Down, KeyModifiers::SHIFT);
    let Overlay::Logs { at, from } = a.overlay else { panic!() };
    a.logs_key(at, from, KeyCode::Down, KeyModifiers::SHIFT);
    assert_eq!(a.overlay, Overlay::Logs { at: Some(n0 + 2), from: Some(n0) });
    let s = screen(&mut a, DESKTOP);
    assert!(s.contains("3 lines selected"), "{s}");
    let (first, lines) = tveee_core::log::recent_numbered();
    let picked = super::logview::lines_between(first, &lines, n0, n0 + 2);
    assert!(
        picked.contains(&format!("{mark} line 0")) && picked.contains(&format!("{mark} line 2")) && !picked.contains(&format!("{mark} line 3")),
        "{picked}"
    );
    // esc lets go of the selection, then closes
    a.logs_key(Some(n0 + 2), Some(n0), KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(a.overlay, Overlay::Logs { at: Some(n0 + 2), from: None });
    // s: a file name asked for (one offered), saved, back to the log
    a.logs_key(Some(n0 + 2), None, KeyCode::Char('s'), KeyModifiers::NONE);
    assert_eq!(a.overlay, Overlay::SaveLog);
    assert!(a.prompt.starts_with("tveee-log-") && a.prompt.ends_with(".txt"), "{}", a.prompt);
    let path = std::env::temp_dir().join(format!("{mark}.txt"));
    let _ = std::fs::remove_file(&path);
    a.prompt = path.display().to_string();
    a.save_log_key(KeyCode::Enter, KeyModifiers::NONE);
    let saved = std::fs::read_to_string(&path).expect("saved");
    assert!(saved.contains(&format!("{mark} line 4")), "{saved}");
    assert_eq!(a.overlay, Overlay::Logs { at: Some(n0 + 2), from: None });
    // again, the same name: asked before writing over it
    a.logs_key(Some(n0 + 2), None, KeyCode::Char('s'), KeyModifiers::NONE);
    a.prompt = path.display().to_string();
    a.save_log_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(a.overlay, Overlay::SaveLog);
    assert!(a.note.contains("again writes over it"), "{}", a.note);
    a.save_log_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(a.note.starts_with("saved"), "{}", a.note);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn position_predicted() {
    use super::drawing::{predict, stops_prediction};
    let p = |position, paused, rate| Progress { position, duration: Some(100.0), paused, rate };
    // playing: on at its speed; fast-forward faster; rewinding back
    assert_eq!(predict(p(10.0, false, 1.0), 2.0).position, 12.0);
    assert_eq!(predict(p(10.0, false, 4.0), 2.0).position, 18.0);
    assert_eq!(predict(p(10.0, false, -8.0), 2.0).position, 0.0);
    // kept within the bar; paused stays
    assert_eq!(predict(p(99.0, false, 1.0), 5.0).position, 100.0);
    assert_eq!(predict(p(10.0, true, 1.0), 5.0).position, 10.0);
    // what stops it
    assert!(stops_prediction(&Action::PlayPause) && stops_prediction(&Action::Seek(10.0)) && stops_prediction(&Action::Key(86)));
    assert!(!stops_prediction(&Action::Volume(1)));

    // in the app: playing Kodi, moved on since the read
    let mut st = kodi_paused();
    if let Some(pr) = st.snap.as_mut().and_then(|s| s.activity.as_mut()).and_then(|a| a.progress.as_mut()) {
        pr.paused = false;
    }
    st.at = std::time::Instant::now() - std::time::Duration::from_secs(3);
    let read = 27.0 * 60.0 + 56.0;
    let (mut a, _rx, h) = app_seeing_predict(st);
    let now = a.progress_now().unwrap().position;
    assert!((now - (read + 3.0)).abs() < 0.5, "{now}");
    // pause asked for: held where it is until a read
    a.apply(Input::Do(Action::PlayPause), &h);
    let held = a.progress_now().unwrap().position;
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert_eq!(a.progress_now().unwrap().position, held);
    a.handle(CoreEvent::Changed(Change::Snapshot));
    assert!(a.frozen.is_none());
    // the option off: as read
    a.opts = Some(vec![(tveee_core::options::PREDICT_POSITION.to_string(), false)]);
    assert_eq!(a.progress_now().unwrap().position, read);
}

/// An app on `st` with its own options, and a handle that keeps what it's asked.
fn app_seeing_predict(st: TvState) -> (App, std::sync::mpsc::Receiver<Action>, Handle) {
    super::tests::install_all();
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = Handle::new(tx, Arc::new(Mutex::new(Arc::new(st))));
    let mut a = App::new("192.168.1.19".into(), handle.clone(), Vec::new(), true);
    a.opts = Some(Vec::new());
    (a, rx, handle)
}

/// Kodi showing Music > Artists, the list read, its highlight on Radiohead.
fn kodi_artists() -> TvState {
    use tveee_core::more::Go;
    use tveee_core::provider::{ContextList, ListItem, ListPlace};
    let mut st = kodi_paused();
    let item = |id: &str, label: &str, folder: bool, meta: Option<&str>| ListItem {
        section: None,
        id: id.into(),
        label: label.into(),
        meta: meta.map(str::to_string),
        folder,
        go: Go::Link(if folder { format!("kodi-window:music|{id}") } else { id.to_string() }),
    };
    let list = ContextList {
        title: "Artists".into(),
        kind: "folder",
        key: "10502|musicdb://artists/".into(),
        items: vec![
            item("musicdb://artists/1/", "Massive Attack", true, Some("6 albums")),
            item("musicdb://artists/7/", "Portishead", true, Some("3 albums")),
            item("musicdb://artists/9/", "Radiohead", true, Some("9 albums")),
            item("smb://nas/music/exit.flac", "Exit Music.flac", false, Some("4:24")),
        ],
        playing: None,
        at: Some(2),
        up: None,
        reopen: None,
        crumbs: Vec::new(),
        crumbs_whole: false,
        location: None,
        open_location: None,
        browsed: false,
    };
    st.list = Some(Arc::new(Ok(Some(list))));
    if let Some(s) = st.snap.as_mut() {
        s.list_place = Some(ListPlace {
            key: "10502|musicdb://artists/".into(),
            focus_label: "Radiohead".into(),
            focus_id: "musicdb://artists/9/".into(),
            playing_id: String::new(),
        });
    }
    st
}

/// The state an engine handle reads, to change in a test.
type Held = Arc<Mutex<Arc<TvState>>>;

/// An app whose engine handle keeps what it's asked to do.
fn app_seeing(st: TvState) -> (App, std::sync::mpsc::Receiver<Action>, Handle) {
    let (a, rx, h, _) = app_holding(st);
    (a, rx, h)
}

/// [`app_seeing`], and the state its handle reads.
fn app_holding(st: TvState) -> (App, std::sync::mpsc::Receiver<Action>, Handle, Held) {
    super::tests::install_all();
    let (tx, rx) = std::sync::mpsc::channel();
    let held: Held = Arc::new(Mutex::new(Arc::new(st)));
    let handle = Handle::new(tx, held.clone());
    let mut app = App::new("192.168.1.19".into(), handle.clone(), Vec::new(), true);
    // the options as they come: not the real file's
    app.opts = Some(Vec::new());
    (app, rx, handle, held)
}

#[test]
fn list_pane_desktop() {
    let (mut a, _rx, _h) = app_seeing(kodi_artists());
    a.overlay = Overlay::List { sel: 2 };
    let s = screen(&mut a, DESKTOP);
    insta::assert_snapshot!(s);
    // Kodi's highlight marked; folders end in /; the count
    assert!(s.contains("› Radiohead/") && s.contains("Exit Music.flac") && s.contains("3 of 4"), "{s}");
    // the rows answer clicks
    assert_eq!(a.hits.iter().filter(|(_, h)| matches!(h, Hit::ListRow(_))).count(), 4);
    // Kodi's card offers it
    let (mut b, _rx, _h) = app_seeing(kodi_artists());
    let card = screen(&mut b, DESKTOP);
    assert!(card.lines().any(|l| l.starts_with("│do ") && l.contains(" t ") && l.contains("list")), "{card}");
}

#[test]
fn list_pane_keys() {
    use tveee_core::more::Go;
    let (mut a, rx, h) = app_seeing(kodi_artists());
    // opening asks for the list, and for where it is on each refresh
    a.open_list(&h);
    let asked: Vec<Action> = rx.try_iter().collect();
    assert!(asked.contains(&Action::WatchList(true)) && asked.contains(&Action::LoadList), "{asked:?}");
    assert_eq!(a.overlay, Overlay::List { sel: 2 });
    // the list keys
    a.list_key(2, KeyCode::Home, KeyModifiers::NONE, &h);
    assert_eq!(a.overlay, Overlay::List { sel: 0 });
    a.list_key(0, KeyCode::End, KeyModifiers::NONE, &h);
    assert_eq!(a.overlay, Overlay::List { sel: 3 });
    // typing filters: the highlight goes to what's left
    for c in "port".chars() {
        let Overlay::List { sel } = a.overlay else { panic!() };
        a.list_key(sel, KeyCode::Char(c), KeyModifiers::NONE, &h);
    }
    assert_eq!(a.overlay, Overlay::List { sel: 1 });
    assert_eq!(a.list_shown(), vec![1]);
    // a folder: opened, the pane stays (and the filter goes)
    a.list_key(1, KeyCode::Enter, KeyModifiers::NONE, &h);
    assert_eq!(rx.try_iter().collect::<Vec<_>>(), vec![Action::ListGo(Go::Link("kodi-window:music|musicdb://artists/7/".into()))]);
    assert!(matches!(a.overlay, Overlay::List { .. }) && a.prompt.is_empty());
    // a file: played, the pane closes and the refresh stops reading the list
    a.list_choose(3, &h);
    let asked: Vec<Action> = rx.try_iter().collect();
    assert_eq!(asked, vec![Action::ListGo(Go::Link("smb://nas/music/exit.flac".into())), Action::WatchList(false)]);
    assert_eq!(a.overlay, Overlay::None);
}

#[test]
fn list_pane_follows_kodi() {
    use tveee_core::provider::ListPlace;
    let st = kodi_artists();
    let (mut a, rx, h, held) = app_holding(st.clone());
    a.open_list(&h);
    let _ = rx.try_iter().count();
    a.list_sync(Change::Snapshot);
    // Kodi's highlight moves to Portishead: the pane follows
    let mut moved = st.clone();
    moved.snap.as_mut().unwrap().list_place = Some(ListPlace {
        key: "10502|musicdb://artists/".into(),
        focus_label: "Portishead".into(),
        focus_id: "musicdb://artists/7/".into(),
        playing_id: String::new(),
    });
    *held.lock().unwrap() = Arc::new(moved);
    a.st = h.state();
    a.list_sync(Change::Snapshot);
    assert_eq!(a.overlay, Overlay::List { sel: 1 });
    // nothing new asked for: same list
    assert!(rx.try_iter().next().is_none());
    // Kodi opens another folder: its items are read (once)
    let mut other = st;
    other.snap.as_mut().unwrap().list_place =
        Some(ListPlace { key: "10502|musicdb://artists/9/".into(), focus_label: "OK Computer".into(), focus_id: String::new(), playing_id: String::new() });
    *held.lock().unwrap() = Arc::new(other);
    a.st = h.state();
    a.list_sync(Change::Snapshot);
    assert_eq!(rx.try_iter().collect::<Vec<_>>(), vec![Action::LoadList]);
}

#[test]
fn list_pane_parts() {
    // the list in two parts: a subheading where each starts
    let mut st = kodi_artists();
    if let Some(Ok(Some(mut l))) = st.list.as_deref().cloned() {
        for (n, it) in l.items.iter_mut().enumerate() {
            it.section = Some(if n < 3 { "Artists" } else { "Files" }.into());
        }
        st.list = Some(Arc::new(Ok(Some(l))));
    }
    let (mut a, _rx, h) = app_seeing(st);
    a.overlay = Overlay::List { sel: 3 };
    let s = screen(&mut a, DESKTOP);
    insta::assert_snapshot!(s);
    assert!(s.contains("─ Artists") && s.contains("─ Files"), "{s}");
    // subheadings aren't chosen: still 4 rows to click
    assert_eq!(a.hits.iter().filter(|(_, h)| matches!(h, Hit::ListRow(_))).count(), 4);
    // filtered to the files only: the artists' subheading goes
    for c in "exit".chars() {
        let Overlay::List { sel } = a.overlay else { panic!() };
        a.list_key(sel, KeyCode::Char(c), KeyModifiers::NONE, &h);
    }
    let s = screen(&mut a, DESKTOP);
    assert!(!s.contains("─ Artists") && s.contains("─ Files"), "{s}");
}

/// Kodi playing, with an Ambilight as the TV has it: on, following video
/// (standard), Hue on, three lights a side.
fn with_ambilight() -> TvState {
    use tveee_core::panel::{Choice, Row, Run, Shown, View};
    const ID: &str = "ambilight.change";
    let mut st = kodi_paused();
    let light = |side: &str, n: u32, rgb| Choice {
        label: format!("{side} {n}"),
        colour: Some(rgb),
        run: Run::Ask { prompt: format!("Colour for {side} {n}:"), hint: "red, warm, #ff8800", id: ID, args: vec!["one".into(), side.into(), n.to_string()] },
    };
    let mut lights = vec![Choice {
        label: "all".into(),
        colour: None,
        run: Run::Ask { prompt: "Colour for every light:".into(), hint: "red", id: ID, args: vec!["all".into()] },
    }];
    for n in 0..3 {
        lights.push(light("left", n, (255, 120 - n as u8 * 40, 0)));
    }
    for n in 0..3 {
        lights.push(light("right", n, (0, 80 + n as u8 * 60, 255)));
    }
    let shown = Shown {
        info: vec![
            ("power".into(), "on".into()),
            ("control".into(), "the TV".into()),
            ("style".into(), "follow video, standard".into()),
            ("hue".into(), "on (Ambilight+Hue)".into()),
        ],
        swatches: Vec::new(),
        rows: vec![
            Row {
                name: "power",
                options: vec![Choice::act("on", ID, &["power", "on"]), Choice::act("off", ID, &["power", "off"])],
                current: Some(0),
                help: None,
            },
            Row {
                name: "style",
                options: vec![
                    Choice::act("video", ID, &["style", "FOLLOW_VIDEO"]),
                    Choice::act("sound", ID, &["style", "FOLLOW_AUDIO"]),
                    Choice::act("colour", ID, &["style", "FOLLOW_COLOR"]),
                ],
                current: Some(0),
                help: None,
            },
            Row {
                name: "setting",
                options: ["standard", "natural", "vivid", "game", "comfort", "relax"]
                    .iter()
                    .map(|s| Choice::act(*s, ID, &["style", "FOLLOW_VIDEO", s]))
                    .collect(),
                current: Some(0),
                help: None,
            },
            Row {
                name: "ambi+hue",
                options: vec![Choice::act("on", ID, &["hue", "on"]), Choice::act("off", ID, &["hue", "off"])],
                current: Some(0),
                help: Some("sync Hue bulbs with the TV"),
            },
            Row { name: "lights", options: lights, current: None, help: None },
        ],
    };
    st.panels = vec!["ambilight"];
    if let Some(s) = st.snap.as_mut() {
        s.panels = vec![("ambilight", View::Shown(shown))];
    }
    st
}

#[test]
fn ambilight_panel_edits_in_place() {
    let (mut a, rx, h) = app_seeing(with_ambilight());
    a.ambi.open = true;
    let s = screen(&mut a, DESKTOP);
    insta::assert_snapshot!("ambilight_panel", s);
    // the rows are the choices; "control" (not a row) still said; no colour blocks twice
    assert!(s.contains("power") && s.contains(" video ") && s.contains(" standard ") && s.contains("control"), "{s}");
    assert!(!s.contains("follow video, standard"), "the style isn't said twice: {s}");
    // Ambilight+Hue: its own toggle, said what it does
    assert!(s.contains("ambi+hue") && s.contains("sync Hue bulbs with the TV"), "{s}");
    // L: the cursor in the panel, no pop-up
    a.apply(Input::AmbilightDialog, &h);
    assert_eq!(a.overlay, Overlay::Ambilight { row: 0, col: 0 });
    let s = screen(&mut a, DESKTOP);
    assert!(s.contains("esc done"), "{s}");
    let _ = rx.try_iter().count();
    // ↓ to style, → changes it at once
    a.ambi_key(0, 0, KeyCode::Down, &h);
    let Overlay::Ambilight { row, col } = a.overlay else { panic!() };
    a.ambi_key(row, col, KeyCode::Right, &h);
    assert!(rx.try_iter().any(|x| matches!(&x, Action::Run { id: "ambilight.change", args } if args == &["style", "FOLLOW_AUDIO"])));
    // a light: ⏎ opens the picker
    a.overlay = Overlay::Ambilight { row: 4, col: 2 };
    a.ambi_key(4, 2, KeyCode::Enter, &h);
    assert_eq!(a.overlay, Overlay::AmbiColour { row: 4, col: 2 });
    let s = screen(&mut a, DESKTOP);
    insta::assert_snapshot!("ambilight_picker", s);
    // → twice along the full row (red, orange, yellow), ⏎ sets it as #hex
    a.ambi.pick = (0, 1);
    a.ambi_ask_key(4, 2, KeyCode::Right, KeyModifiers::NONE, &h);
    a.ambi_ask_key(4, 2, KeyCode::Right, KeyModifiers::NONE, &h);
    a.ambi_ask_key(4, 2, KeyCode::Enter, KeyModifiers::NONE, &h);
    let want = super::ambi::hex(super::ambi::pick_colour(2, 1));
    assert!(rx.try_iter().any(|x| matches!(&x, Action::Run { args, .. } if args == &["one", "left", "1", want.as_str()])), "{want}");
    // the picker stays, to try another; Esc back to the panel
    assert_eq!(a.overlay, Overlay::AmbiColour { row: 4, col: 2 });
    a.ambi_ask_key(4, 2, KeyCode::Right, KeyModifiers::NONE, &h);
    a.ambi_ask_key(4, 2, KeyCode::Enter, KeyModifiers::NONE, &h);
    let next = super::ambi::hex(super::ambi::pick_colour(3, 1));
    assert!(rx.try_iter().any(|x| matches!(&x, Action::Run { args, .. } if args == &["one", "left", "1", next.as_str()])), "{next}");
    a.ambi_ask_key(4, 2, KeyCode::Esc, KeyModifiers::NONE, &h);
    assert_eq!(a.overlay, Overlay::Ambilight { row: 4, col: 2 });
    // a click on the panel beside the picker doesn't close it
    a.overlay = Overlay::AmbiColour { row: 4, col: 2 };
    let _ = screen(&mut a, DESKTOP);
    let panel = a.hits.iter().find(|(_, hit)| *hit == Hit::Input(Input::AmbilightDialog)).map(|(r, _)| *r).expect("the panel");
    let spot = (panel.x..panel.right())
        .flat_map(|x| (panel.y..panel.bottom()).map(move |y| (x, y)))
        .find(|&(x, y)| a.hit_at(x, y) == Some(Hit::Input(Input::AmbilightDialog)));
    if let Some((x, y)) = spot {
        a.click(x, y, &h);
        assert_eq!(a.overlay, Overlay::AmbiColour { row: 4, col: 2 });
    }
    // typed instead: what's typed is set
    a.overlay = Overlay::AmbiColour { row: 4, col: 0 };
    for c in "warm".chars() {
        a.ambi_ask_key(4, 0, KeyCode::Char(c), KeyModifiers::NONE, &h);
    }
    a.ambi_ask_key(4, 0, KeyCode::Enter, KeyModifiers::NONE, &h);
    assert!(rx.try_iter().any(|x| matches!(&x, Action::Run { args, .. } if args == &["all", "warm"])));
    // a click on a colour sets it
    a.overlay = Overlay::AmbiColour { row: 4, col: 0 };
    let _ = screen(&mut a, DESKTOP);
    let spot = a.hits.iter().find(|(_, hit)| *hit == Hit::AmbiPick(0, super::ambi::SPECIALS)).map(|(r, _)| *r).expect("white in the picker");
    a.click(spot.x, spot.y, &h);
    assert!(rx.try_iter().any(|x| matches!(&x, Action::Run { args, .. } if args == &["all", "#ffffff"])));
}

#[test]
fn ambilight_picker_switches_lights() {
    let (mut a, rx, h) = app_seeing(with_ambilight());
    a.ambi.open = true;
    // picking a colour for every light
    a.overlay = Overlay::Ambilight { row: 4, col: 0 };
    a.ambi_key(4, 0, KeyCode::Enter, &h);
    assert_eq!(a.overlay, Overlay::AmbiColour { row: 4, col: 0 });
    for c in "warm".chars() {
        a.ambi_ask_key(4, 0, KeyCode::Char(c), KeyModifiers::NONE, &h);
    }
    let s = screen(&mut a, DESKTOP);
    insta::assert_snapshot!(s);
    assert!(s.contains("▕all▏") && s.contains("tab: the next"), "{s}");
    // click another light (in the panel): the picker is for that one now
    let spot = |a: &App, hit: Hit| a.hits.iter().rev().find(|(_, x)| *x == hit).map(|(r, _)| *r).expect("on screen");
    let r = spot(&a, Hit::AmbiOption(4, 3));
    a.click(r.x, r.y, &h);
    assert_eq!(a.overlay, Overlay::AmbiColour { row: 4, col: 3 });
    // tab: the next light; what's typed is kept
    a.ambi_ask_key(4, 3, KeyCode::Tab, KeyModifiers::NONE, &h);
    assert_eq!((a.overlay, a.prompt.as_str()), (Overlay::AmbiColour { row: 4, col: 4 }, "warm"));
    // "all" in the picker's own row of lights
    let _ = screen(&mut a, DESKTOP);
    let r = spot(&a, Hit::AmbiOption(4, 0));
    a.click(r.x, r.y, &h);
    assert_eq!(a.overlay, Overlay::AmbiColour { row: 4, col: 0 });
    assert!(!rx.try_iter().any(|x| matches!(x, Action::Run { .. })), "switching sets nothing");
    // a choice that isn't a light (power off): the picker goes, it's changed
    let _ = screen(&mut a, DESKTOP);
    let r = spot(&a, Hit::AmbiOption(0, 1));
    a.click(r.x, r.y, &h);
    assert_eq!(a.overlay, Overlay::Ambilight { row: 0, col: 1 });
    assert!(rx.try_iter().any(|x| matches!(&x, Action::Run { args, .. } if args == &["power", "off"])));
}

#[test]
fn picker_shows_what_is_typed() {
    let (mut a, _rx, h) = app_seeing(with_ambilight());
    a.ambi.open = true;
    a.overlay = Overlay::AmbiColour { row: 4, col: 1 };
    let typed = |a: &mut App, text: &str| {
        for c in text.chars() {
            let Overlay::AmbiColour { row, col } = a.overlay else { panic!() };
            a.ambi_ask_key(row, col, KeyCode::Char(c), KeyModifiers::NONE, &h);
        }
        screen(a, DESKTOP)
    };
    // part of a name: not one yet
    let s = typed(&mut a, "ora");
    assert!(s.contains("ora: not a colour yet"), "{s}");
    // the whole name: its colour (and its #hex)
    let s = typed(&mut a, "nge");
    assert!(s.contains("#ff6400") && !s.contains("not a colour"), "{s}");
    // #hex and r,g,b too
    a.prompt.clear();
    assert!(typed(&mut a, "#00ff88").contains("#00ff88"));
    a.prompt.clear();
    assert!(typed(&mut a, "10,20,30").contains("#0a141e"));
}

#[test]
fn links_under_a_pop_up_stay_under_it() {
    let mut a = app(kodi_catchup());
    // the link drawn, showing
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(DESKTOP.0, DESKTOP.1)).unwrap();
    let frame = term.draw(|f| a.draw(f)).unwrap();
    assert_eq!(super::links::showing(&a.links, frame.buffer).len(), a.links.len());
    assert!(!a.links.is_empty());
    // help drawn over it: not written again on top
    a.overlay = Overlay::Help(0);
    let frame = term.draw(|f| a.draw(f)).unwrap();
    assert!(super::links::showing(&a.links, frame.buffer).is_empty(), "the link would show through help");
}

#[test]
fn list_trail_and_going_up() {
    use super::ctxlist::{Crumb, next_trail};
    use tveee_core::more::Go;
    use tveee_core::provider::ContextList;
    let list = |title: &str, key: &str, up: Option<(&str, &str)>| ContextList {
        title: title.into(),
        kind: "folder",
        key: key.into(),
        items: Vec::new(),
        playing: None,
        at: None,
        up: up.map(|(k, g)| (k.to_string(), Go::Link(g.into()))),
        reopen: Some(Go::Link(format!("open:{key}"))),
        crumbs: Vec::new(),
        crumbs_whole: false,
        location: None,
        open_location: None,
        browsed: false,
    };
    let music = list("Music", "10502|", None);
    let albums = list("Albums", "10502|musicdb://albums/", Some(("10502|", "kodi-window:music")));
    let album = list("A Love Supreme", "10502|musicdb://albums/7/", Some(("10502|musicdb://albums/", "kodi-window:music|musicdb://albums/")));
    // down: each one's way up teaches how to get back to the one before
    let t = next_trail(&[], &music);
    let t = next_trail(&t, &albums);
    let t = next_trail(&t, &album);
    assert_eq!(t.iter().map(|c| c.title.as_str()).collect::<Vec<_>>(), ["Music", "Albums", "A Love Supreme"]);
    // each crumb goes back to its own list
    assert_eq!(t[0].go, Some(Go::Link("open:10502|".into())));
    assert_eq!(t[1].go, Some(Go::Link("open:10502|musicdb://albums/".into())));
    // back to one on the trail: it's cut there
    let back = next_trail(&t, &albums);
    assert_eq!(back.len(), 2);
    // a list not on it (opened in Kodi straight away): "‹ up", then it
    let fresh = next_trail(&[], &album);
    assert_eq!(
        fresh[0],
        Crumb { title: "‹ up".into(), key: "10502|musicdb://albums/".into(), go: Some(Go::Link("kodi-window:music|musicdb://albums/".into())) }
    );

    // in the pane: the crumbs drawn, ⌫ goes up, a crumb click goes back to it
    let mut st = kodi_artists();
    if let Some(Ok(Some(mut l))) = st.list.as_deref().cloned() {
        l.up = Some(("10502|".into(), Go::Link("kodi-window:music".into())));
        st.list = Some(Arc::new(Ok(Some(l))));
    }
    let (mut a, rx, h) = app_seeing(st);
    a.list.crumbs = vec![
        Crumb { title: "Music".into(), key: "10502|".into(), go: Some(Go::Link("kodi-window:music".into())) },
        Crumb { title: "Artists".into(), key: "10502|musicdb://artists/".into(), go: None },
    ];
    a.overlay = Overlay::List { sel: 0 };
    let s = screen(&mut a, DESKTOP);
    insta::assert_snapshot!(s);
    assert!(s.contains("Music › Artists"), "{s}");
    a.list_key(0, KeyCode::Backspace, KeyModifiers::NONE, &h);
    assert!(rx.try_iter().any(|x| x == Action::ListGo(Go::Link("kodi-window:music".into()))));
    let r = a.hits.iter().find(|(_, x)| *x == Hit::Crumb(0)).map(|(r, _)| *r).expect("Music to click");
    a.click(r.x, r.y, &h);
    assert!(rx.try_iter().any(|x| x == Action::ListGo(Go::Link("kodi-window:music".into()))));
    // typing: ⌫ takes a letter off, not up
    a.prompt = "ra".into();
    a.list_key(0, KeyCode::Backspace, KeyModifiers::NONE, &h);
    assert_eq!(a.prompt, "r");
    assert!(!rx.try_iter().any(|x| matches!(x, Action::ListGo(_))));
}

#[test]
fn playing_marked_apart_from_the_highlight() {
    use tveee_core::provider::ListPlace;
    // Kodi's highlight on Radiohead (›), the file playing is another item (▶)
    let mut st = kodi_artists();
    if let Some(s) = st.snap.as_mut() {
        s.list_place = Some(ListPlace {
            key: "10502|musicdb://artists/".into(),
            focus_label: "Radiohead".into(),
            focus_id: "musicdb://artists/9/".into(),
            playing_id: "smb://nas/music/exit.flac".into(),
        });
    }
    let (mut a, _rx, _h) = app_seeing(st.clone());
    a.overlay = Overlay::List { sel: 0 };
    let s = screen(&mut a, DESKTOP);
    insta::assert_snapshot!(s);
    assert!(s.contains("› Radiohead/") && s.contains("▶ Exit Music.flac"), "{s}");

    // no highlight of its own (YouTube): the pane follows what's playing
    let (mut b, _rx, hb, held) = app_holding(st.clone());
    let playing = |id: &str| {
        let mut moved = st.clone();
        moved.snap.as_mut().unwrap().list_place =
            Some(ListPlace { key: "10502|musicdb://artists/".into(), focus_label: String::new(), focus_id: String::new(), playing_id: id.into() });
        *held.lock().unwrap() = Arc::new(moved);
    };
    b.overlay = Overlay::List { sel: 0 };
    playing("musicdb://artists/7/");
    b.st = hb.state();
    b.list_sync(Change::Snapshot);
    assert_eq!(b.overlay, Overlay::List { sel: 1 });
    playing("smb://nas/music/exit.flac");
    b.st = hb.state();
    b.list_sync(Change::Snapshot);
    assert_eq!(b.overlay, Overlay::List { sel: 3 });
}

#[test]
fn breadcrumbs_from_the_path_and_a_typed_location() {
    use super::ctxlist::{Crumb, breadcrumbs};
    use tveee_core::more::Go;
    use tveee_core::provider::ContextList;
    let crumb = |t: &str, k: &str| Crumb { title: t.into(), key: k.into(), go: Some(Go::Link(format!("open:{k}"))) };
    let list = |title: &str, key: &str, crumbs: Vec<Crumb>, whole: bool| ContextList {
        title: title.into(),
        kind: "folder",
        key: key.into(),
        items: Vec::new(),
        playing: None,
        at: None,
        up: None,
        reopen: Some(Go::Link(format!("open:{key}"))),
        crumbs,
        crumbs_whole: whole,
        location: Some(key.trim_start_matches("10502|").into()),
        open_location: Some("kodi-window:music|{}".into()),
        browsed: false,
    };
    let titles = |c: &[Crumb]| c.iter().map(|x| x.title.clone()).collect::<Vec<_>>();
    // a share: every step from the path, straight away
    let album = list(
        "Selected Ambient Works",
        "10502|smb://nas/music/Aphex Twin/Selected Ambient Works/",
        vec![
            crumb("Music", "10502|"),
            crumb("nas", "10502|smb://nas/"),
            crumb("music", "10502|smb://nas/music/"),
            crumb("Aphex Twin", "10502|smb://nas/music/Aphex Twin/"),
        ],
        true,
    );
    assert_eq!(titles(&breadcrumbs(&[], &album)), ["Music", "nas", "music", "Aphex Twin", "Selected Ambient Works"]);
    // the library, opened straight into an album: its section, then …
    let named = vec![crumb("Music", "10502|"), crumb("Albums", "10502|musicdb://albums/")];
    let mut lib = list("A Polished Solid", "10502|musicdb://albums/251/", named.clone(), false);
    // as Kodi's are: each in the one before (its ".." row)
    lib.up = Some(("10502|musicdb://albums/".into(), Go::Link("kodi-parentdir".into())));
    let straight = list("A Polished Solid", "10502|musicdb://albums/251/", vec![crumb("Music", "10502|"), crumb("Albums", "10502|musicdb://albums/")], false);
    assert_eq!(titles(&breadcrumbs(&[], &straight)), ["Music", "Albums", "…", "A Polished Solid"]);
    // ...and gone through here: the trail fills the gap
    let albums = list("Albums", "10502|musicdb://albums/", vec![crumb("Music", "10502|")], true);
    let mut disc = list("Disc 1", "10502|musicdb://albums/251/1/", named, false);
    disc.up = Some(("10502|musicdb://albums/251/".into(), Go::Link("kodi-parentdir".into())));
    let t = breadcrumbs(&[], &albums);
    let t = breadcrumbs(&t, &lib);
    let t = breadcrumbs(&t, &disc);
    assert_eq!(titles(&t), ["Music", "Albums", "A Polished Solid", "Disc 1"]);

    // ^L in the pane: the location to edit; ⏎ opens what's typed in Kodi
    let mut st = kodi_artists();
    if let Some(Ok(Some(mut l))) = st.list.as_deref().cloned() {
        l.location = Some("musicdb://artists/".into());
        l.open_location = Some("kodi-window:music|{}".into());
        st.list = Some(Arc::new(Ok(Some(l))));
    }
    let (mut a, rx, h) = app_seeing(st);
    a.overlay = Overlay::List { sel: 0 };
    a.list_key(0, KeyCode::Char('l'), KeyModifiers::CONTROL, &h);
    assert_eq!(a.list.location.as_deref(), Some("musicdb://artists/"));
    let s = screen(&mut a, DESKTOP);
    insta::assert_snapshot!(s);
    assert!(s.contains("location musicdb://artists/"), "{s}");
    a.list_key(0, KeyCode::Char('u'), KeyModifiers::CONTROL, &h);
    for c in "smb://nas/music/".chars() {
        a.list_key(0, KeyCode::Char(c), KeyModifiers::NONE, &h);
    }
    // typing went to the location, not the filter
    assert!(a.prompt.is_empty());
    a.list_key(0, KeyCode::Enter, KeyModifiers::NONE, &h);
    assert!(rx.try_iter().any(|x| x == Action::ListGo(Go::Link("kodi-window:music|smb://nas/music/".into()))));
    assert!(a.list.location.is_none());
    // Esc: back to the list, nothing opened
    a.list_key(0, KeyCode::Char('l'), KeyModifiers::CONTROL, &h);
    a.list_key(0, KeyCode::Esc, KeyModifiers::NONE, &h);
    assert!(a.list.location.is_none() && matches!(a.overlay, Overlay::List { .. }));
}

#[test]
fn tab_to_the_breadcrumbs() {
    use super::ctxlist::Crumb;
    use tveee_core::more::Go;
    let (mut a, rx, h) = app_seeing(kodi_artists());
    let crumb = |t: &str, k: &str| Crumb { title: t.into(), key: k.into(), go: Some(Go::Link(format!("open:{k}"))) };
    a.list.crumbs = vec![crumb("Music", "10502|"), crumb("Artists", "10502|musicdb://artists/")];
    a.overlay = Overlay::List { sel: 0 };
    // Tab: onto the breadcrumbs, on the list shown
    a.list_key(0, KeyCode::Tab, KeyModifiers::NONE, &h);
    assert_eq!(a.list.crumb_at, Some(1));
    let s = screen(&mut a, DESKTOP);
    insta::assert_snapshot!(s);
    assert!(s.contains("tab back to the list"), "{s}");
    // ← to Music, ⏎ goes there; the keys are back on the list
    a.list_key(0, KeyCode::Left, KeyModifiers::NONE, &h);
    assert_eq!(a.list.crumb_at, Some(0));
    a.list_key(0, KeyCode::Enter, KeyModifiers::NONE, &h);
    assert!(rx.try_iter().any(|x| x == Action::ListGo(Go::Link("open:10502|".into()))));
    assert_eq!(a.list.crumb_at, None);
    // ⏎ on the list shown goes nowhere; Esc (or Tab, ↓) back to the list
    a.list_key(0, KeyCode::Tab, KeyModifiers::NONE, &h);
    a.list_key(0, KeyCode::Enter, KeyModifiers::NONE, &h);
    assert!(!rx.try_iter().any(|x| matches!(x, Action::ListGo(_))));
    a.list_key(0, KeyCode::Tab, KeyModifiers::NONE, &h);
    a.list_key(0, KeyCode::Esc, KeyModifiers::NONE, &h);
    assert_eq!(a.list.crumb_at, None);
    assert!(matches!(a.overlay, Overlay::List { .. }), "Esc on the breadcrumbs doesn't close the pane");
    // with the keys on the list, ↓ moves in it again
    a.list_key(0, KeyCode::Down, KeyModifiers::NONE, &h);
    assert_eq!(a.overlay, Overlay::List { sel: 1 });
}

#[test]
fn alt_up_goes_up() {
    use tveee_core::more::Go;
    let mut st = kodi_artists();
    if let Some(Ok(Some(mut l))) = st.list.as_deref().cloned() {
        l.up = Some(("10502|".into(), Go::Link("kodi-parentdir".into())));
        st.list = Some(Arc::new(Ok(Some(l))));
    }
    let (mut a, rx, h) = app_seeing(st);
    a.overlay = Overlay::List { sel: 2 };
    // plain ↑ moves in the list; alt+↑ goes up a level (even with a filter typed)
    a.prompt = "ra".into();
    a.list_key(2, KeyCode::Up, KeyModifiers::ALT, &h);
    assert!(rx.try_iter().any(|x| x == Action::ListGo(Go::Link("kodi-parentdir".into()))));
    a.list_key(2, KeyCode::Up, KeyModifiers::NONE, &h);
    assert!(!rx.try_iter().any(|x| matches!(x, Action::ListGo(_))));
}

/// Kodi's Albums, browsed here (not following its screen).
fn kodi_albums_browsed() -> TvState {
    use tveee_core::more::Go;
    use tveee_core::provider::{ContextList, ListCrumb, ListItem, ListPlace};
    let mut st = kodi_paused();
    let album = |id: u32, name: &str, artist: &str| ListItem {
        section: None,
        id: format!("musicdb://albums/{id}/"),
        label: name.into(),
        meta: Some(artist.into()),
        folder: true,
        go: Go::Link(format!("music|{name}|musicdb://albums/{id}/")),
    };
    let list = ContextList {
        title: "Albums".into(),
        kind: "folder",
        key: "10502|musicdb://albums/".into(),
        items: vec![album(250, "Dummy", "Portishead"), album(251, "A Polished Solid", "Luke Vibert"), album(252, "Mezzanine", "Massive Attack")],
        playing: None,
        at: None,
        up: Some(("10502|".into(), Go::Link("music||".into()))),
        reopen: Some(Go::Link("music|Albums|musicdb://albums/".into())),
        crumbs: vec![ListCrumb { title: "Music".into(), key: "10502|".into(), go: Some(Go::Link("music|Music|".into())) }],
        crumbs_whole: true,
        location: Some("musicdb://albums/".into()),
        open_location: Some("music||{}".into()),
        browsed: true,
    };
    st.list = Some(Arc::new(Ok(Some(list))));
    if let Some(s) = st.snap.as_mut() {
        // Kodi's screen is somewhere else entirely
        s.list_place = Some(ListPlace {
            key: "10025|videodb://movies/".into(),
            focus_label: "Alien".into(),
            focus_id: "videodb://movies/1/".into(),
            playing_id: String::new(),
        });
    }
    st
}

#[test]
fn browsing_kodi_here() {
    let st = kodi_albums_browsed();
    let (mut a, rx, h, held) = app_holding(st.clone());
    a.opts = Some(Vec::new());
    a.open_list(&h);
    a.list_sync(Change::Snapshot);
    let _ = rx.try_iter().count();
    // Kodi's screen shows another list: the pane doesn't follow it or read it
    a.list_key(0, KeyCode::Down, KeyModifiers::NONE, &h);
    a.list_sync(Change::Snapshot);
    assert_eq!(a.overlay, Overlay::List { sel: 1 });
    assert!(!rx.try_iter().any(|x| x == Action::LoadList));
    let s = screen(&mut a, DESKTOP);
    insta::assert_snapshot!(s);
    assert!(s.contains("^S sync folder: off"), "{s}");
    // a folder opens here, not on the TV
    a.list_choose(1, &h);
    assert_eq!(rx.try_iter().collect::<Vec<_>>(), vec![Action::ListOpen { at: "music|A Polished Solid|musicdb://albums/251/".into(), show: false }]);
    // ^S: sync folder on, folders open on the TV too
    a.list_key(1, KeyCode::Char('s'), KeyModifiers::CONTROL, &h);
    assert!(a.opt(tveee_core::options::LIST_SYNC));
    // said in the title too
    let s = screen(&mut a, DESKTOP);
    assert!(s.contains("Albums (Kodi) ⇄ TV"), "{s}");
    a.list_up(&h);
    assert_eq!(rx.try_iter().filter(|x| matches!(x, Action::ListOpen { .. })).collect::<Vec<_>>(), vec![Action::ListOpen { at: "music||".into(), show: true }]);
    // the album, read: then back up, landing on it
    let mut inside = st.clone();
    if let Some(Ok(Some(l))) = inside.list.as_deref().cloned() {
        let mut l = l;
        l.title = "A Polished Solid".into();
        l.key = "10502|musicdb://albums/251/".into();
        l.location = Some("musicdb://albums/251/".into());
        l.items.truncate(1);
        inside.list = Some(Arc::new(Ok(Some(l))));
    }
    *held.lock().unwrap() = Arc::new(inside);
    a.st = h.state();
    a.list_sync(Change::List);
    a.list_up(&h);
    *held.lock().unwrap() = Arc::new(st.clone());
    a.st = h.state();
    a.list_sync(Change::List);
    assert_eq!(a.overlay, Overlay::List { sel: 1 }, "back on A Polished Solid");
    // a file plays (ListGo), as before
    let _ = rx.try_iter().count();
}

#[test]
fn a_failed_read_keeps_the_list() {
    use tveee_core::engine::keeps_list;
    let st = kodi_albums_browsed();
    let old = st.list.as_deref();
    assert!(keeps_list(old, &Err("timed out".into())));
    // browsed here: Kodi saying there's no list (a blip) keeps it too
    assert!(keeps_list(old, &Ok(None)));
    // one following the screen: no list there is news
    let shown = kodi_artists();
    assert!(!keeps_list(shown.list.as_deref(), &Ok(None)));
    assert!(keeps_list(shown.list.as_deref(), &Err("timed out".into())));
    // nothing to keep
    assert!(!keeps_list(None, &Err("timed out".into())));
    // kept, the pane says why
    let mut st = st;
    st.list_failed = Some("timed out".into());
    let (mut a, _rx, h) = app_seeing(st);
    a.opts = Some(Vec::new());
    a.open_list(&h);
    let s = screen(&mut a, DESKTOP);
    assert!(s.contains("couldn't read it again (timed out): showing it as it was"), "{s}");
    assert!(s.contains("A Polished Solid"), "{s}");
}

#[test]
fn long_lists_have_a_scrollbar() {
    use tveee_core::more::Go;
    use tveee_core::provider::ListItem;
    let mut st = kodi_albums_browsed();
    if let Some(Ok(Some(mut l))) = st.list.as_deref().cloned() {
        l.items = (0..60)
            .map(|n| ListItem {
                section: None,
                id: format!("musicdb://albums/{n}/"),
                label: format!("Album {n}"),
                meta: None,
                folder: true,
                go: Go::Link(format!("music||musicdb://albums/{n}/")),
            })
            .collect();
        st.list = Some(Arc::new(Ok(Some(l))));
    }
    let (mut a, _rx, h) = app_seeing(st);
    a.opts = Some(Vec::new());
    a.open_list(&h);
    let s = screen(&mut a, CORNER);
    assert!(s.contains('┃'), "a thumb beside the rows: {s}");
    // at the top: the thumb starts at the first row
    let first_row = s.lines().position(|l| l.contains("Album 0/")).expect("the first album");
    assert!(s.lines().nth(first_row).is_some_and(|l| l.contains('┃')), "{s}");
    // a short list: none
    let (mut b, _rx, h) = app_seeing(kodi_albums_browsed());
    b.opts = Some(Vec::new());
    b.open_list(&h);
    let s = screen(&mut b, DESKTOP);
    assert!(s.lines().filter(|l| l.contains("Dummy/") || l.contains("Mezzanine/")).all(|l| !l.contains('┃')), "{s}");
}

#[test]
fn options_scroll_when_short() {
    let (mut a, _rx, _h) = app_seeing(kodi_paused());
    a.opts = Some(Vec::new());
    let last = tveee_core::options::OPTIONS.len() - 1;
    let label = tveee_core::options::OPTIONS[last].label;
    a.overlay = Overlay::Options(last);
    let s = screen(&mut a, (100, 11));
    // the chosen one in view, the tabs and keys still there, a bar beside the rows
    assert!(s.contains(label) && s.contains("Options") && s.contains("tab: plugins"), "{s}");
    assert!(!s.contains(tveee_core::options::OPTIONS[0].label), "the first scrolled out: {s}");
    assert!(s.contains('┃'), "{s}");
    // clicked where it's drawn: that one
    let y = u16::try_from(s.lines().position(|l| l.contains(label)).unwrap()).unwrap();
    assert!(a.hits.iter().any(|(r, h)| *h == Hit::OptionItem(last) && r.y == y), "{:?}", a.hits);
}

fn mouse_at(kind: ratatui::crossterm::event::MouseEventKind, x: u16, y: u16) -> ratatui::crossterm::event::MouseEvent {
    ratatui::crossterm::event::MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE }
}

fn the_bar(a: &App) -> super::drawing::BarAt {
    a.hits.iter().rev().find_map(|(_, h)| if let Hit::Scrollbar(b) = h { Some(*b) } else { None }).expect("a scrollbar")
}

#[test]
fn scrollbars_drag_with_the_mouse() {
    use ratatui::crossterm::event::{MouseButton, MouseEventKind::*};
    let (mut a, rx, h) = app_seeing(kodi_paused());
    a.opts = Some(Vec::new());
    a.overlay = Overlay::Help(0);
    let _ = screen(&mut a, CORNER);
    let b = the_bar(&a);
    let (start, len) = b.thumb();
    assert_eq!(start, 0);
    // grab the thumb, drag it to the bottom (wandering off the bar on the way)
    assert!(!a.mouse(mouse_at(Down(MouseButton::Left), b.area.x, b.area.y), &h));
    a.mouse(mouse_at(Drag(MouseButton::Left), 2, b.area.y + 3), &h);
    a.mouse(mouse_at(Drag(MouseButton::Left), 0, b.area.bottom() + 5), &h);
    assert_eq!(a.overlay, Overlay::Help(b.max_top()), "at the end");
    a.mouse(mouse_at(Up(MouseButton::Left), 0, b.area.bottom() + 5), &h);
    // let go: dragging no longer scrolls
    a.mouse(mouse_at(Drag(MouseButton::Left), b.area.x, b.area.y), &h);
    assert_eq!(a.overlay, Overlay::Help(b.max_top()));
    // a click on the track (above the thumb, now at the bottom): the thumb jumps there
    let _ = screen(&mut a, CORNER);
    let b = the_bar(&a);
    a.mouse(mouse_at(Down(MouseButton::Left), b.area.x, b.area.y), &h);
    a.mouse(mouse_at(Up(MouseButton::Left), b.area.x, b.area.y), &h);
    assert_eq!(a.overlay, Overlay::Help(0), "{len}");

    // the list pane: dragged, the highlight moves so the rows dragged to show
    let mut st = kodi_albums_browsed();
    if let Some(Ok(Some(mut l))) = st.list.as_deref().cloned() {
        use tveee_core::more::Go;
        use tveee_core::provider::ListItem;
        l.items = (0..60)
            .map(|n| ListItem {
                section: None,
                id: format!("musicdb://albums/{n}/"),
                label: format!("Album {n}"),
                meta: None,
                folder: true,
                go: Go::Link(format!("music||musicdb://albums/{n}/")),
            })
            .collect();
        st.list = Some(Arc::new(Ok(Some(l))));
    }
    let (mut a, rx2, h) = app_seeing(st);
    a.opts = Some(Vec::new());
    a.open_list(&h);
    let _ = screen(&mut a, CORNER);
    let b = the_bar(&a);
    a.mouse(mouse_at(Down(MouseButton::Left), b.area.x, b.area.y), &h);
    a.mouse(mouse_at(Drag(MouseButton::Left), b.area.x, b.area.bottom()), &h);
    // let go over a row: nothing chosen
    a.mouse(mouse_at(Up(MouseButton::Left), b.area.x - 10, b.area.y + 1), &h);
    assert!(matches!(a.overlay, Overlay::List { sel } if sel > 50), "{:?}", a.overlay);
    let s = screen(&mut a, CORNER);
    assert!(s.contains("Album 59/"), "the end in view: {s}");
    assert!(!rx2.try_iter().any(|x| matches!(x, Action::ListOpen { .. } | Action::ListGo(_))));
    let _ = rx.try_iter().count();
}

#[test]
fn update_frequency_is_a_choice() {
    let (mut a, _rx, _h) = app_seeing(kodi_paused());
    a.opts = Some(Vec::new());
    let row = tveee_core::options::OPTIONS.iter().position(|o| o.name == tveee_core::options::LOW_BANDWIDTH).expect("the option");
    a.overlay = Overlay::Options(row);
    let s = screen(&mut a, DESKTOP);
    assert!(s.contains("update frequency") && s.contains("‹responsive›") && s.contains(" low bandwidth "), "{s}");
    a.toggle_option(row);
    assert_eq!(a.note, "update frequency: low bandwidth");
    let s = screen(&mut a, DESKTOP);
    assert!(s.contains("‹low bandwidth›"), "{s}");
}

#[test]
fn help_has_colour() {
    use super::overlays::help_line;
    use ratatui::style::Color;
    let parts = |l: &str| help_line(l).spans.iter().map(|s| (s.content.to_string(), s.style.fg)).collect::<Vec<_>>();
    // a heading
    assert_eq!(parts(" player mode"), vec![(" player mode".to_string(), Some(Color::Cyan))]);
    // both columns' keys
    let p = parts("   space p  play/pause       ⌫      back");
    assert!(p.contains(&("space p".to_string(), Some(Color::Yellow))), "{p:?}");
    assert!(p.contains(&("⌫".to_string(), Some(Color::Yellow))), "{p:?}");
    assert!(p.iter().any(|(t, c)| t.contains("play/pause") && c.is_none()), "{p:?}");
    // an aside, dim; a key after it still a key
    let p = parts("   F        free memory (force-stop idle apps)   S  sleep   R  restart the app");
    assert!(p.contains(&("(force-stop idle apps)".to_string(), Some(Color::DarkGray))), "{p:?}");
    assert!(p.contains(&("S".to_string(), Some(Color::Yellow))) && p.contains(&("R".to_string(), Some(Color::Yellow))), "{p:?}");
    // not keys: a sentence, and a continuation line
    assert!(parts("   click the buttons, the progress bar (jump) or volume bar (set)").iter().all(|(_, c)| *c != Some(Color::Yellow)));
    assert!(parts("            in front, coming back where it was (YouTube, Kodi)").iter().all(|(_, c)| *c != Some(Color::Yellow)));
    // the text itself unchanged
    let line = "   home     the TV's home screen    pgup pgdn  page up/down";
    assert_eq!(help_line(line).spans.iter().map(|s| s.content.as_ref()).collect::<String>(), line);
    assert!(parts(line).contains(&("pgup pgdn".to_string(), Some(Color::Yellow))));
    // an aside that wraps: dim on the next line too, up to its bracket
    let both =
        super::overlays::help_styled(&["   u        the link (click it: the same;".to_string(), "            its buttons copy it) and more".to_string()]);
    let second: Vec<_> = both[1].spans.iter().map(|s| (s.content.to_string(), s.style.fg)).collect();
    assert!(second.iter().any(|(t, c)| t.contains("its buttons copy it)") && *c == Some(Color::DarkGray)), "{second:?}");
    assert!(second.iter().any(|(t, c)| t.contains("and more") && c.is_none()), "{second:?}");
}

#[test]
fn options_left_right_set_the_row() {
    use tveee_core::options::{LOW_BANDWIDTH, OPTIONS};
    let (mut a, _rx, _h) = app_seeing(kodi_paused());
    a.opts = Some(Vec::new());
    let row = OPTIONS.iter().position(|o| o.name == LOW_BANDWIDTH).expect("update frequency");
    a.overlay = Overlay::Options(row);
    // → the second choice (low bandwidth), ← back; the tab doesn't change
    a.options_key(row, KeyCode::Right);
    assert!(a.opt(LOW_BANDWIDTH) && !a.options.plugins);
    a.options_key(row, KeyCode::Right);
    assert!(a.opt(LOW_BANDWIDTH), "→ again: still the second");
    a.options_key(row, KeyCode::Left);
    assert!(!a.opt(LOW_BANDWIDTH) && !a.options.plugins);
    assert_eq!(a.overlay, Overlay::Options(row), "still open, on it");
    // ↑ from the first row: the tab bar, where ←/→ switch tabs
    a.overlay = Overlay::Options(0);
    a.options_key(0, KeyCode::Up);
    assert!(a.options.on_tabs);
    let s = screen(&mut a, DESKTOP);
    assert!(s.contains("←→ switch"), "{s}");
    a.options_key(0, KeyCode::Right);
    assert!(a.options.plugins && a.options.on_tabs);
    // ↓ back to the rows; Tab still switches from there
    a.options_key(0, KeyCode::Down);
    assert!(!a.options.on_tabs);
    a.options_key(0, KeyCode::Tab);
    assert!(!a.options.plugins);
    // Esc on the tab bar leaves it; Esc again closes
    a.options_key(0, KeyCode::Up);
    a.options_key(0, KeyCode::Esc);
    assert!(!a.options.on_tabs && matches!(a.overlay, Overlay::Options(_)));
    a.options_key(0, KeyCode::Esc);
    assert_eq!(a.overlay, Overlay::None);
}

#[test]
fn a_plugin_turned_off_goes_at_once() {
    use tveee_core::plugin::{PluginState, Source};
    let (mut a, _rx, _h) = app_seeing(kodi_paused());
    a.opts = Some(Vec::new());
    a.plugins = vec![PluginState { name: "kodi", desc: "Kodi", enabled: true, source: Source::Cache }];
    a.options.plugins = true;
    a.overlay = Overlay::Options(0);
    // ← turns it off: now, not at the next start
    a.options_key(0, KeyCode::Left);
    assert!(!a.plugins[0].enabled && a.plugins[0].source == Source::Override);
    assert_eq!(a.note, "plugin kodi: off");
    assert_eq!(a.plugin_opt("kodi"), Some(false));
    let s = screen(&mut a, DESKTOP);
    assert!(!s.contains("(from the next start)"), "{s}");
    a.options_key(0, KeyCode::Right);
    assert!(a.plugins[0].enabled);
}

#[test]
fn remote_mode_findable_when_narrow() {
    let (mut a, _rx, h) = app_seeing(kodi_paused());
    a.opts = Some(Vec::new());
    let s = screen(&mut a, CORNER);
    assert!(s.contains("tab  remote") || s.contains("tab remote"), "{s}");
    // the tag top right switches mode when clicked
    let first = s.lines().next().unwrap_or_default();
    let x = first.chars().count() - " player ".len() + 1;
    assert!(!a.click(u16::try_from(x).unwrap(), 0, &h));
    assert_eq!(a.mode, Mode::Remote);
}

#[test]
fn system_panel_pauses() {
    let st = kodi_paused();
    let (mut a, rx, h, held) = app_holding(st.clone());
    a.opts = Some(Vec::new());
    a.info = Some(true);
    let s = screen(&mut a, DESKTOP);
    assert!(s.contains("I pauses"), "{s}");
    assert!(rx.try_iter().any(|x| x == Action::WatchSystem(true)));
    // I: paused, the TV no longer asked for the figures, the ones shown kept
    a.apply(Input::PauseInfo, &h);
    let mut later = st.clone();
    if let Some(s) = later.snap.as_mut() {
        s.system.load = Some((9.99, 9.99, 9.99));
    }
    *held.lock().unwrap() = Arc::new(later);
    a.st = h.state();
    let s = screen(&mut a, DESKTOP);
    assert!(s.contains("paused, I resumes") && !s.contains("9.99 9.99"), "the panel kept: {s}");
    assert!(rx.try_iter().any(|x| x == Action::WatchSystem(false)));
    // again: updating, the new figures shown
    a.apply(Input::PauseInfo, &h);
    let s = screen(&mut a, DESKTOP);
    assert!(s.contains("9.99 9.99") && s.contains("I pauses"), "{s}");
    assert!(rx.try_iter().any(|x| x == Action::WatchSystem(true)));
    // closed: not paused when opened again
    a.apply(Input::PauseInfo, &h);
    a.apply(Input::ToggleInfo, &h);
    a.apply(Input::ToggleInfo, &h);
    assert!(screen(&mut a, DESKTOP).contains("I pauses"));
}

#[test]
fn esc_clears_the_filter_first() {
    // the list pane
    let (mut a, _rx, h) = app_seeing(kodi_artists());
    a.opts = Some(Vec::new());
    a.open_list(&h);
    a.list_key(0, KeyCode::Char('r'), KeyModifiers::NONE, &h);
    a.list_key(0, KeyCode::Esc, KeyModifiers::NONE, &h);
    assert!(a.prompt.is_empty() && matches!(a.overlay, Overlay::List { .. }), "the filter cleared, still open");
    a.list_key(0, KeyCode::Esc, KeyModifiers::NONE, &h);
    assert_eq!(a.overlay, Overlay::None);
    // the palette
    a.overlay = Overlay::Palette(0);
    a.palette_key(0, KeyCode::Char('m'), KeyModifiers::NONE, &h);
    a.palette_key(0, KeyCode::Esc, KeyModifiers::NONE, &h);
    assert!(a.prompt.is_empty() && matches!(a.overlay, Overlay::Palette(_)));
    a.palette_key(0, KeyCode::Esc, KeyModifiers::NONE, &h);
    assert_eq!(a.overlay, Overlay::None);
}

#[test]
fn reads_ahead_what_may_be_opened_next() {
    let st = kodi_albums_browsed();
    let (mut a, rx, h, held) = app_holding(st.clone());
    a.opts = Some(Vec::new());
    a.open_list(&h);
    // a browsed list arrives: the folder highlighted, and the one above
    *held.lock().unwrap() = Arc::new(st);
    a.st = h.state();
    a.list_sync(Change::List);
    let asked: Vec<Action> = rx.try_iter().filter(|x| matches!(x, Action::ListAhead(_))).collect();
    assert_eq!(asked, vec![Action::ListAhead(vec!["music|Dummy|musicdb://albums/250/".into(), "music||".into()])]);
    // moving: the next one (the folder above asked for again with it)
    a.list_key(0, KeyCode::Down, KeyModifiers::NONE, &h);
    let asked: Vec<Action> = rx.try_iter().filter(|x| matches!(x, Action::ListAhead(_))).collect();
    assert_eq!(asked, vec![Action::ListAhead(vec!["music|A Polished Solid|musicdb://albums/251/".into(), "music||".into()])]);
    // the same again: not asked twice
    a.read_ahead(1);
    assert!(!rx.try_iter().any(|x| matches!(x, Action::ListAhead(_))));
}

#[test]
fn remote_preview_says_the_tracks() {
    let (mut a, _rx, _h) = app_seeing(kodi_paused());
    a.opts = Some(Vec::new());
    let line = a.track_line().expect("Kodi says its tracks");
    assert!(line.starts_with("Audio: "), "{line}");
    // in remote mode, with the preview open, it's under the picture
    a.mode = Mode::Remote;
    a.preview.open = true;
    let s = screen(&mut a, DESKTOP);
    assert!(s.contains(&line), "{s}");
    // player mode: the card already says it
    a.mode = Mode::Player;
    let s = screen(&mut a, DESKTOP);
    assert_eq!(s.matches(&line).count(), 0, "{s}");
}

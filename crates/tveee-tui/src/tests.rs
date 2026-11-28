/// Every vendor plugin's apps and providers, as the binary installs them
/// (call before looking any up).
pub(super) fn install_all() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        tveee_core::labels::ignore_cache();
        let mut apps = tveee_plugin_philips::apps();
        apps.extend(tveee_plugin_youtube::apps());
        tveee_core::builtin::apps::install(apps);
        let mut providers = tveee_plugin_philips::providers();
        providers.extend(tveee_plugin_kodi::providers());
        providers.extend(tveee_plugin_youtube::providers());
        tveee_core::provider::install(providers);
    });
}

#[test]
fn guide_sub_headers() {
    use GuideItem::{Header, Row};
    let g = [Some("Ent"), Some("Ent"), Some("HD"), Some("HD"), Some("Radio")];
    // all of it
    assert_eq!(guide_items(&g, 0, 10), [Header("Ent"), Row(0), Row(1), Header("HD"), Row(2), Row(3), Header("Radio"), Row(4)]);
    // the selected row centred
    assert_eq!(guide_items(&g, 3, 4), [Header("HD"), Row(2), Row(3), Header("Radio")]);
    // scrolled into a group: its header stays at the top
    let long = [Some("Ent"); 6];
    assert_eq!(guide_items(&long, 4, 3), [Header("Ent"), Row(4), Row(5)]);
    // ... but not when the next line starts another group
    assert_eq!(guide_items(&g, 4, 3), [Row(3), Header("Radio"), Row(4)]);
    // ungrouped (no lineup): just rows
    assert_eq!(guide_items(&[None, None], 1, 5), [Row(0), Row(1)]);
    assert!(guide_items(&[], 0, 5).is_empty());
}

#[test]
fn guide_filters_and_moves() {
    let row = |id: u32, n: &str, name: &str, now: &str, fav: bool| GuideRow {
        channel: tveee_core::provider::Channel { id, number: Some(n.into()), name: Some(name.into()), radio: false, favourite: fav, delivery: None, dvb: None },
        events: vec![tveee_core::provider::Event { channel: id, title: now.into(), start: 0, end: 1, genre: None, info: None }],
    };
    let rows = [
        row(159, "1", "BBC ONE Lon", "Countryfile", true),
        row(158, "2", "BBC TWO", "Race Across the World", false),
        row(156, "4", "Channel 4", "50 First Dates", false),
        row(143, "18", "More 4", "Come Dine with Me", true),
        GuideRow { channel: tveee_core::provider::Channel { radio: true, ..row(12, "722", "BBC Essex", "Dotun Adebayo", false).channel }, events: Vec::new() },
    ];
    let ids = |tab, q| guide_shown(&rows, tab, q, 0).iter().map(|r| r.channel.id).collect::<Vec<_>>();
    assert_eq!(ids(GuideTab::All, ""), [159, 158, 156, 143, 12]);
    assert_eq!(ids(GuideTab::Radio, ""), [12]);
    assert_eq!(ids(GuideTab::Radio, "essex"), [12]);
    assert_eq!((GuideTab::All.next(), GuideTab::Radio.next()), (GuideTab::Favourites, GuideTab::All));
    assert_eq!(ids(GuideTab::Favourites, ""), [159, 143]);
    assert_eq!(ids(GuideTab::All, "bbc"), [159, 158, 12]);
    assert_eq!(ids(GuideTab::All, "1"), [159, 143]); // number starts with 1
    assert_eq!(ids(GuideTab::All, "dine"), [143]); // what's on
    assert_eq!(ids(GuideTab::All, "bbc race"), [158]); // every word
    assert_eq!(ids(GuideTab::Favourites, "two"), Vec::<u32>::new());
    let all = ids(GuideTab::All, "");
    assert_eq!(guide_move(&all, 158, KeyCode::Down), 156);
    assert_eq!(guide_move(&all, 159, KeyCode::Up), 12);
    assert_eq!(guide_move(&all, 159, KeyCode::End), 12);
    assert_eq!(guide_move(&[159, 143], 158, KeyCode::Char('x')), 159); // filtered away: first
    assert_eq!(guide_move(&[], 158, KeyCode::Down), 158);
    // later in the day: what's on then, and after it
    let ev = |title: &str, start, end| tveee_core::provider::Event { channel: 1, title: title.into(), start, end, genre: None, info: None };
    let day = GuideRow { channel: rows[0].channel.clone(), events: vec![ev("Apple", 0, 100), ev("Banana", 100, 200), ev("Cherry", 250, 300)] };
    let titles = |t| {
        let (a, b) = day.at(t);
        (a.map(|e| e.title.clone()), b.map(|e| e.title.clone()))
    };
    assert_eq!(titles(50), (Some("Apple".into()), Some("Banana".into())));
    assert_eq!(titles(150), (Some("Banana".into()), Some("Cherry".into())));
    assert_eq!(titles(220), (None, Some("Cherry".into()))); // a gap
    assert_eq!(titles(400), (None, None));
    assert!(guide_matches(&day, "banana", 150) && !guide_matches(&day, "banana", 260));
    // the grid: 60s a cell, from 0
    let grid = |w| grid_row(&day.events, 0, 60, w, 0, None).into_iter().map(|(_, t, _)| t).collect::<String>();
    // Apple 0-100s (1 cell), Banana 100-200s (cells 1-3), gap, Cherry 250-300s (cell 4)
    assert_eq!(grid(6), "││B │ ");
    let long = [ev("A long title here", 0, 1800), ev("B", 1800, 2400)];
    let row = grid_row(&long, 0, 60, 40, 0, Some(1800));
    let flat: String = row.iter().map(|(_, t, _)| t.as_str()).collect();
    // on the half hour: no time (the ruler says it)
    assert_eq!(flat, "│A long title here            │B        ");
    assert_eq!(row[0], (Seg::Edge, "│".into(), false));
    assert_eq!(row[1], (Seg::Title, "A long title here".into(), false));
    // the focused programme (B, from 1800) is flagged, its spaces too
    let marked: String = row.iter().filter(|(_, _, m)| *m).map(|(_, t, _)| t.as_str()).collect();
    assert_eq!(marked, "│B        ");
    // off the half hour: the time too
    let odd: String = grid_row(&[ev("C", 300, 1800)], 0, 60, 30, 0, None).iter().map(|(_, t, _)| t.as_str()).collect();
    assert_eq!(odd.trim_end(), "     │00:05 C");
    // too narrow for the time as well: just the title
    let narrow: String = grid_row(&[ev("Newsnight", 300, 900)], 0, 60, 20, 0, None).iter().map(|(_, t, _)| t.as_str()).collect();
    assert_eq!(narrow.trim_end(), "     │Newsnight");
    // started before the left edge; and a cut-off title
    let flat: String = grid_row(&long, 300, 60, 10, 0, None).iter().map(|(_, t, _)| t.as_str()).collect();
    assert_eq!(flat, "◂A long t…");
    let (ruler, mark) = grid_ruler(0, 60, 40, 600, 0);
    assert_eq!((ruler.trim_end(), mark), ("00:00                         00:30", Some(10)));
    // now on a time: the marker goes just before it, or after it at the edge
    assert_eq!(grid_ruler(0, 60, 40, 30 * 60 + 60, 0).1, Some(29));
    assert_eq!(grid_ruler(0, 60, 40, 60, 0).1, Some(5));
    assert_eq!(grid_scale(90), 80);
    // the details text
    let mut e = ev("Garage Squad", 0, 3600);
    e.genre = Some(6);
    e.info = Some("The team restore a classic car for a family.\nNew series.".into());
    let info = e.info.clone();
    let lines: Vec<String> = details_text(&e, &rows[3].channel, Some(info.as_deref()), 30, 0).into_iter().map(|(l, _)| l).collect();
    assert_eq!(
        lines,
        ["Garage Squad", "Thu 00:00–01:00 (1h00) · 18 More 4 · documentary", "", "The team restore a classic car", "for a family.", "New series."]
    );
    // still loading: no body yet (the panel keeps its size with blank lines)
    let loading: Vec<String> = details_text(&e, &rows[3].channel, None, 30, 0).into_iter().map(|(l, _)| l).collect();
    assert_eq!(loading.len(), 3);
    // access tags become words on the time line
    e.info = Some("Teleshopping. [S,AD] [HD]".into());
    let info = e.info.clone();
    let lines: Vec<String> = details_text(&e, &rows[3].channel, Some(info.as_deref()), 60, 0).into_iter().map(|(l, _)| l).collect();
    assert_eq!(lines, ["Garage Squad", "Thu 00:00–01:00 (1h00) · 18 More 4 · documentary · subtitles · audio description · HD", "", "Teleshopping."]);
    assert_eq!(split_tags("News. [SL]"), ("News.", vec!["signed"]));
    assert_eq!(split_tags("Film [2019] [S]"), ("Film [2019]", vec!["subtitles"]));
    assert_eq!(split_tags("[S]"), ("", vec!["subtitles"]));
    assert_eq!(split_tags("Nothing here"), ("Nothing here", vec![]));
    // switching tabs: the highlight stays if shown, else what's on, else the first
    assert_eq!(guide_tab_pick(&[159, 143], 143, Some(159)), 143);
    assert_eq!(guide_tab_pick(&[159, 143], 156, Some(159)), 159);
    assert_eq!(guide_tab_pick(&[12], 156, Some(159)), 12);
    assert_eq!(guide_tab_pick(&[], 156, None), 156);
    // steps land on the half hour
    assert_eq!(guide_time_at(1_000, 0), 1_000);
    assert_eq!(guide_time_at(1_000, 1), 1_800);
    assert_eq!(guide_time_at(1_800, 2), 5_400);
    assert_eq!(textwrap("needs tveee to be paired", 12), ["needs tveee", "to be paired"]);
    assert_eq!(input_for(KeyCode::Char('g'), KeyModifiers::NONE, Mode::Player), Input::OpenGuide);
    assert_eq!(input_for(KeyCode::Char('z'), KeyModifiers::NONE, Mode::Player), Input::ToggleVisualiser);
}

#[test]
fn settling_waits_out_the_steps_on_the_way() {
    let t0 = Instant::now();
    let ms = |n: u64| t0 + Duration::from_millis(n);
    let st = |expect: Option<&str>, before: &str| {
        let passing = vec!["projectivy".to_string(), "android".to_string()];
        Settling::new("launching Netflix...".into(), expect.map(str::to_string), Some(before.into()), Some(format!("In {before}")), passing, t0)
    };
    // launching from YouTube: the home screen, then Netflix loading
    let mut s = st(Some("netflix"), "youtube");
    s.observe(Some("youtube"), Some("In youtube"), ms(1000)); // nothing new
    assert!(!s.done(ms(2000)));
    s.observe(Some("projectivy"), Some("In Projectivy"), ms(2000)); // on the way
    assert!(!s.done(ms(4000)) && s.arrived.is_none());
    s.observe(Some("netflix"), None, ms(3000)); // in front, nothing to say yet
    assert!(!s.done(ms(6500)) && s.arrived.is_none());
    s.observe(Some("netflix"), Some("In Netflix"), ms(4000)); // arrived
    s.observe(Some("netflix"), Some("Browsing Netflix"), ms(5000)); // still loading
    assert!(!s.done(ms(6000)));
    // then it stays put: settled
    assert!(s.done(ms(5000) + SETTLE_STEADY));

    // nothing at all happens: give up after SETTLE
    let s = st(Some("netflix"), "youtube");
    assert!(!s.done(ms(2000)) && s.done(t0 + SETTLE));

    // something else comes up on the way: show it
    let mut s = st(Some("netflix"), "youtube");
    s.observe(Some("dialog.app"), Some("In Dialog"), ms(500));
    assert!(s.done(ms(500)));
    // or leaves the app it arrived in
    let mut s = st(Some("netflix"), "youtube");
    s.observe(Some("netflix"), Some("In Netflix"), ms(500));
    s.observe(Some("projectivy"), Some("In Projectivy"), ms(1000));
    assert!(s.done(ms(1000)));

    // searching the app in front: arrives when the what line changes
    let mut s = st(Some("youtube"), "youtube");
    s.observe(Some("youtube"), Some("In youtube"), ms(500));
    assert!(s.arrived.is_none());
    s.observe(Some("youtube"), Some("Searching YouTube"), ms(1000));
    assert!(s.arrived.is_some());

    // nothing expected (switching channel): any change arrives
    let mut s = st(None, "zapster");
    s.observe(Some("zapster"), Some("BBC Two"), ms(500));
    s.observe(Some("zapster"), Some("BBC Two: Newsnight"), ms(1500));
    assert!(!s.done(ms(2500)) && s.done(ms(1500) + SETTLE_STEADY));

    // a run of changes can't hold it forever
    let mut s = st(None, "zapster");
    for i in 1..20 {
        s.observe(Some("zapster"), Some(&format!("step {i}")), ms(i * 1000));
    }
    assert!(s.done(t0 + SETTLE_MAX));
}

use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[test]
fn keys_by_mode() {
    let k = |c, m| input_for(c, KeyModifiers::NONE, m);
    assert_eq!(k(KeyCode::Char('q'), Mode::Player), Input::Quit);
    assert_eq!(input_for(KeyCode::Char('c'), KeyModifiers::CONTROL, Mode::Remote), Input::Quit);
    assert_eq!(k(KeyCode::Right, Mode::Player), Input::Do(Action::Seek(10.0)));
    assert_eq!(k(KeyCode::Right, Mode::Remote), Input::Do(Action::Key(22)));
    assert_eq!(k(KeyCode::Up, Mode::Player), Input::Do(Action::Volume(1)));
    assert_eq!(k(KeyCode::Up, Mode::Remote), Input::Do(Action::Key(19)));
    assert_eq!(k(KeyCode::Enter, Mode::Remote), Input::Do(Action::Key(KEY_OK)));
    assert_eq!(k(KeyCode::Enter, Mode::Player), Input::Nothing);
    assert_eq!(k(KeyCode::Backspace, Mode::Player), Input::Do(Action::Key(KEY_BACK)));
    assert_eq!(k(KeyCode::Char('h'), Mode::Player), Input::Do(Action::Key(KEY_HOME)));
    assert_eq!(k(KeyCode::Char('5'), Mode::Player), Input::Do(Action::Jump(0.5)));
    assert_eq!(k(KeyCode::Tab, Mode::Player), Input::ToggleMode);
    // remote mode: the keyboard goes to the TV
    let text = |s: &str| Input::Do(Action::Text(s.into()));
    assert_eq!(k(KeyCode::Char('h'), Mode::Remote), text("h"));
    assert_eq!(k(KeyCode::Char('q'), Mode::Remote), text("q"));
    assert_eq!(k(KeyCode::Char(' '), Mode::Remote), text(" "));
    assert_eq!(input_for(KeyCode::Char('A'), KeyModifiers::SHIFT, Mode::Remote), text("A"));
    assert_eq!(k(KeyCode::Char('é'), Mode::Remote), Input::Nothing);
    assert_eq!(k(KeyCode::Esc, Mode::Remote), Input::Do(Action::Key(KEY_BACK)));
    assert_eq!(k(KeyCode::Backspace, Mode::Remote), Input::Do(Action::Key(KEY_DEL)));
    assert_eq!(k(KeyCode::Home, Mode::Remote), Input::Do(Action::Key(KEY_HOME)));
    // and these still get out
    assert_eq!(k(KeyCode::Tab, Mode::Remote), Input::ToggleMode);
    assert_eq!(k(KeyCode::Esc, Mode::Player), Input::Quit);
    assert_eq!(k(KeyCode::Char('x'), Mode::Player), Input::Nothing);
}

fn ok(_: &tveee_core::actions::Cx, _: &[String]) -> Result<tveee_core::actions::Output> {
    Ok(Default::default())
}

fn items() -> Vec<ActionDescriptor> {
    vec![
        ActionDescriptor::new("power.sleep", "power", "sleep", ok).key('s'),
        ActionDescriptor::new("power.off", "power", "power off", ok).key('X').confirm("sure?"),
    ]
}

#[test]
fn menu_keys() {
    let it = items();
    let k = |o, c| overlay_key(o, c, &it);
    // a plain item runs straight away; a risky one asks first
    assert_eq!(k(Overlay::Menu(0), KeyCode::Char('s')), (Overlay::None, Some(Action::Run { id: "power.sleep", args: vec![] })));
    assert_eq!(k(Overlay::Menu(0), KeyCode::Char('X')), (Overlay::Confirm { id: "power.off", yes: false }, None));
    assert_eq!(k(Overlay::Menu(1), KeyCode::Enter), (Overlay::Confirm { id: "power.off", yes: false }, None));
    assert_eq!(k(Overlay::Menu(0), KeyCode::Down), (Overlay::Menu(1), None));
    assert_eq!(k(Overlay::Menu(1), KeyCode::Down), (Overlay::Menu(0), None));
    assert_eq!(k(Overlay::Menu(0), KeyCode::Up), (Overlay::Menu(1), None));
    assert_eq!(k(Overlay::Menu(0), KeyCode::Esc), (Overlay::None, None));
    assert_eq!(k(Overlay::Menu(0), KeyCode::Char('z')), (Overlay::Menu(0), None));
}

#[test]
fn confirmations_default_to_no() {
    let it = items();
    let k = |o, c| overlay_key(o, c, &it);
    let ask = Overlay::Confirm { id: "power.off", yes: false };
    assert_eq!(k(ask, KeyCode::Enter), (Overlay::None, None));
    assert_eq!(k(ask, KeyCode::Char('n')), (Overlay::None, None));
    assert_eq!(k(ask, KeyCode::Char(' ')), (Overlay::None, None));
    assert_eq!(k(ask, KeyCode::Char('y')), (Overlay::None, Some(Action::Run { id: "power.off", args: vec![] })));
    let yes = Overlay::Confirm { id: "power.off", yes: true };
    assert_eq!(k(ask, KeyCode::Right), (yes, None));
    assert_eq!(k(yes, KeyCode::Enter), (Overlay::None, Some(Action::Run { id: "power.off", args: vec![] })));
}

#[test]
fn bar_clicks() {
    // a 10-cell bar starting at x=9: cell 0 is 5%, cell 9 is 95%
    assert_eq!(bar_fraction(9, 9, 10), 0.05);
    assert_eq!(bar_fraction(18, 9, 10), 0.95);
    // clicks past either end clamp to the ends
    assert_eq!(bar_fraction(3, 9, 10), 0.05);
    assert_eq!(bar_fraction(40, 9, 10), 0.95);
    // the marker lands on the clicked cell
    for c in 0..10u16 {
        let frac = bar_fraction(9 + c, 9, 10);
        assert_eq!(bar(frac, 10).chars().position(|ch| ch == '┃'), Some(usize::from(c)));
    }
}

#[test]
fn buttons_wrap_and_have_unique_keys() {
    for mode in [Mode::Player, Mode::Remote] {
        let bs = buttons(mode, true, true);
        let mut keys: Vec<_> = bs.iter().map(|b| b.key).filter(|k| !k.is_empty()).collect();
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), bs.iter().filter(|b| !b.key.is_empty()).count(), "{mode:?}");
        // narrow terminal: several rows, and nothing past the edge
        let places = flow(&bs, 40);
        assert!(places.iter().any(|(r, _)| *r > 0));
        for (b, (_, x)) in bs.iter().zip(&places) {
            assert!(x + button_width(b) <= 40, "{} overflows", b.label);
        }
        // cut to one row: what matters most stays
        for width in [70, 100, 120, 200] {
            let row = one_row(buttons(mode, true, true), width);
            assert!(flow(&row, width).iter().all(|(r, _)| *r == 0), "{mode:?} at {width}: more than a row");
            for must in ["quit", "cmds"] {
                assert!(row.iter().any(|b| b.label == must), "{mode:?} at {width}: no {must}");
            }
        }
    }
}

#[test]
fn list_names() {
    assert_eq!(list_name("the home screen"), "Home screen");
    assert_eq!(list_name("the Play Store"), "Play Store");
    assert_eq!(list_name("iPlayer"), "iPlayer");
    assert_eq!(list_name("live TV"), "Live TV");
    assert_eq!(list_name("YouTube"), "YouTube");
}

fn searchables() -> Vec<search::Searchable> {
    search::parse_searchable("    org.xbmc.kodi/.XBMCSearchableActivity\n@@\n@@\n", Some("package:com.google.android.youtube.tv\n"))
}

#[test]
fn search_box_targets() {
    install_all();
    let s = searchables();
    // the app in front first, then the searchable ones
    let t = search_targets(Some("com.netflix.ninja"), &s);
    let got: Vec<_> = t.iter().map(|t| (t.name.as_str(), t.method)).collect();
    assert_eq!(got, vec![("Netflix", "keys"), ("Kodi", "search"), ("YouTube", "results link")]);
    // a searchable app in front isn't listed twice
    let t = search_targets(Some("org.xbmc.kodi"), &s);
    assert_eq!(t.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), vec!["Kodi", "YouTube"]);
    // nothing to search on the home screen
    let t = search_targets(Some("com.spocky.projengmenu"), &s);
    assert_eq!(t[0].name, "Kodi");
}

#[test]
fn search_box_keys() {
    install_all();
    let t = search_targets(Some("com.netflix.ninja"), &searchables());
    let none = KeyModifiers::NONE;
    let mut q = String::new();
    assert_eq!(search_key(0, KeyCode::Tab, none, &mut q, &t), (Overlay::Search(1), None));
    assert_eq!(search_key(0, KeyCode::Up, none, &mut q, &t), (Overlay::Search(2), None));
    assert_eq!(search_key(0, KeyCode::Char('d'), none, &mut q, &t), (Overlay::Search(0), None));
    assert_eq!(q, "d");
    // Enter with nothing typed stays open
    let mut empty = String::new();
    assert_eq!(search_key(1, KeyCode::Enter, none, &mut empty, &t), (Overlay::Search(1), None));
    q.push_str("une");
    assert_eq!(
        search_key(0, KeyCode::Enter, none, &mut q, &t),
        (Overlay::None, Some(Action::Search { pkg: "com.netflix.ninja".into(), query: "dune".into(), keys: true }))
    );
    assert_eq!(
        search_key(1, KeyCode::Enter, none, &mut q, &t),
        (Overlay::None, Some(Action::Search { pkg: "org.xbmc.kodi".into(), query: "dune".into(), keys: false }))
    );
    assert_eq!(search_key(1, KeyCode::Esc, none, &mut q, &t), (Overlay::None, None));
}

#[test]
fn typing_a_link() {
    let none = KeyModifiers::NONE;
    let mut t = String::new();
    for c in "youtu.be/x".chars() {
        assert_eq!(prompt_key("apps.open_link", KeyCode::Char(c), none, &mut t), (Overlay::Prompt("apps.open_link"), None));
    }
    assert_eq!(prompt_key("apps.open_link", KeyCode::Backspace, none, &mut t), (Overlay::Prompt("apps.open_link"), None));
    assert_eq!(t, "youtu.be/");
    assert_eq!(prompt_key("apps.open_link", KeyCode::Char('u'), KeyModifiers::CONTROL, &mut t), (Overlay::Prompt("apps.open_link"), None));
    assert_eq!(t, "");
    // Enter on nothing stays put; with text it runs the item
    assert_eq!(prompt_key("apps.open_link", KeyCode::Enter, none, &mut t), (Overlay::Prompt("apps.open_link"), None));
    t.push_str("  vlc http://h/f.mkv ");
    assert_eq!(
        prompt_key("apps.open_link", KeyCode::Enter, none, &mut t),
        (Overlay::None, Some(Action::Run { id: "apps.open_link", args: vec!["vlc http://h/f.mkv".into()] }))
    );
    assert_eq!(prompt_key("apps.open_link", KeyCode::Esc, none, &mut t), (Overlay::None, None));
    assert_eq!(paste_text("https://youtu.be/x\r\n"), "https://youtu.be/x");
    assert_eq!(paste_text("vlc\thttp://h/f.mkv"), "vlc http://h/f.mkv");
}

#[test]
fn text_items_open_a_prompt() {
    let items = vec![ActionDescriptor::new("apps.open_link", "apps", "open a link", ok).key('o').text("Link:", "")];
    assert_eq!(pick(&items, 0), (Overlay::Prompt("apps.open_link"), None));
    assert_eq!(overlay_key(Overlay::Menu(0), KeyCode::Char('o'), &items), (Overlay::Prompt("apps.open_link"), None));
}

fn rows(pm: &str, guessed: &[apps::Guess]) -> Vec<Entry> {
    launcher_entries(&apps::installed_aliases(pm), guessed, None)
}

#[test]
fn launcher_keys() {
    install_all();
    let all = rows("package:com.google.android.youtube.tv\npackage:org.videolan.vlc\npackage:org.xbmc.kodi\n", &[]);
    let list = tab_list(&all, AppTab::Known);
    // home, youtube, kodi, vlc (table order)
    let names: Vec<_> = list.iter().filter_map(|e| e.alias).collect();
    assert_eq!(names, vec!["home", "youtube", "kodi", "vlc"]);
    let at = |sel| Overlay::Apps { tab: AppTab::Known, sel };
    let k = |sel, c| apps_key(AppTab::Known, sel, c, &list);
    assert_eq!(k(0, KeyCode::Enter), (Overlay::None, Some(Action::Launch("home".into()))));
    assert_eq!(k(0, KeyCode::Up), (at(3), None));
    assert_eq!(k(3, KeyCode::Down), (at(0), None));
    assert_eq!(k(0, KeyCode::Char('k')), (at(2), None));
    assert_eq!(k(0, KeyCode::Char('V')), (at(3), None));
    assert_eq!(k(1, KeyCode::Char('z')), (at(1), None));
    assert_eq!(k(1, KeyCode::Esc), (Overlay::None, None));
    assert_eq!(k(1, KeyCode::Backspace), (Overlay::None, None));
    // switching tabs starts at the top of the other one
    for key in [KeyCode::Tab, KeyCode::BackTab] {
        assert_eq!(k(2, key), (Overlay::Apps { tab: AppTab::Other, sel: 0 }, None));
    }
    // ←/→: nothing (the rows have no value to change)
    for key in [KeyCode::Left, KeyCode::Right] {
        assert_eq!(k(2, key), (at(2), None));
    }
    assert_eq!(apps_key(AppTab::Other, 0, KeyCode::Tab, &[]), (at(0), None));
    // letters jump, even the ones that mean something on the main screen
    let list2 = tab_list(&rows("package:org.droidtv.nettvapp1814\npackage:com.apple.atve.androidtv.appletv\n", &[]), AppTab::Known);
    let names2: Vec<_> = list2.iter().filter_map(|e| e.alias).collect();
    assert_eq!(names2, vec!["home", "arte", "appletv"]);
    assert_eq!(apps_key(AppTab::Known, 0, KeyCode::Char('a'), &list2), (at(1), None));
    assert_eq!(apps_key(AppTab::Known, 1, KeyCode::Char('a'), &list2), (at(2), None));
    assert_eq!(apps_key(AppTab::Known, 2, KeyCode::Char('a'), &list2), (at(1), None));
    // empty list doesn't panic
    assert_eq!(apps_key(AppTab::Other, 0, KeyCode::Down, &[]), (Overlay::Apps { tab: AppTab::Other, sel: 0 }, None));
}

#[test]
fn app_actions() {
    let all = launcher_entries(&apps::installed_aliases("package:org.videolan.vlc\n"), &[], None);
    let home = &all[0];
    let vlc = all.iter().find(|e| e.target == "vlc").unwrap();
    assert_eq!(app_choices(home, true, false), vec![AppChoice::Front, AppChoice::Cancel]);
    assert_eq!(app_choices(vlc, false, false), vec![AppChoice::Open, AppChoice::Search, AppChoice::Cancel]);
    assert_eq!(app_choices(vlc, true, false), vec![AppChoice::Front, AppChoice::Restart, AppChoice::Close, AppChoice::Search, AppChoice::Cancel]);
    // persistent (live TV): no closing or restarting
    assert_eq!(app_choices(vlc, true, true), vec![AppChoice::Front, AppChoice::Search, AppChoice::Cancel]);
    assert_eq!(choice_action(AppChoice::Close, vlc), (Some(Action::Close("org.videolan.vlc".into())), false));
    assert_eq!(choice_action(AppChoice::Restart, vlc), (Some(Action::Restart { pkg: "org.videolan.vlc".into(), target: "vlc".into() }), false));
    assert_eq!(choice_action(AppChoice::Search, vlc), (None, true));
    // space on a row opens its actions
    let list = tab_list(&all, AppTab::Known);
    assert_eq!(apps_key(AppTab::Known, 1, KeyCode::Char(' '), &list), (Overlay::AppActions { tab: AppTab::Known, sel: 1, at: 0 }, None));
}

#[test]
fn non_default_home_apps_go_to_other() {
    let pm = "package:com.spocky.projengmenu\npackage:com.google.android.tvlauncher\npackage:org.videolan.vlc\n";
    let all = launcher_entries(&apps::installed_aliases(pm), &[], Some("com.spocky.projengmenu"));
    let names = |tab| tab_list(&all, tab).iter().map(|e| e.name.clone()).collect::<Vec<_>>();
    // "home" is Projectivy here, so it isn't listed again; the Google TV
    // launcher is another home app
    assert_eq!(names(AppTab::Known), vec!["Home screen", "VLC"]);
    assert_eq!(names(AppTab::Other), vec!["com.google.android.tvlauncher"]);
    assert_eq!(tab_list(&all, AppTab::Other)[0].section, "Other home apps");
    // with no default, both are other home apps
    let all = launcher_entries(&apps::installed_aliases(pm), &[], None);
    assert_eq!(tab_list(&all, AppTab::Other).len(), 2);
}

#[test]
fn launcher_lists_other_tv_apps() {
    install_all();
    let g = |p: &str, k, c: Option<&str>| apps::Guess { package: p.into(), kind: k, component: c.map(Into::into) };
    let guessed = vec![
        g("nextapp.fx", apps::Kind::Player, Some("nextapp.fx/.ui.Launcher")),
        g("com.example.game", apps::Kind::Other, Some("com.example.game/.Main")),
        // already listed by alias: not repeated
        g("org.videolan.vlc", apps::Kind::Player, Some("org.videolan.vlc/.StartActivity")),
        // not in the TV launcher: can't be launched from here
        g("com.android.tv.settings", apps::Kind::Launcher, None),
        // in tveee's table without an alias: its kind, but a specific name
        g("com.spocky.projengmenu", apps::Kind::Launcher, Some("com.spocky.projengmenu/.ui.home.MainActivity")),
        g("org.droidtv.nettvbrowser", apps::Kind::Other, Some("org.droidtv.nettvbrowser/.Main")),
    ];
    let all = rows("package:org.videolan.vlc\n", &guessed);
    let shown = |tab| tab_list(&all, tab).iter().map(|e| (e.section, e.name.clone(), e.alias.or(e.tag))).collect::<Vec<_>>();
    assert_eq!(shown(AppTab::Known), vec![("Other home apps", "Home screen".to_string(), Some("home")), ("Players", "VLC".into(), Some("vlc"))]);
    // by section, then name
    assert_eq!(
        shown(AppTab::Other),
        vec![
            ("Other home apps", "com.spocky.projengmenu".to_string(), Some("home")),
            ("Video & music", "org.droidtv.nettvbrowser".into(), Some("video")),
            ("Players", "nextapp.fx".into(), Some("player")),
            ("Other apps", "com.example.game".into(), None),
        ]
    );
    // 4 apps under 4 headings
    assert_eq!(tab_rows(&all, AppTab::Other), 8);
    // other apps launch by their TV-launcher activity, and jump by name
    let other = tab_list(&all, AppTab::Other);
    assert_eq!(apps_key(AppTab::Other, 0, KeyCode::Char('n'), &other), (Overlay::Apps { tab: AppTab::Other, sel: 2 }, None));
    assert_eq!(apps_key(AppTab::Other, 2, KeyCode::Enter, &other), (Overlay::None, Some(Action::Launch("nextapp.fx/.ui.Launcher".into()))));
}

#[test]
fn batches_keys_in_order() {
    use Action::*;
    assert_eq!(batches(vec![Key(20), Key(20), Key(23), PlayPause, Key(4)]), vec![Batch::Keys(vec![20, 20, 23]), Batch::One(PlayPause), Batch::Keys(vec![4])]);
    assert_eq!(key_name(4), "back");
    assert_eq!(key_name(20), "down");
}

#[test]
fn volume_when_a_refresh_cant_read_it() {
    let s = Duration::from_secs;
    assert_eq!(volume_shown(Some((5, 60)), Some(((3, 60), s(1)))), (Some((5, 60)), None));
    // missed a read: the last one, as current, while recent
    assert_eq!(volume_shown(None, Some(((3, 60), s(4)))), (Some((3, 60)), None));
    // too old: greyed
    assert_eq!(volume_shown(None, Some(((3, 60), s(11)))), (None, Some((3, 60))));
    // never read: a grey empty bar
    assert_eq!(volume_shown(None, None), (None, None));
}

#[test]
fn asks_only_picture_terminals() {
    let env = |pairs: &'static [(&'static str, &'static str)]| move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string());
    assert!(!picture_terminal(env(&[("TERM", "xterm-256color")])));
    assert!(picture_terminal(env(&[("TERM", "xterm-kitty")])));
    assert!(picture_terminal(env(&[("TERM_PROGRAM", "iTerm.app")])));
    assert!(picture_terminal(env(&[("TERM", "xterm-256color"), ("TVEEE_PICTURES", "detect")])));
    assert!(!picture_terminal(env(&[("TERM", "xterm-kitty"), ("TVEEE_PICTURES", "halfblocks")])));
}

#[test]
fn actions_go_to_their_lanes() {
    use Action::*;
    assert_eq!(lane(&Key(19)), Lane::Keys);
    assert_eq!(lane(&Text("a".into())), Lane::Keys);
    assert_eq!(lane(&PlayPause), Lane::Act);
    assert_eq!(lane(&Launch("youtube".into())), Lane::Act);
    assert_eq!(lane(&LoadGuide), Lane::Read);
    assert_eq!(lane(&CheckRunning), Lane::Read);
    assert_eq!(lane(&PrefetchInfo), Lane::Idle);
}

#[test]
fn background_waits_for_a_lull() {
    let sh = Shared::new("x".into(), Arc::new(AtomicBool::new(false)));
    *lock(&sh.input_at) = Instant::now() - LULL * 2;
    // not before the first refresh is up
    assert!(!sh.idle());
    sh.shown.store(true, Ordering::Relaxed);
    assert!(sh.idle());
    // nor while an action runs
    sh.busy.fetch_add(1, Ordering::Relaxed);
    assert!(!sh.idle());
    sh.busy.fetch_sub(1, Ordering::Relaxed);
    // nor just after the user asked for something
    sh.touched();
    assert!(!sh.idle());
    assert!(sh.channel_stale.load(Ordering::Relaxed));
}

#[test]
fn coalesces_repeats() {
    use Action::*;
    assert_eq!(
        coalesce(vec![Seek(10.0), Seek(10.0), Volume(1), Volume(1), Volume(-1), Refresh, Refresh, Seek(-60.0)]),
        vec![Seek(20.0), Volume(1), Refresh, Seek(-60.0)]
    );
    // typing fast: one `input text`
    let t = |s: &str| Text(s.into());
    assert_eq!(coalesce(vec![t("c"), t("a"), t("t"), Key(66), t("s")]), vec![t("cat"), Key(66), t("s")]);
}

#[test]
fn last_seen_round_trip() {
    let l = LastSeen { what: "Watching YouTube".into(), title: Some("A video".into()), volume: Some((12, 60)), at: 1_700_000_000 };
    assert_eq!(super::last::parse(&super::last::format(&l)), Some(l));
    let bare = LastSeen { what: "Live TV".into(), title: None, volume: None, at: 5 };
    assert_eq!(super::last::parse(&super::last::format(&bare)), Some(bare));
    assert_eq!(super::last::parse("what=x\n"), None); // no time
    assert_eq!(super::last::parse(""), None);
}

#[test]
fn palette_keys() {
    let k = |c, m| input_for(c, m, Mode::Player);
    assert_eq!(k(KeyCode::Char(':'), KeyModifiers::NONE), Input::OpenPalette);
    assert_eq!(k(KeyCode::Char('p'), KeyModifiers::CONTROL), Input::OpenPalette);
    // in remote mode too (typing goes to the TV, Ctrl-P doesn't)
    assert_eq!(input_for(KeyCode::Char('p'), KeyModifiers::CONTROL, Mode::Remote), Input::OpenPalette);
    assert_eq!(input_for(KeyCode::Char(':'), KeyModifiers::NONE, Mode::Remote), Input::Do(Action::Text(":".into())));
}

#[test]
fn quick_keys() {
    let k = |c| input_for(KeyCode::Char(c), KeyModifiers::NONE, Mode::Player);
    assert_eq!(k('F'), Input::Do(Action::Run { id: "apps.free", args: vec![] }));
    assert_eq!(k('S'), Input::Do(Action::Run { id: "power.sleep", args: vec![] }));
    assert_eq!(k('R'), Input::RestartFront);
    // typed into the TV in remote mode
    assert_eq!(input_for(KeyCode::Char('F'), KeyModifiers::NONE, Mode::Remote), Input::Do(Action::Text("F".into())));
}

#[test]
fn view_keys() {
    let none = KeyModifiers::NONE;
    assert_eq!(view_key(KeyCode::F(1), none), Some(View::Guide));
    assert_eq!(view_key(KeyCode::F(2), none), Some(View::Apps));
    assert_eq!(view_key(KeyCode::F(3), none), Some(View::Remote));
    assert_eq!(view_key(KeyCode::Char('2'), KeyModifiers::ALT), Some(View::Apps));
    // digits alone: jumping through the video, or a list's filter
    assert_eq!(view_key(KeyCode::Char('2'), none), None);
    assert_eq!(view_key(KeyCode::F(4), none), None);
}

//! `tveee ui`: an interactive terminal remote (ratatui). Only built with the
//! `ui` cargo feature.
//!
//! The screen shows what is on (as `nowplaying`) plus the volume, and keys
//! control playback and volume. All TV traffic happens on a worker thread so
//! the screen never waits on adb; the worker keeps one devtools connection
//! open, which makes refreshing every second and exact seeking cheap.

use crate::devtools::{self, SeekTo};
use crate::error::Result;
use crate::menu::{MenuItem, Run};
use crate::nowplaying::{self, Activity, bar, clock, progress_bar};
use crate::plugins::apps::list_name;
use crate::plugins::{apps, info, media, search};
use crate::registry::Command;
use crate::tv::Tv;
use crate::Ctx;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

mod keymap;
mod lanes;
mod guide;
mod preview;
mod launcher;
mod ambi;
mod overlays;
mod drawing;
mod last;
#[cfg(test)]
mod tests;

pub use self::keymap::*;
pub use self::lanes::*;
pub use self::guide::*;
pub use self::preview::*;
pub use self::launcher::*;
pub use self::overlays::*;
pub use self::drawing::*;
pub use self::last::LastSeen;

const CAT: &str = "ui";
const REFRESH: Duration = Duration::from_secs(1);
/// A volume level and its maximum.
pub type Vol = (u32, u32);

/// The volume to show as current (this refresh's, or a recent one), and
/// the last known one when that's too old to (shown greyed). `last`: the
/// last reported volume and how long ago.
pub fn volume_shown(now: Option<Vol>, last: Option<(Vol, Duration)>) -> (Option<Vol>, Option<Vol>) {
    match (now, last) {
        (Some(v), _) => (Some(v), None),
        (None, Some((v, age))) if age < VOLUME_STALE => (Some(v), None),
        (None, Some((v, _))) => (None, Some(v)),
        (None, None) => (None, None),
    }
}

/// How long the last known volume is shown as current when a refresh can't
/// read it; after that it's greyed.
const VOLUME_STALE: Duration = Duration::from_secs(10);
/// What one fast-forward/rewind key press is assumed to skip, for apps we
/// can't seek exactly.
const KEY_STEP: f64 = 10.0;

/// A user request, handled by one of the lanes (see [`route`]).
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    PlayPause,
    Seek(f64),
    Jump(f64),
    Volume(i32),
    Mute,
    /// The Ambilight panel opened (true) or closed: the refresh reads it while open.
    AmbilightPanel(bool),
    /// The preview panel opened or closed: pictures are made while it's open.
    PreviewPanel(bool),
    /// Ask the TV to pair (it shows a PIN), then finish with the PIN typed.
    PairStart,
    PairFinish(String),
    /// Change the Ambilight (from its dialog).
    AmbilightChange(crate::plugins::ambilight::Change),
    /// Mute or unmute, the ui knowing which it is now (true: muted): one key,
    /// without reading the audio state first.
    MuteKnown(bool),
    /// Any remote key (back, home, d-pad, OK), by Android keycode.
    Key(u32),
    /// Type this text into the TV (remote mode).
    Text(String),
    /// Run menu item N (an index into the plugins' menu items).
    Menu(usize),
    /// Run menu item N with the text the user typed for it.
    MenuText(usize, String),
    /// Start an app: an alias, package or package/activity.
    Launch(String),
    /// Bring a running app's task to the front.
    Front(String),
    /// Force-stop an app (by package).
    Close(String),
    /// Force-stop, then start fresh.
    Restart { pkg: String, target: String },
    /// Find out which apps are running (for the launcher).
    CheckRunning,
    /// Search in an app; `keys` means by typing into it (no search intent).
    Search { pkg: String, query: String, keys: bool },
    /// Fetch the programme guide (for the guide pop-up).
    LoadGuide,
    /// Fetch a programme's description (channel, start).
    LoadInfo(u32, i64),
    /// Start reading the guide's descriptions, ahead of the details panel.
    PrefetchInfo,
    /// Switch live TV to a channel.
    Watch(crate::provider::Channel),
    /// Make a channel a favourite (true) or not.
    Favourite(crate::provider::Channel, bool),
    Refresh,
}

#[derive(Default)]
struct Snapshot {
    activity: Option<Activity>,
    volume: Option<(u32, u32)>,
    muted: Option<bool>,
    /// whether exact seeking is available (devtools video found)
    exact: bool,
    standby: bool,
    /// For the system panel.
    system: info::System,
    /// For the Ambilight panel, while it's open.
    ambilight: Option<crate::plugins::ambilight::Panel>,
    /// "player" or "player, guessed": what kind of app is in front.
    kind: Option<String>,
    power: Option<String>,
    focus: Option<String>,
}

/// The package of the app in front in a snapshot.
fn front_pkg(s: &Snapshot) -> Option<String> {
    s.focus.as_deref().map(|f| f.split('/').next().unwrap_or(f).to_string())
}

enum Msg {
    Snap(Box<Snapshot>, Instant),
    /// Feedback for the status line.
    Note(String),
    /// A seek landed here (seconds), so the bar can move before the next refresh.
    Seeked(f64, Instant),
    Failed(String),
    /// The launcher's rows (fetched once).
    Apps(Vec<Entry>),
    /// The apps with a proper search (fetched once).
    Searchable(Vec<search::Searchable>),
    /// The packages with a running process.
    /// Apps with a process, and those of them Android keeps running (persistent).
    Running(std::collections::BTreeSet<String>, std::collections::BTreeSet<String>),
    /// The guide pop-up's rows, or why there aren't any.
    Guide(std::result::Result<Guide, String>),
    /// A programme's description (channel, start).
    Info((u32, i64), std::result::Result<Option<String>, String>),
    /// Whether the TV has Ambilight tveee can control.
    Ambilight(bool),
    /// Pairing: the TV is showing a PIN; ask for it.
    AskPin,
    /// A picture for the preview panel.
    Preview(crate::preview::Picture),
    /// What the preview is doing, or why there's no picture.
    PreviewNote(String),
}

struct App {
    ip: String,
    snap: Option<Snapshot>,
    /// `snap` is what was last seen in an earlier run, not read yet.
    seeded: bool,
    /// What was last kept for the next run (kept again when it changes).
    kept: Option<LastSeen>,
    /// When `snap` was taken, to move the progress bar along between refreshes.
    at: Instant,
    note: String,
    error: Option<String>,
    mode: Mode,
    overlay: Overlay,
    menu: Vec<MenuItem>,
    /// Where the clickable things were drawn last frame.
    hits: Vec<(ratatui::layout::Rect, Hit)>,
    /// Show the system panel (memory, storage, ...).
    info: bool,
    /// Memory used (MB) at each refresh, oldest first, for the graph.
    mem_history: Vec<u64>,
    /// Installed apps for the launcher; None until the worker has looked.
    apps: Option<Vec<Entry>>,
    /// The launcher tab used last.
    apps_tab: AppTab,
    /// What has been typed into the open prompt.
    prompt: String,
    /// Packages with a running process (for the launcher).
    running: std::collections::BTreeSet<String>,
    /// Running apps that can't be closed (Android keeps them running).
    fixed: std::collections::BTreeSet<String>,
    /// Apps with a proper search; None until the worker has looked.
    searchable: Option<Vec<search::Searchable>>,
    /// The search box's app choices, made when it opens.
    search_targets: Vec<SearchTarget>,
    /// An action still settling: its label stands in for the what line.
    settling: Option<Settling>,
    /// Off (`--no-settle`): show every state the TV reports, as it comes.
    settle: bool,
    /// The last volume the TV reported (level, max), and when: shown while a
    /// refresh can't read it, greyed once it's [`VOLUME_STALE`] old.
    last_volume: Option<((u32, u32), Instant)>,
    /// The preview panel: open or not, its picture (ready to draw), where it
    /// came from, and what it's doing.
    preview_open: bool,
    preview: Option<ratatui_image::protocol::StatefulProtocol>,
    preview_source: &'static str,
    preview_note: String,
    /// How this terminal shows pictures (asked once at the start).
    picker: Option<ratatui_image::picker::Picker>,
    /// The TV has Ambilight tveee can control; and whether its panel is open.
    ambi_supported: bool,
    ambi_open: bool,
    /// The guide pop-up's rows; None until fetched.
    guide: Option<std::result::Result<Guide, String>>,
    /// The guide is being read afresh (the last copy shows meanwhile).
    guide_loading: bool,
    /// How many GUIDE_STEPs ahead of now the guide shows.
    guide_later: i64,
    /// Where the grid's left edge is drawn now (Unix seconds); it slides
    /// toward where it should be when scroll animations are on.
    guide_view: i64,
    /// When the slide last moved.
    guide_slid: Instant,
    /// The details panel, and how far it's scrolled.
    details: Details,
    details_scroll: usize,
    /// The programme the details are for (channel, start), to start each
    /// one from the top.
    details_of: Option<(u32, i64)>,
    /// Descriptions fetched so far (None: the guide has none), by (channel, start).
    infos: std::collections::HashMap<(u32, i64), Option<String>>,
    /// Descriptions asked for and not back yet; `want_info` is the next to ask for.
    infos_asked: std::collections::HashSet<(u32, i64)>,
    want_info: Option<(u32, i64)>,
}

/// An action under way waits this long for its next step: each change on
/// the way (see [`Settling`]) starts the wait again, and when nothing comes
/// the screen shows what the TV last reported.
const SETTLE: Duration = Duration::from_secs(4);
/// Once the action has arrived, the what line has to stay put this long (more
/// than a refresh) to count as settled: apps go on changing as they load.
const SETTLE_STEADY: Duration = Duration::from_millis(1600);
/// However it goes, the label gives way after this.
const SETTLE_MAX: Duration = Duration::from_secs(12);

/// An action in flight (launching, searching, ...). On the way the TV goes
/// through in-between states (the old app, the home screen, the new app
/// loading), and the "what" line shows `label` instead of them. It ends when
/// the action has arrived (the expected app is in front, or something
/// changed, and it has something to say: an app coming up shows nothing at
/// first) and then stayed the same for [`SETTLE_STEADY`]; when something
/// unexpected comes up; when nothing changes for [`SETTLE`]; or after
/// [`SETTLE_MAX`] in all. Then the latest snapshot is shown as it is.
#[derive(Debug, Clone, PartialEq)]
pub struct Settling {
    /// e.g. "launching Netflix..."
    pub label: String,
    /// The package that should end up in front, if the action says.
    pub expect: Option<String>,
    /// What was showing when it started (package in front, what line).
    pub before_pkg: Option<String>,
    pub before_what: Option<String>,
    /// Packages the TV can pass through on the way (home screens, Android's).
    pub passing: Vec<String>,
    pub since: Instant,
    /// When the last change on the way was seen.
    pub moved: Instant,
    /// Where it arrived: the package that came up.
    pub arrived: Option<Option<String>>,
    /// The last (package, what line) seen.
    pub last: (Option<String>, Option<String>),
    /// Something unexpected came up.
    pub off_course: bool,
    /// The expected app is in front but has nothing to say yet: loading.
    pub loading: bool,
}

impl Settling {
    pub fn new(label: String, expect: Option<String>, before_pkg: Option<String>, before_what: Option<String>, passing: Vec<String>, now: Instant) -> Settling {
        let last = (before_pkg.clone(), before_what.clone());
        Settling { label, expect, before_pkg, before_what, passing, since: now, moved: now, arrived: None, last, off_course: false, loading: false }
    }

    /// Take in a snapshot showing `pkg` in front with `what`.
    pub fn observe(&mut self, pkg: Option<&str>, what: Option<&str>, now: Instant) {
        let state = (pkg.map(str::to_string), what.map(str::to_string));
        if state == self.last {
            return;
        }
        self.last = state;
        // an app coming up shows nothing at first: that's not there yet
        self.loading = self.arrived.is_none() && what.is_none() && pkg.is_some() && pkg == self.expect.as_deref();
        let on_the_way = match &self.arrived {
            // arrived: the app it arrived in can go on changing
            Some(at) => at.as_deref() == pkg,
            None => {
                let changed = what != self.before_what.as_deref();
                let arrives = !self.loading
                    && match self.expect.as_deref() {
                        Some(e) => pkg == Some(e) && (self.before_pkg.as_deref() != Some(e) || changed),
                        None => true,
                    };
                if arrives {
                    self.arrived = Some(pkg.map(str::to_string));
                }
                let passing = pkg.is_none_or(|p| self.passing.iter().any(|x| x == p));
                arrives || self.loading || passing || pkg == self.before_pkg.as_deref()
            }
        };
        if on_the_way {
            self.moved = now;
        } else {
            self.off_course = true;
        }
    }

    /// Whether to stop showing the label and show what's really on.
    pub fn done(&self, now: Instant) -> bool {
        let quiet = now.duration_since(self.moved);
        let wait = match (&self.arrived, self.loading) {
            (Some(_), _) => SETTLE_STEADY,
            // the app is coming up: however long it takes, up to SETTLE_MAX
            (None, true) => SETTLE_MAX,
            (None, false) => SETTLE,
        };
        self.off_course || now.duration_since(self.since) >= SETTLE_MAX || quiet >= wait
    }
}

/// Bracketed paste on for as long as this lives, so a pasted link arrives
/// as one piece rather than as key presses (a newline would press Enter).
struct BracketedPaste;

impl BracketedPaste {
    fn on() -> BracketedPaste {
        let _ = ratatui::crossterm::execute!(std::io::stdout(), ratatui::crossterm::event::EnableBracketedPaste);
        BracketedPaste
    }
}

impl Drop for BracketedPaste {
    fn drop(&mut self) {
        let _ = ratatui::crossterm::execute!(std::io::stdout(), ratatui::crossterm::event::DisableBracketedPaste);
    }
}

/// Mouse reporting on for as long as this lives (also when unwinding).
struct MouseCapture;

impl MouseCapture {
    fn on() -> MouseCapture {
        let _ = ratatui::crossterm::execute!(std::io::stdout(), ratatui::crossterm::event::EnableMouseCapture);
        MouseCapture
    }
}

impl Drop for MouseCapture {
    fn drop(&mut self) {
        let _ = ratatui::crossterm::execute!(std::io::stdout(), ratatui::crossterm::event::DisableMouseCapture);
    }
}

fn event_loop(term: &mut DefaultTerminal, app: &mut App, actions: &Sender<Action>, msgs: &Receiver<Msg>) -> std::io::Result<()> {
    use ratatui::crossterm::event::{MouseButton, MouseEventKind};
    // redraw when something changed (a key, news from the TV), and at least
    // every TICK so the progress bar moves
    const TICK: Duration = Duration::from_millis(200);
    let mut drawn: Option<Instant> = None;
    let mut dirty = true;
    loop {
        for m in msgs.try_iter() {
            app.handle(m);
            dirty = true;
        }
        if dirty || app.animating() || drawn.is_none_or(|d| d.elapsed() >= TICK) {
            term.draw(|f| app.draw(f))?;
            drawn = Some(Instant::now());
            dirty = false;
        }
        // drawing the details panel can find a description it needs
        if let Some((ch, start)) = app.want_info.take() {
            let _ = actions.send(Action::LoadInfo(ch, start));
        }
        // a short wait, so news from the TV shows at once (keys wake it anyway);
        // shorter still while the guide slides
        let wait = if app.animating() { Duration::from_millis(10) } else { Duration::from_millis(25) };
        if !event::poll(wait)? {
            continue;
        }
        dirty = true;
        match event::read()? {
            Event::Key(k) if k.kind != KeyEventKind::Release => {
                if let Overlay::AppActions { tab, sel, at } = app.overlay {
                    app.choice_key(tab, sel, at, k.code, actions);
                } else if let Overlay::Options(i) = app.overlay {
                    app.options_key(i, k.code);
                } else if let Overlay::Guide { tab, at } = app.overlay {
                    app.guide_key(tab, at, k.code, k.modifiers, actions);
                } else if let Overlay::Search(at) = app.overlay {
                    let mut text = std::mem::take(&mut app.prompt);
                    let (next, action) = search_key(at, k.code, k.modifiers, &mut text, &app.search_targets);
                    app.prompt = text;
                    app.apply_overlay(next, action, actions);
                } else if let (Overlay::Apps { .. }, KeyCode::Char('#')) = (app.overlay, k.code) {
                    // package names in the launcher, or not (remembered)
                    let on = !crate::options::get(crate::options::SHOW_PACKAGES);
                    app.note = match crate::options::set(crate::options::SHOW_PACKAGES, on) {
                        Ok(()) => if on { "showing package names" } else { "package names hidden" }.into(),
                        Err(e) => e.to_string(),
                    };
                } else if let (Overlay::Apps { tab, sel }, KeyCode::Char('/')) = (app.overlay, k.code) {
                    // search the highlighted app
                    let rows = tab_list(app.apps.as_deref().unwrap_or_default(), tab);
                    let pkg = rows.get(sel).map(|e| e.package.clone());
                    app.open_search(pkg);
                } else if let Overlay::Ambilight { row, col } = app.overlay {
                    app.ambi_key(row, col, k.code, actions);
                } else if let Overlay::AmbiColour { col } = app.overlay {
                    let lights_row = app.ambi_rows().iter().position(|r| r.name == "lights").unwrap_or(0);
                    match k.code {
                        KeyCode::Esc => app.overlay = Overlay::Ambilight { row: lights_row, col },
                        KeyCode::Enter => match crate::plugins::ambilight::parse_colour(&app.prompt) {
                            Some(rgb) => {
                                let change = match col.checked_sub(1).and_then(|i| app.ambi_lights().get(i).cloned()) {
                                    Some(l) => crate::plugins::ambilight::Change::One(l.side, l.n, rgb),
                                    None => crate::plugins::ambilight::Change::All(rgb),
                                };
                                let _ = actions.send(Action::AmbilightChange(change));
                                app.note = "ambilight: setting the colour...".into();
                                app.overlay = Overlay::Ambilight { row: lights_row, col };
                            }
                            None => app.note = format!("'{}' isn't a colour tveee knows (red, #ff8800, 255,136,0)", app.prompt),
                        },
                        KeyCode::Backspace => {
                            app.prompt.pop();
                        }
                        KeyCode::Char(c) if !k.modifiers.contains(KeyModifiers::CONTROL) => app.prompt.push(c),
                        _ => {}
                    }
                } else if let Overlay::Pin = app.overlay {
                    match k.code {
                        KeyCode::Esc => {
                            app.overlay = Overlay::None;
                            app.note = "pairing cancelled".into();
                        }
                        KeyCode::Enter if !app.prompt.is_empty() => {
                            app.overlay = Overlay::None;
                            app.note = "pairing...".into();
                            let _ = actions.send(Action::PairFinish(std::mem::take(&mut app.prompt)));
                        }
                        KeyCode::Backspace => {
                            app.prompt.pop();
                        }
                        KeyCode::Char(c) if c.is_ascii_digit() && app.prompt.len() < 8 => app.prompt.push(c),
                        _ => {}
                    }
                } else if let Overlay::Prompt(item) = app.overlay {
                    let mut text = std::mem::take(&mut app.prompt);
                    let (next, action) = prompt_key(item, k.code, k.modifiers, &mut text);
                    app.prompt = text;
                    app.apply_overlay(next, action, actions);
                } else if let Overlay::Apps { tab, sel } = app.overlay {
                    let rows = tab_list(app.apps.as_deref().unwrap_or_default(), tab);
                    let (next, action) = apps_key(tab, sel, k.code, &rows);
                    app.apply_overlay(next, action, actions);
                } else if app.overlay != Overlay::None {
                    let (next, action) = overlay_key(app.overlay, k.code, &app.menu);
                    app.apply_overlay(next, action, actions);
                } else if app.apply(input_for(k.code, k.modifiers, app.mode), actions) {
                    return Ok(());
                }
            }
            Event::Paste(p) => {
                match app.overlay {
                    Overlay::Prompt(_) | Overlay::Search(_) => app.prompt.push_str(&paste_text(&p)),
                    // remote mode: type it into the TV (what `input text` can)
                    Overlay::None if app.mode == Mode::Remote => {
                        let t: String = paste_text(&p).chars().filter(|c| c.is_ascii() && !c.is_ascii_control()).collect();
                        if !t.is_empty() {
                            let _ = actions.send(Action::Text(t));
                        }
                    }
                    _ => {}
                }
            }
            Event::Mouse(m) => match m.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    if app.click(m.column, m.row, actions) {
                        return Ok(());
                    }
                }
                MouseEventKind::Down(MouseButton::Right) => {
                    if let (Overlay::Apps { tab, .. }, Some(Hit::AppItem(i))) = (app.overlay, app.hit_at(m.column, m.row)) {
                        app.overlay = Overlay::AppActions { tab, sel: i, at: 0 };
                    }
                }
                MouseEventKind::ScrollUp => app.scroll(true, actions),
                MouseEventKind::ScrollDown => app.scroll(false, actions),
                _ => {}
            },
            _ => {}
        }
    }
}

fn ui(c: &Ctx, args: &[String]) -> Result<()> {
    let (mut mouse, mut settle) = (true, true);
    for a in args {
        match a.as_str() {
            "--no-mouse" => mouse = false,
            "--no-settle" => settle = false,
            _ => return crate::error::usage(format!("usage: tveee ui [--no-mouse] [--no-settle]  (unexpected '{a}')")),
        }
    }
    use std::io::IsTerminal;
    if !std::io::stdout().is_terminal() {
        return crate::error::usage("ui needs a terminal");
    }
    let (act_tx, act_rx) = mpsc::channel();
    let (msg_tx, msg_rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let shared = Arc::new(Shared::new(c.tv.ip.clone(), Arc::clone(&stop)));
    // asked every second: keep the apps' devtools connections open
    devtools::keep_open(true);
    // the lanes (see `route`): keys, actions, reads, background, and the refresh
    let (key_tx, key_rx) = mpsc::channel();
    let (do_tx, do_rx) = mpsc::channel();
    let (read_tx, read_rx) = mpsc::channel();
    let (idle_tx, idle_rx) = mpsc::channel();
    let (wake_tx, wake_rx) = mpsc::channel();
    let push_wake = wake_tx.clone();
    let lanes = vec![(Lane::Keys, key_tx), (Lane::Act, do_tx), (Lane::Read, read_tx.clone()), (Lane::Idle, idle_tx)];
    let sh = Arc::clone(&shared);
    spawn_named("route", move || route(&act_rx, &sh, &lanes));
    let (key_msgs, key_tv) = (msg_tx.clone(), shared.tv());
    spawn_named("keys", move || send_keys(key_tv, &key_rx, &key_msgs));
    let actor = Actor { tv: shared.tv(), shared: Arc::clone(&shared), menu: c.menu.clone(), wake: wake_tx, reads: read_tx, pairing: None };
    let actor_msgs = msg_tx.clone();
    spawn_named("actions", move || actor.run(&do_rx, actor_msgs));
    let (read_msgs, sh) = (msg_tx.clone(), Arc::clone(&shared));
    spawn_named("reads", move || reads(&sh, &read_rx, &read_msgs));
    let (idle_msgs, sh) = (msg_tx.clone(), Arc::clone(&shared));
    spawn_named("background", move || background(&sh, &idle_rx, idle_msgs));
    let (preview_msgs, sh) = (msg_tx.clone(), Arc::clone(&shared));
    spawn_named("preview", move || preview_lane(&sh, &preview_msgs));
    let sh = Arc::clone(&shared);
    spawn_named("push", move || push_lane(&sh, &push_wake));
    let refresher = Refresher { tv: shared.tv(), shared: Arc::clone(&shared), channel: None, named: None, last_window: String::new() };
    spawn_named("refresh", move || refresher.run(&wake_rx, msg_tx));
    let _ = act_tx.send(Action::Refresh);
    let seen = last::load(&c.tv.ip);
    let mut app = App {
        ip: c.tv.ip.clone(),
        snap: seen.as_ref().map(last::seed),
        seeded: seen.is_some(),
        kept: seen.clone(),
        at: Instant::now(),
        note: String::new(),
        error: None,
        mode: Mode::Player,
        overlay: Overlay::None,
        menu: c.menu.clone(),
        hits: Vec::new(),
        info: false,
        mem_history: Vec::new(),
        apps: None,
        apps_tab: AppTab::Known,
        prompt: String::new(),
        running: std::collections::BTreeSet::new(),
        fixed: std::collections::BTreeSet::new(),
        searchable: None,
        search_targets: Vec::new(),
        settling: None,
        settle,
        ambi_supported: false,
        ambi_open: false,
        // greyed: it's from before
        last_volume: seen.as_ref().and_then(|l| Some((l.volume?, Instant::now().checked_sub(VOLUME_STALE * 2)?))),
        preview_open: false,
        preview: None,
        preview_source: "",
        preview_note: String::new(),
        picker: None,
        guide: None,
        guide_loading: false,
        guide_later: 0,
        guide_view: 0,
        guide_slid: Instant::now(),
        details: Details::Closed,
        details_scroll: 0,
        details_of: None,
        infos: std::collections::HashMap::new(),
        infos_asked: std::collections::HashSet::new(),
        want_info: None,
    };
    // ratatui::run sets up the terminal, restores it afterwards (and on panic)
    // how this terminal shows pictures (sixel, kitty, ...; else half-blocks),
    // asked before ratatui takes the terminal (the query turns raw mode on and
    // off). Only asked of terminals known to draw pictures: one that doesn't
    // answer everything leaves the query reading the keyboard, eating keys.
    let ask = picture_terminal(|k| std::env::var(k).ok());
    app.picker = Some(if ask {
        ratatui_image::picker::Picker::from_query_stdio().unwrap_or_else(|_| ratatui_image::picker::Picker::halfblocks())
    } else {
        ratatui_image::picker::Picker::halfblocks()
    });
    let result = ratatui::run(|term| {
        let _mouse = mouse.then(MouseCapture::on);
        let _paste = BracketedPaste::on();
        event_loop(term, &mut app, &act_tx, &msg_rx)
    });
    // stop the worker and give it a moment to remove its port forward (it may
    // be mid-refresh); don't wait forever on a TV that is slow to answer
    stop.store(true, Ordering::Relaxed);
    drop(act_tx);
    let deadline = Instant::now() + Duration::from_secs(3);
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        if let Err(RecvTimeoutError::Disconnected) = msg_rx.recv_timeout(left) {
            break;
        }
    }
    // and close the kept connection (its port forward)
    devtools::keep_open(false);
    Ok(result?)
}

pub fn commands() -> Vec<Command> {
    vec![Command::new(CAT, "ui", "[--no-mouse] [--no-settle]", "interactive remote: what's on, volume, seeking", ui).long(
        "A full-screen remote showing what is playing (as 'tveee nowplaying') and the\n\
         volume, refreshed every second. Two modes, switched with Tab: in player mode\n\
         the arrows seek and change volume; in remote mode the keyboard is the TV's:\n\
         arrows move around its menus, Enter is OK, Esc is back, and what you type\n\
         (or paste) is typed into the TV. Tab switches back; Ctrl-C quits.\n\
         \n\
         \x20 space / p     play/pause   Backspace  back   h  home\n\
         \x20 left/right    seek 10s     [ / ]  seek 60s     0-9  jump to 0%-90%\n\
         \x20 up/down + -   volume       m      mute/unmute\n\
         \x20 r  refresh    .  more: power, mirror, screenshot, inputs (risky ones ask first)\n\
         \x20 i  system panel: memory graph, swap, storage, load, uptime\n\
         \x20 l  Ambilight panel (Philips TVs that have it): state and each light's\n\
         \x20    colour; L (or clicking it) changes power, style, Hue and any light's\n\
         \x20    colour; P pairs with the TV if needed\n\
         \x20 v  preview: a picture of what's on, refreshed every second or so (with\n\
         \x20    scrcpy and ffmpeg installed: scrcpy at 320 pixels, which sees video too;\n\
         \x20    YouTube's storyboard frames while it starts or without it; otherwise a\n\
         \x20    screenshot every few seconds, where video comes out black)\n\
         \x20 a  launcher: the installed apps tveee has aliases for (see 'tveee apps'); ● marks\n\
         \x20    running ones (Enter brings them to the front), ⊘ ones Android keeps running\n\
         \x20    (live TV: they can't be closed); Space or right-click opens actions:\n\
         \x20    restart, close, search\n\
         \x20 /  search the app in front, or (in the launcher) the highlighted app\n\
         \x20 ?  all keys   q / Esc / Ctrl-C  quit\n\
         \n\
         Seeking is exact for apps tveee can read the video of (YouTube); elsewhere\n\
         left/right send fast-forward/rewind key presses (~10s each, apps differ)\n\
         and number keys do nothing.\n\
         \n\
         The mouse works too: click the buttons along the bottom, the progress bar\n\
         (jump there) or the volume bar (set that level); the wheel changes the\n\
         volume. Hold Shift to select text as usual. --no-mouse turns this off.\n\
         \n\
         While an action is under way (launching, searching, switching channel) the\n\
         what line says so (\"launching Netflix...\") instead of showing each step\n\
         the TV passes through, until the action has arrived and settled.\n\
         --no-settle shows every step as it comes (for debugging).",
    )]
}

impl App {
    fn handle(&mut self, m: Msg) {
        match m {
            Msg::Snap(s, at) => {
                if let Some(m) = s.system.mem {
                    self.mem_history.push(m.used() / 1024);
                    if self.mem_history.len() > MEM_HISTORY {
                        self.mem_history.remove(0);
                    }
                }
                // the latest snapshot is always kept; while an action
                // settles it's only shown once the action has got there
                let headline = s.activity.as_ref().map(Activity::headline);
                if let Some(st) = &mut self.settling {
                    st.observe(front_pkg(&s).as_deref(), headline.as_deref(), Instant::now());
                }
                if let Some(v) = s.volume {
                    self.last_volume = Some((v, Instant::now()));
                }
                // kept for the next run's first frame, when what's on changes
                if let Some(l) = last::of(&s) {
                    let same = self.kept.as_ref().is_some_and(|k| (&k.what, &k.title, k.volume) == (&l.what, &l.title, l.volume));
                    if !same {
                        last::save(&self.ip, &l);
                        self.kept = Some(l);
                    }
                }
                (self.snap, self.at, self.error, self.seeded) = (Some(*s), at, None, false);
            }
            Msg::Note(n) => self.note = n,
            Msg::Failed(e) => {
                crate::tlog!("failed: {e}");
                // the action didn't happen: show what's really on
                self.settling = None;
                self.error = Some(e);
            }
            Msg::Apps(list) => self.apps = Some(list),
            Msg::Searchable(list) => self.searchable = Some(list),
            Msg::Running(set, fixed) => (self.running, self.fixed) = (set, fixed),
            Msg::Ambilight(has) => self.ambi_supported = has,
            Msg::Preview(p) => {
                let picker = self.picker.get_or_insert_with(ratatui_image::picker::Picker::halfblocks);
                if let Some(img) = image::RgbImage::from_raw(p.width, p.height, p.rgb) {
                    self.preview = Some(picker.new_resize_protocol(image::DynamicImage::ImageRgb8(img)));
                    self.preview_source = p.source;

                }
            }
            Msg::PreviewNote(n) => {
                if !n.is_empty() && n != self.preview_note {
                    crate::tlog!("preview: {n}");
                }
                self.preview_note = n;
            }
            Msg::AskPin => {
                self.prompt.clear();
                self.overlay = Overlay::Pin;
            }
            Msg::Info(key, info) => {
                self.infos_asked.remove(&key);
                let info = info.unwrap_or_else(|e| Some(format!("(couldn't read the description: {e})")));
                self.infos.insert(key, info);
            }
            Msg::Guide(g) => {
                if let Some(news) = g.as_ref().ok().and_then(|g| g.news.clone()) {
                    self.note = news;
                }
                // nothing picked yet: start on the channel that's on
                if let (Overlay::Guide { tab, at: 0 }, Ok(g)) = (self.overlay, &g) {
                    let first = g.rows.first().map_or(0, |r| r.channel.id);
                    self.overlay = Overlay::Guide { tab, at: g.current.unwrap_or(first) };
                }
                self.guide = Some(g);
                self.guide_loading = false;
            }
            Msg::Seeked(pos, at) => {
                if let Some(p) = self.snap.as_mut().and_then(|s| s.activity.as_mut()).and_then(|a| a.progress.as_mut()) {
                    p.position = pos;
                    self.at = at;
                }
            }
        }
    }

    /// Show `label` in place of the what line until the action lands (with
    /// `expect` in front, if given) or it's had time to.
    fn settle(&mut self, label: String, expect: Option<String>) {
        if !self.settle {
            return;
        }
        let snap = self.snap.as_ref();
        // home screens on the way: the ones tveee knows, and those on the TV
        let mut passing: Vec<String> = self
            .apps
            .iter()
            .flatten()
            .filter(|e| e.section == SECTIONS[0] || apps::passes_through(&e.package))
            .map(|e| e.package.clone())
            .collect();
        passing.extend(["android", "com.android.systemui"].map(String::from));
        self.settling = Some(Settling::new(
            label,
            expect,
            snap.and_then(front_pkg),
            snap.and_then(|s| s.activity.as_ref()).map(Activity::headline),
            passing,
            Instant::now(),
        ));
    }

    /// The settling label to show now, if an action is still settling.
    fn settling_label(&mut self) -> Option<String> {
        if self.settling.as_ref().is_some_and(|s| s.done(Instant::now())) {
            self.settling = None;
        }
        self.settling.as_ref().map(|s| s.label.clone())
    }

    /// Run what a clicked field opens.
    fn open(&mut self, o: nowplaying::Open, actions: &Sender<Action>) {
        match o {
            nowplaying::Open::Guide { channel } => self.open_guide(channel, actions),
        }
    }

    /// Act on a key or button. Returns true to quit.
    fn apply(&mut self, input: Input, actions: &Sender<Action>) -> bool {
        match input {
            Input::Quit => return true,
            Input::ToggleMode => {
                self.mode = if self.mode == Mode::Player { Mode::Remote } else { Mode::Player };
                self.note = match self.mode {
                    Mode::Remote => "remote mode: keys go to the TV (typing too); Tab for player mode".into(),
                    Mode::Player => "player mode: arrows seek and change volume".into(),
                };
            }
            Input::ToggleHelp => self.overlay = Overlay::Help,
            Input::ToggleInfo => self.info = !self.info,
            Input::ToggleAmbilight if !self.ambi_supported => {
                self.note = "no Ambilight here that tveee can control".into();
            }
            Input::ToggleAmbilight => {
                self.ambi_open = !self.ambi_open;
                let _ = actions.send(Action::AmbilightPanel(self.ambi_open));
            }
            Input::TogglePreview => {
                self.preview_open = !self.preview_open;
                if !self.preview_open {
                    (self.preview, self.preview_source) = (None, "");
                    self.preview_note.clear();
                }
                let _ = actions.send(Action::PreviewPanel(self.preview_open));
            }
            Input::AmbilightDialog if !self.ambi_supported => {
                self.note = "no Ambilight here that tveee can control".into();
            }
            Input::AmbilightDialog => {
                // the panel's reads feed the dialog
                if !self.ambi_open {
                    self.ambi_open = true;
                    let _ = actions.send(Action::AmbilightPanel(true));
                }
                let col = self.ambi_rows().first().and_then(|r| r.current).unwrap_or(0);
                self.overlay = Overlay::Ambilight { row: 0, col };
            }
            Input::Pair => {
                self.note = "asking the TV to pair...".into();
                let _ = actions.send(Action::PairStart);
            }
            Input::OpenSearch => self.open_search(None),
            Input::OpenGuide => self.open_guide(None, actions),
            Input::OpenOptions => self.overlay = Overlay::Options(0),
            Input::OpenMenu if !self.menu.is_empty() => {
                // in standby, start on "wake"
                let standby = self.snap.as_ref().is_some_and(|s| s.standby);
                let wake = self.menu.iter().position(|m| m.label == "wake").filter(|_| standby);
                self.overlay = Overlay::Menu(wake.unwrap_or(0));
            }
            Input::OpenMenu => self.note = "no menu items in this build".into(),
            Input::OpenApps => {
                // start on the app that's in front, if it's in the list
                let pkg = self.snap.as_ref().and_then(|s| s.focus.as_deref()).map(|f| f.split('/').next().unwrap_or(f));
                let tab = self.apps_tab;
                let rows = tab_list(self.apps.as_deref().unwrap_or_default(), tab);
                let now = rows.iter().position(|a| Some(a.package.as_str()) == pkg);
                self.overlay = Overlay::Apps { tab, sel: now.unwrap_or(0) };
                let _ = actions.send(Action::CheckRunning);
            }
            // the last refresh knows whether it's muted: just press the key
            Input::Do(Action::Mute) => {
                let known = self.snap.as_ref().and_then(|s| s.muted);
                let _ = actions.send(known.map_or(Action::Mute, Action::MuteKnown));
            }
            Input::Do(a) => {
                let _ = actions.send(a);
            }
            Input::Nothing => {}
        }
        false
    }

    /// Move to the overlay `next`, running `action` if there is one.
    fn apply_overlay(&mut self, next: Overlay, action: Option<Action>, actions: &Sender<Action>) {
        let was = self.overlay;
        self.overlay = next;
        if matches!(next, Overlay::Prompt(_)) && !matches!(was, Overlay::Prompt(_)) {
            self.prompt.clear();
        }
        if let Overlay::Apps { tab, .. } = next {
            self.apps_tab = tab;
        }
        match (was, next, action) {
            (_, _, Some(a @ Action::Menu(i))) => {
                self.note = format!("{}...", self.menu.get(i).map_or("", |m| m.label));
                let _ = actions.send(a);
            }
            (_, _, Some(a @ Action::MenuText(i, _))) => {
                // e.g. "open a link": something new should come up
                self.note = format!("{}...", self.menu.get(i).map_or("", |m| m.label));
                self.settle(self.note.clone(), None);
                let _ = actions.send(a);
            }
            (Overlay::Prompt(_) | Overlay::Search(_), Overlay::None, None) => self.note = "cancelled".into(),
            (_, _, Some(Action::Search { pkg, query, keys })) => {
                let name = apps::by_package(&pkg).map_or_else(|| pkg.clone(), |a| list_name(a.name));
                self.note = if keys { format!("typing \"{query}\" into {name}...") } else { format!("searching {name}...") };
                self.settle(self.note.clone(), Some(pkg.clone()));
                let _ = actions.send(Action::Search { pkg, query, keys });
            }
            (_, _, Some(Action::Launch(target))) => {
                // already running: resume it rather than start it again
                let pkg = apps::pkg_of(&target).to_string();
                if self.running.contains(&pkg) && target != "home" {
                    self.note = format!("bringing {} to the front...", app_name(&pkg));
                    self.settle(self.note.clone(), Some(pkg));
                    let _ = actions.send(Action::Front(target));
                } else {
                    self.note = format!("launching {}...", app_name(&pkg));
                    // "home" is whichever home app is set
                    self.settle(self.note.clone(), (target != "home").then_some(pkg));
                    let _ = actions.send(Action::Launch(target));
                }
            }
            (_, _, Some(a @ (Action::Front(_) | Action::Close(_) | Action::Restart { .. }))) => {
                self.note = match &a {
                    Action::Close(p) => format!("closing {}...", app_name(p)),
                    Action::Restart { pkg, .. } => format!("restarting {}...", app_name(pkg)),
                    _ => "bringing it to the front...".into(),
                };
                let expect = match &a {
                    Action::Restart { pkg, .. } => Some(pkg.clone()),
                    Action::Front(t) => Some(apps::pkg_of(t).to_string()),
                    // closing: whatever comes up next
                    _ => None,
                };
                self.settle(self.note.clone(), expect);
                let _ = actions.send(a);
            }
            (Overlay::Confirm { .. }, Overlay::None, None) => self.note = "cancelled".into(),
            _ => {}
        }
    }

    fn hit_at(&self, x: u16, y: u16) -> Option<Hit> {
        let pos = ratatui::layout::Position { x, y };
        // drawn last = on top
        self.hits.iter().rev().find(|(r, _)| r.contains(pos)).map(|(_, h)| h.clone())
    }

    /// A left click at (x, y). Returns true to quit.
    fn click(&mut self, x: u16, y: u16, actions: &Sender<Action>) -> bool {
        let hit = self.hit_at(x, y);
        match (self.overlay, hit) {
            (Overlay::Menu(_), Some(Hit::MenuItem(i))) => {
                let (next, action) = pick(&self.menu, i);
                self.apply_overlay(next, action, actions);
            }
            (Overlay::Menu(_), Some(Hit::Popup)) => {}
            (Overlay::Confirm { item, .. }, Some(Hit::ConfirmYes)) => {
                self.apply_overlay(Overlay::None, Some(Action::Menu(item)), actions)
            }
            (Overlay::Confirm { .. }, Some(Hit::Popup)) => {}
            (Overlay::Apps { tab, .. }, Some(Hit::AppTab(t))) if t != tab => {
                self.apply_overlay(Overlay::Apps { tab: t, sel: 0 }, None, actions);
            }
            (Overlay::Options(_), Some(Hit::OptionItem(i))) => self.toggle_option(i),
            (Overlay::Options(_), Some(Hit::Popup)) => {}
            (Overlay::Options(_), _) => self.overlay = Overlay::None,
            (Overlay::Guide { .. }, Some(Hit::Details(click))) => {
                self.details = match click {
                    DetailsClick::Open => Details::Open,
                    DetailsClick::Focus => Details::Focused,
                    DetailsClick::Unfocus => Details::Open,
                    DetailsClick::Hide => Details::Closed,
                };
            }
            (Overlay::Guide { .. }, Some(Hit::GuideStar(id))) => self.toggle_favourite(id, actions),
            (Overlay::Guide { .. }, Some(Hit::GuideRow(id))) => {
                if self.details == Details::Focused {
                    self.details = Details::Open;
                }
                self.watch_channel(id, actions);
            }
            (Overlay::Guide { at, .. }, Some(Hit::GuideTab(t))) => self.guide_tab(t, at),
            (Overlay::None, Some(Hit::Open(o))) => self.open(o, actions),
            (Overlay::Guide { .. }, Some(Hit::Popup)) => {}
            // clicking outside closes it
            (Overlay::Guide { .. }, _) => self.overlay = Overlay::None,
            (Overlay::Apps { tab, .. }, Some(Hit::AppItem(i))) => {
                let rows = tab_list(self.apps.as_deref().unwrap_or_default(), tab);
                let target = rows.get(i).map(|e| e.target.clone());
                if let Some(target) = target {
                    self.apply_overlay(Overlay::None, Some(Action::Launch(target)), actions);
                }
            }
            (Overlay::Apps { .. }, Some(Hit::Popup | Hit::AppTab(_))) => {}
            (Overlay::AppActions { tab, sel, .. }, Some(Hit::AppChoice(i))) => self.run_choice(tab, sel, i, actions),
            (Overlay::AppActions { .. }, Some(Hit::Popup)) => {}
            (Overlay::AppActions { tab, sel, .. }, _) => self.overlay = Overlay::Apps { tab, sel },
            (Overlay::Prompt(_) | Overlay::Search(_), Some(Hit::Popup)) => {}
            // a click elsewhere closes the pop-up (cancelling a confirmation)
            (Overlay::Help | Overlay::Menu(_) | Overlay::Confirm { .. } | Overlay::Apps { .. } | Overlay::Prompt(_) | Overlay::Search(_), _) => {
                self.apply_overlay(Overlay::None, None, actions)
            }
            (Overlay::None, Some(Hit::Input(i))) => return self.apply(i, actions),
            (Overlay::None, Some(Hit::Progress { x0, width })) => {
                let _ = actions.send(Action::Jump(bar_fraction(x, x0, width)));
            }
            (Overlay::None, Some(Hit::Volume { x0, width })) => {
                if let Some((v, max)) = self.snap.as_ref().and_then(|s| s.volume) {
                    let target = (bar_fraction(x, x0, width) * f64::from(max)).round() as i32;
                    let _ = actions.send(Action::Volume(target - v as i32));
                }
            }
            (Overlay::None, _) => {}
            (Overlay::Ambilight { .. }, Some(Hit::AmbiOption(row, col))) => self.ambi_pick(row, col, actions),
            (Overlay::Ambilight { .. }, Some(Hit::Popup)) => {}
            (Overlay::Ambilight { .. }, _) => self.overlay = Overlay::None,
            (Overlay::AmbiColour { .. }, Some(Hit::Popup)) => {}
            (Overlay::AmbiColour { col }, _) => {
                let row = self.ambi_rows().iter().position(|r| r.name == "lights").unwrap_or(0);
                self.overlay = Overlay::Ambilight { row, col };
            }
            // the PIN box: a click outside it cancels
            (Overlay::Pin, Some(Hit::Popup)) => {}
            (Overlay::Pin, _) => {
                self.overlay = Overlay::None;
                self.note = "pairing cancelled".into();
            }
        }
        false
    }

    /// Mouse wheel: volume, or the menu highlight while the menu is open.
    fn scroll(&mut self, up: bool, actions: &Sender<Action>) {
        match self.overlay {
            Overlay::Menu(_) => {
                let code = if up { KeyCode::Up } else { KeyCode::Down };
                let (next, action) = overlay_key(self.overlay, code, &self.menu);
                self.apply_overlay(next, action, actions);
            }
            Overlay::Apps { tab, sel } => {
                let rows = tab_list(self.apps.as_deref().unwrap_or_default(), tab);
                let (next, _) = apps_key(tab, sel, if up { KeyCode::Up } else { KeyCode::Down }, &rows);
                self.overlay = next;
            }
            Overlay::Options(i) => self.options_key(i, if up { KeyCode::Up } else { KeyCode::Down }),
            Overlay::Guide { tab, at } => self.guide_key(tab, at, if up { KeyCode::Up } else { KeyCode::Down }, KeyModifiers::NONE, actions),
            Overlay::None => {
                let _ = actions.send(Action::Volume(if up { 1 } else { -1 }));
            }
            _ => {}
        }
    }
}

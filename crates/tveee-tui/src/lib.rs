//! `tveee ui`: an interactive terminal remote (ratatui), a front end on the
//! core's engine. The `tveee` binary has it with its `ui` cargo feature.
//!
//! The screen shows what is on (as `nowplaying`) plus the volume, and keys
//! control playback and volume. All TV traffic happens on a worker thread so
//! the screen never waits on adb; the worker keeps one devtools connection
//! open, which makes refreshing every second and exact seeking cheap.

use tveee_core::error::Result;
use tveee_core::actions::{ActionDescriptor, Params};
use tveee_core::nowplaying::{self, Activity, bar, progress_bar};
use tveee_core::builtin::apps::list_name;
use tveee_core::builtin::{apps, info, search};
use tveee_core::Ctx;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::{Duration, Instant};

mod keymap;
mod guide;
mod preview;
mod launcher;
mod ambi;
mod overlays;
mod palette;
mod drawing;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod snapshots;

use tveee_core::engine::*;
pub use self::keymap::*;
pub use self::guide::*;
pub use self::preview::*;
pub use self::launcher::*;
pub use self::overlays::*;
pub use self::palette::*;
pub use self::drawing::*;

const CAT: &str = "ui";
struct App {
    ip: String,
    /// The engine: where requests go, and what it knows (`st`).
    engine: Handle,
    /// The engine's state, as of this frame.
    st: Arc<TvState>,
    note: String,
    mode: Mode,
    overlay: Overlay,
    /// The `.` menu: the actions with a menu key.
    menu: Vec<ActionDescriptor>,
    /// Every action of the enabled plugins (the palette).
    actions: Vec<ActionDescriptor>,
    /// Where the clickable things were drawn last frame.
    hits: Vec<(ratatui::layout::Rect, Hit)>,
    /// Show the system panel (memory, storage, ...).
    info: bool,
    /// The launcher tab used last.
    apps_tab: AppTab,
    /// What has been typed into the open prompt.
    prompt: String,
    /// The search box's app choices, made when it opens.
    search_targets: Vec<SearchTarget>,
    /// An action still settling: its label stands in for the what line.
    settling: Option<Settling>,
    /// Off (`--no-settle`): show every state the TV reports, as it comes.
    settle: bool,
    /// The preview panel: open or not, its picture (ready to draw), where it
    /// came from, and what it's doing.
    preview_open: bool,
    preview: Option<ratatui_image::protocol::StatefulProtocol>,
    preview_source: &'static str,
    preview_note: String,
    /// How this terminal shows pictures (asked once at the start).
    picker: Option<ratatui_image::picker::Picker>,
    /// Whether the Ambilight panel is open.
    ambi_open: bool,
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
    /// Descriptions asked for; `want_info` is the next to ask for.
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

fn event_loop(term: &mut DefaultTerminal, app: &mut App, events: &Receiver<CoreEvent>) -> std::io::Result<()> {
    use ratatui::crossterm::event::{MouseButton, MouseEventKind};
    // redraw when something changed (a key, news from the TV), and at least
    // every TICK so the progress bar moves
    const TICK: Duration = Duration::from_millis(200);
    let mut drawn: Option<Instant> = None;
    let mut dirty = true;
    loop {
        let actions = &app.engine.clone();
        for e in events.try_iter() {
            app.handle(e);
            dirty = true;
        }
        // what was asked for may have changed it at once (a star, loading)
        app.st = app.engine.state();
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
            // F1-F3 (Alt+1-3): the guide, the launcher, remote mode; from the
            // player or from either list, not while typing into a box
            Event::Key(k) if k.kind != KeyEventKind::Release && app.can_switch_view() && view_key(k.code, k.modifiers).is_some() => {
                if let Some(v) = view_key(k.code, k.modifiers) {
                    app.switch_view(v, actions);
                }
            }
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
                    let on = !tveee_core::options::get(tveee_core::options::SHOW_PACKAGES);
                    app.note = match tveee_core::options::set(tveee_core::options::SHOW_PACKAGES, on) {
                        Ok(()) => if on { "showing package names" } else { "package names hidden" }.into(),
                        Err(e) => e.to_string(),
                    };
                } else if let (Overlay::Apps { tab, sel }, KeyCode::Char('/')) = (app.overlay, k.code) {
                    // search the highlighted app
                    let rows = tab_list(app.st.apps_list(), tab);
                    let pkg = rows.get(sel).map(|e| e.package.clone());
                    app.open_search(pkg);
                } else if let Overlay::Ambilight { row, col } = app.overlay {
                    app.ambi_key(row, col, k.code, actions);
                } else if let Overlay::AmbiColour { row, col } = app.overlay {
                    app.ambi_ask_key(row, col, k.code, k.modifiers, actions);
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
                } else if let Overlay::Palette(sel) = app.overlay {
                    app.palette_key(sel, k.code, k.modifiers, actions);
                } else if let Overlay::Prompt(id) = app.overlay {
                    let mut text = std::mem::take(&mut app.prompt);
                    let (next, action) = prompt_key(id, k.code, k.modifiers, &mut text);
                    app.prompt = text;
                    app.apply_overlay(next, action, actions);
                } else if let Overlay::Apps { tab, sel } = app.overlay {
                    let rows = tab_list(app.st.apps_list(), tab);
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
                    Overlay::Prompt(_) | Overlay::Search(_) | Overlay::Palette(_) => app.prompt.push_str(&paste_text(&p)),
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
            _ => return tveee_core::error::usage(format!("usage: tveee ui [--no-mouse] [--no-settle]  (unexpected '{a}')")),
        }
    }
    use std::io::IsTerminal;
    if !std::io::stdout().is_terminal() {
        return tveee_core::error::usage("ui needs a terminal");
    }
    let engine = Engine::start(&c.tv.ip, c.actions.clone());
    let mut app = App::new(c.tv.ip.clone(), engine.handle.clone(), c.actions.clone(), settle);
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
        event_loop(term, &mut app, &engine.events)
    });
    engine.stop();
    Ok(result?)
}

pub fn actions() -> Vec<ActionDescriptor> {
    vec![ActionDescriptor::terminal("ui.ui", CAT, "interactive remote: what's on, volume, seeking", ui).cli("ui", "[--no-mouse] [--no-settle]", "interactive remote: what's on, volume, seeking").long(
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
         \x20 :  commands (or Ctrl-P): everything tveee can do, found by typing a\n\
         \x20    few letters of it\n\
         \x20 F  free memory   S  sleep   R  restart the app in front, where it was\n\
         \x20 F1 F2 F3  the guide, the apps, remote mode (Alt+1-3 too), from the\n\
         \x20    player or from the guide or the apps\n\
         \x20 The top line: the TV's load and free memory, [F]ree, [l] the Ambilight,\n\
         \x20 [:] commands (click them too). A small pane keeps one row of keys.\n\
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
    /// A screen on `engine`'s state, nothing open yet.
    fn new(ip: String, engine: Handle, actions: Vec<ActionDescriptor>, settle: bool) -> App {
        App {
            ip,
            st: engine.state(),
            engine,
            note: String::new(),
            mode: Mode::Player,
            overlay: Overlay::None,
            menu: actions.iter().filter(|a| a.key.is_some()).copied().collect(),
            actions,
            hits: Vec::new(),
            info: false,
            apps_tab: AppTab::Known,
            prompt: String::new(),
            search_targets: Vec::new(),
            settling: None,
            settle,
            ambi_open: false,
            preview_open: false,
            preview: None,
            preview_source: "",
            preview_note: String::new(),
            picker: None,
            guide_later: 0,
            guide_view: 0,
            guide_slid: Instant::now(),
            details: Details::Closed,
            details_scroll: 0,
            details_of: None,
            infos_asked: std::collections::HashSet::new(),
            want_info: None,
        }
    }
}

impl App {
    fn handle(&mut self, e: CoreEvent) {
        match e {
            CoreEvent::Changed(change) => {
                self.st = self.engine.state();
                match change {
                    // while an action settles the latest snapshot is only
                    // shown once the action has got there
                    Change::Snapshot => {
                        let snap = self.st.snap.as_ref();
                        let headline = snap.and_then(|s| s.activity.as_ref()).map(Activity::headline);
                        if let Some(st) = &mut self.settling {
                            st.observe(snap.and_then(front_pkg).as_deref(), headline.as_deref(), Instant::now());
                        }
                    }
                    // nothing picked yet: start on the channel that's on
                    Change::Guide => {
                        if let (Overlay::Guide { tab, at: 0 }, Some(g)) = (self.overlay, self.st.guide_ok()) {
                            let first = g.rows.first().map_or(0, |r| r.channel.id);
                            self.overlay = Overlay::Guide { tab, at: g.current.unwrap_or(first) };
                        }
                    }
                    _ => {}
                }
            }
            CoreEvent::Note(n) => self.note = n,
            // the action didn't happen: show what's really on
            CoreEvent::Failed => self.settling = None,
            CoreEvent::Preview(p) => {
                let picker = self.picker.get_or_insert_with(ratatui_image::picker::Picker::halfblocks);
                if let Some(img) = image::RgbImage::from_raw(p.width, p.height, p.rgb) {
                    self.preview = Some(picker.new_resize_protocol(image::DynamicImage::ImageRgb8(img)));
                    self.preview_source = p.source;
                }
            }
            CoreEvent::PreviewNote(n) => {
                if !n.is_empty() && n != self.preview_note {
                    tveee_core::tlog!("preview: {n}");
                }
                self.preview_note = n;
            }
            CoreEvent::AskPin => {
                self.prompt.clear();
                self.overlay = Overlay::Pin;
            }
        }
    }

    /// Show `label` in place of the what line until the action lands (with
    /// `expect` in front, if given) or it's had time to.
    fn settle(&mut self, label: String, expect: Option<String>) {
        if !self.settle {
            return;
        }
        let snap = self.st.snap.as_ref();
        // home screens on the way: the ones tveee knows, and those on the TV
        let mut passing: Vec<String> = self
            .st
            .apps_list()
            .iter()
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
    fn open(&mut self, o: nowplaying::Open, actions: &Handle) {
        match o {
            nowplaying::Open::Guide { channel } => self.open_guide(channel, actions),
        }
    }

    /// Act on a key or button. Returns true to quit.
    fn apply(&mut self, input: Input, actions: &Handle) -> bool {
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
            Input::ToggleAmbilight if self.panel_id().is_none() => {
                self.note = "no Ambilight here that tveee can control".into();
            }
            Input::ToggleAmbilight => {
                self.ambi_open = !self.ambi_open;
                if let Some(id) = self.panel_id() {
                    let _ = actions.send(Action::WatchPanel(id, self.ambi_open));
                }
            }
            Input::TogglePreview => {
                self.preview_open = !self.preview_open;
                if !self.preview_open {
                    (self.preview, self.preview_source) = (None, "");
                    self.preview_note.clear();
                }
                let _ = actions.send(Action::PreviewPanel(self.preview_open));
            }
            Input::AmbilightDialog if self.panel_id().is_none() => {
                self.note = "no Ambilight here that tveee can control".into();
            }
            Input::AmbilightDialog => {
                // the panel's reads feed the dialog
                if let (false, Some(id)) = (self.ambi_open, self.panel_id()) {
                    self.ambi_open = true;
                    let _ = actions.send(Action::WatchPanel(id, true));
                }
                let col = self.ambi_rows().first().and_then(|r| r.current).unwrap_or(0);
                self.overlay = Overlay::Ambilight { row: 0, col };
            }
            // the pairing the panel says it needs
            Input::Pair => match self.panel_view() {
                Some(&tveee_core::panel::View::NeedsPairing { pairing, .. }) => {
                    self.note = "asking the TV to pair...".into();
                    let _ = actions.send(Action::PairStart(pairing));
                }
                _ => self.note = "nothing here needs pairing (open the panel with l to see)".into(),
            },
            Input::OpenSearch => self.open_search(None),
            Input::OpenGuide => self.open_guide(None, actions),
            Input::OpenOptions => self.overlay = Overlay::Options(0),
            Input::OpenMenu if !self.menu.is_empty() => {
                // in standby, start on "wake"
                let standby = self.st.snap.as_ref().is_some_and(|s| s.standby);
                let wake = self.menu.iter().position(|m| m.id == "power.wake").filter(|_| standby);
                self.overlay = Overlay::Menu(wake.unwrap_or(0));
            }
            Input::OpenMenu => self.note = "no menu items in this build".into(),
            Input::OpenPalette => self.open_palette(),
            Input::RestartFront => match self.st.snap.as_ref().and_then(front_pkg) {
                Some(pkg) if !apps::passes_through(&pkg) => {
                    self.note = format!("restarting {}...", app_name(&pkg));
                    self.settle(self.note.clone(), Some(pkg.clone()));
                    let _ = actions.send(Action::Restart { target: pkg.clone(), pkg });
                }
                _ => self.note = "nothing in front to restart".into(),
            },
            // free and sleep: what they're doing, till they say
            Input::Do(Action::Run { id, args }) => {
                self.note = format!("{}...", tveee_core::actions::find(&self.actions, id).map_or(id, |a| a.title));
                let _ = actions.dispatch(id, args);
            }
            Input::OpenApps => {
                // start on the app that's in front, if it's in the list
                let pkg = self.st.snap.as_ref().and_then(|s| s.focus.as_deref()).map(|f| f.split('/').next().unwrap_or(f));
                let tab = self.apps_tab;
                let rows = tab_list(self.st.apps_list(), tab);
                let now = rows.iter().position(|a| Some(a.package.as_str()) == pkg);
                self.overlay = Overlay::Apps { tab, sel: now.unwrap_or(0) };
                let _ = actions.send(Action::CheckRunning);
            }
            // the last refresh knows whether it's muted: just press the key
            Input::Do(Action::Mute) => {
                let known = self.st.snap.as_ref().and_then(|s| s.muted);
                let _ = actions.send(known.map_or(Action::Mute, Action::MuteKnown));
            }
            Input::Do(a) => {
                let _ = actions.send(a);
            }
            Input::Nothing => {}
        }
        false
    }

    /// Whether F1-F3 switch view now: in the player, remote mode, or the
    /// guide or launcher (not a box being typed into, or a question).
    fn can_switch_view(&self) -> bool {
        matches!(self.overlay, Overlay::None | Overlay::Guide { .. } | Overlay::Apps { .. } | Overlay::AppActions { .. })
    }

    /// Go to view `v`, leaving whatever list is open.
    fn switch_view(&mut self, v: View, actions: &Handle) {
        match v {
            View::Guide if matches!(self.overlay, Overlay::Guide { .. }) => {}
            View::Guide => {
                self.overlay = Overlay::None;
                self.open_guide(None, actions);
            }
            View::Apps if matches!(self.overlay, Overlay::Apps { .. }) => {}
            View::Apps => {
                self.overlay = Overlay::None;
                self.apply(Input::OpenApps, actions);
            }
            View::Remote => {
                self.overlay = Overlay::None;
                if self.mode != Mode::Remote {
                    self.apply(Input::ToggleMode, actions);
                }
            }
        }
    }

    /// Move to the overlay `next`, running `action` if there is one.
    fn apply_overlay(&mut self, next: Overlay, action: Option<Action>, actions: &Handle) {
        let was = self.overlay;
        self.overlay = next;
        if matches!(next, Overlay::Prompt(_)) && !matches!(was, Overlay::Prompt(_)) {
            self.prompt.clear();
        }
        if let Overlay::Apps { tab, .. } = next {
            self.apps_tab = tab;
        }
        match (was, next, action) {
            (_, _, Some(Action::Run { id, mut args })) => {
                let a = tveee_core::actions::find(&self.actions, id);
                self.note = format!("{}...", a.map_or(id, |m| m.title));
                // typed arguments (not text it asks for): word by word, as
                // on the command line
                if a.is_some_and(|a| a.params == Params::None) && args.len() == 1 {
                    args = args[0].split_whitespace().map(str::to_string).collect();
                }
                // with text typed (e.g. "open a link"): something new should come up
                if !args.is_empty() {
                    self.settle(self.note.clone(), None);
                }
                let _ = actions.dispatch(id, args);
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
                if self.st.running.contains(&pkg) && target != "home" {
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
    fn click(&mut self, x: u16, y: u16, actions: &Handle) -> bool {
        let hit = self.hit_at(x, y);
        match (self.overlay, hit) {
            (Overlay::Menu(_), Some(Hit::MenuItem(i))) => {
                let (next, action) = pick(&self.menu, i);
                self.apply_overlay(next, action, actions);
            }
            (Overlay::Menu(_), Some(Hit::Popup)) => {}
            (Overlay::Confirm { id, .. }, Some(Hit::ConfirmYes)) => {
                self.apply_overlay(Overlay::None, Some(run(id)), actions)
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
                let rows = tab_list(self.st.apps_list(), tab);
                let target = rows.get(i).map(|e| e.target.clone());
                if let Some(target) = target {
                    self.apply_overlay(Overlay::None, Some(Action::Launch(target)), actions);
                }
            }
            (Overlay::Apps { .. }, Some(Hit::Popup | Hit::AppTab(_))) => {}
            (Overlay::AppActions { tab, sel, .. }, Some(Hit::AppChoice(i))) => self.run_choice(tab, sel, i, actions),
            (Overlay::AppActions { .. }, Some(Hit::Popup)) => {}
            (Overlay::AppActions { tab, sel, .. }, _) => self.overlay = Overlay::Apps { tab, sel },
            (Overlay::Prompt(_) | Overlay::Search(_) | Overlay::Palette(_), Some(Hit::Popup)) => {}
            // a click elsewhere closes the pop-up (cancelling a confirmation)
            (Overlay::Help | Overlay::Menu(_) | Overlay::Confirm { .. } | Overlay::Apps { .. } | Overlay::Prompt(_) | Overlay::Search(_) | Overlay::Palette(_), _) => {
                self.apply_overlay(Overlay::None, None, actions)
            }
            (Overlay::None, Some(Hit::Input(i))) => return self.apply(i, actions),
            (Overlay::None, Some(Hit::Progress { x0, width })) => {
                let _ = actions.send(Action::Jump(bar_fraction(x, x0, width)));
            }
            (Overlay::None, Some(Hit::Volume { x0, width })) => {
                if let Some((v, max)) = self.st.snap.as_ref().and_then(|s| s.volume) {
                    let target = (bar_fraction(x, x0, width) * f64::from(max)).round() as i32;
                    let _ = actions.send(Action::Volume(target - v as i32));
                }
            }
            (Overlay::None, _) => {}
            (Overlay::Ambilight { .. }, Some(Hit::AmbiOption(row, col))) => self.ambi_pick(row, col, actions),
            (Overlay::Ambilight { .. }, Some(Hit::Popup)) => {}
            (Overlay::Ambilight { .. }, _) => self.overlay = Overlay::None,
            (Overlay::AmbiColour { .. }, Some(Hit::Popup)) => {}
            (Overlay::AmbiColour { row, col }, _) => self.overlay = Overlay::Ambilight { row, col },
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
    fn scroll(&mut self, up: bool, actions: &Handle) {
        match self.overlay {
            Overlay::Menu(_) => {
                let code = if up { KeyCode::Up } else { KeyCode::Down };
                let (next, action) = overlay_key(self.overlay, code, &self.menu);
                self.apply_overlay(next, action, actions);
            }
            Overlay::Apps { tab, sel } => {
                let rows = tab_list(self.st.apps_list(), tab);
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

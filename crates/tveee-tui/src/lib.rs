//! `tveee ui`: an interactive terminal remote (ratatui), a front end on the
//! core's engine. The `tveee` binary has it with its `ui` cargo feature.
//!
//! The screen shows what is on (as `nowplaying`) plus the volume, and keys
//! control playback and volume. All TV traffic happens on a worker thread so
//! the screen never waits on adb; the worker keeps one devtools connection
//! open, which makes refreshing every second and exact seeking cheap.

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};
use tveee_core::Ctx;
use tveee_core::actions::{ActionDescriptor, Params};
use tveee_core::builtin::apps::list_name;
use tveee_core::builtin::{apps, info, search};
use tveee_core::error::Result;
use tveee_core::nowplaying::{self, Activity, bar, progress_bar};

mod ambi;
mod ctxlist;
mod drawing;
mod guide;
mod keymap;
pub mod keys;
mod launcher;
mod links;
mod listnav;
mod logview;
mod overlays;
mod palette;
mod preview;
#[cfg(test)]
mod snapshots;
#[cfg(test)]
mod tests;

pub use self::drawing::*;
pub use self::guide::*;
pub use self::keymap::*;
pub use self::launcher::*;
pub use self::overlays::*;
pub use self::palette::*;
pub use self::preview::*;
use tveee_core::engine::*;

const CAT: &str = "ui";
struct App {
    /// The command palette's own state (what's typed, what it's over).
    palette: palette::PaletteView,
    /// The system panel's own state (shown or not, read or not, paused).
    system: drawing::SystemView,
    /// The plugins: each one's state, and every plugin there is.
    plugins: overlays::PluginsView,
    /// Links: the ones drawn this frame, and whether the URL is shown whole.
    links: links::LinkView,
    /// The guide's own state (where it's scrolled to, its details panel).
    guide: guide::GuideView,
    /// The preview panel's own state.
    preview: preview::PreviewPane,
    /// The Ambilight panel's own state.
    ambi: ambi::AmbiView,
    /// Scrollbars: one being dragged, and the rows they pick from.
    bars: drawing::Bars,
    /// The list pane's own state (its breadcrumbs, a typed location, ...).
    list: ctxlist::ListPane,
    /// The log pop-up's own state.
    log: logview::LogView,
    /// The options dialog's own state.
    options: overlays::OptionsView,
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
    /// Whether the last frame was wide enough for the side column.
    wide: bool,
    /// The launcher tab used last.
    apps_tab: AppTab,
    /// Where the progress bar stays after a pause, seek or stop asked for
    /// here, until the next read says what it's doing (no prediction).
    frozen: Option<f64>,
    /// tveee's options as this app reads them: None, the saved ones; tests
    /// give each app its own (never the real file, nor each other's).
    opts: Option<Vec<(String, bool)>>,
    /// What has been typed into the open prompt.
    prompt: String,
    /// The search box's app choices, made when it opens.
    search_targets: Vec<SearchTarget>,
    /// An action still settling: its label stands in for the what line.
    settling: Option<Settling>,
    /// Off (`--no-settle`): show every state the TV reports, as it comes.
    settle: bool,
    /// How this terminal shows pictures (asked once at the start).
    picker: Option<ratatui_image::picker::Picker>,
    /// Something asked to quit (the palette's ui: quit).
    quit: bool,
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
/// first) and then stayed the same for `SETTLE_STEADY`; when something
/// unexpected comes up; when nothing changes for `SETTLE`; or after
/// `SETTLE_MAX` in all. Then the latest snapshot is shown as it is.
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
        if app.quit {
            return Ok(());
        }
        if dirty || app.animating() || drawn.is_none_or(|d| d.elapsed() >= TICK) {
            let frame = term.draw(|f| app.draw(f))?;
            // only the links still showing (not under a pop-up)
            let _ = links::emit(&links::showing(&app.links.drawn, frame.buffer));
            drawn = Some(Instant::now());
            dirty = false;
        }
        // drawing the details panel can find a description it needs
        if let Some((ch, start)) = app.guide.want_info.take() {
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
            // the commands open over anything (: where it isn't typed text)
            Event::Key(k) if k.kind != KeyEventKind::Release && app.palette_wanted(k.code, k.modifiers) => app.palette_over(),
            Event::Key(k) if k.kind != KeyEventKind::Release => {
                if let Overlay::AppActions { tab, sel, at } = app.overlay {
                    app.choice_key(tab, sel, at, k.code, actions);
                } else if let Overlay::Logs { at, from } = app.overlay {
                    app.logs_key(at, from, k.code, k.modifiers);
                } else if let Overlay::SaveLog = app.overlay {
                    app.save_log_key(k.code, k.modifiers);
                } else if let Overlay::Options(i) = app.overlay {
                    app.options_key(i, k.code);
                } else if let Overlay::Guide { tab, at } = app.overlay {
                    app.guide_key(tab, at, k.code, k.modifiers, actions);
                } else if let Overlay::List { sel } = app.overlay {
                    app.list_key(sel, k.code, k.modifiers, actions);
                } else if let Overlay::Search(at) = app.overlay {
                    let mut text = std::mem::take(&mut app.prompt);
                    let (next, action) = search_key(at, k.code, k.modifiers, &mut text, &app.search_targets);
                    app.prompt = text;
                    app.apply_overlay(next, action, actions);
                } else if let (Overlay::Apps { .. }, KeyCode::Char('#')) = (app.overlay, k.code) {
                    // this, the launcher's / below and the PIN box: their keys are
                    // listed in keys.rs ("the apps", "pairing"); change those rows with these
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
                } else if let Overlay::Choose { id, sel } = app.overlay {
                    app.choose_key(id, sel, k.code, actions);
                } else if let Overlay::Palette(sel) = app.overlay {
                    app.palette_key(sel, k.code, k.modifiers, actions);
                } else if let Overlay::Prompt(id) = app.overlay
                    && let Some(step) = prompt_step(id, k.code)
                {
                    // ←/→ in a volume or seek box: stepped now (as the main
                    // screen's keys do), the box still open
                    let _ = id;
                    app.apply(Input::Do(step), actions);
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
                    Overlay::Prompt(_) | Overlay::Search(_) => app.prompt.push_str(&paste_text(&p)),
                    Overlay::Palette(_) => app.palette.query.push_str(&paste_text(&p)),
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
            Event::Mouse(m) if app.mouse(m, actions) => return Ok(()),
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
            "--keys" => {
                println!("{}", keys::help());
                return Ok(());
            }
            _ => return tveee_core::error::usage(format!("usage: tveee ui [--no-mouse] [--no-settle] [--keys]  (unexpected '{a}')")),
        }
    }
    use std::io::IsTerminal;
    if !std::io::stdout().is_terminal() {
        return tveee_core::error::usage("ui needs a terminal");
    }
    // the log pop-up's lines, kept from the start
    tveee_core::log::keep_recent();
    let engine = Engine::start(&c.tv.ip, c.actions.clone());
    let mut app = App::new(c.tv.ip.clone(), engine.handle.clone(), c.actions.clone(), settle);
    app.plugins.states = c.plugins.clone();
    app.plugins.all = Some(c.all);
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
    let mut v = vec![
        ActionDescriptor::terminal("ui.ui", CAT, "interactive remote: what's on, volume, seeking", ui)
            .cli("ui", "[--no-mouse] [--no-settle] [--keys]", "interactive remote: what's on, volume, seeking")
            .long(
                "A full-screen remote showing what is playing (as 'tveee nowplaying') and the\n\
         volume, refreshed every second. Two modes, switched with Tab: in player mode\n\
         the arrows seek and change volume; in remote mode the keyboard is the TV's:\n\
         arrows move around its menus, Enter is OK, Esc is back, and what you type\n\
         (or paste) is typed into the TV. Tab switches back; Ctrl-C quits.\n\
         \n\
         Keys: ? in the remote shows them all, by where they work; --keys prints\n\
         them here (the same list).\n\
         \n\
         The top line: the TV's load and free memory, [F]ree, [l] the Ambilight,\n\
         [:] commands (click them too). A small pane keeps one row of keys. The\n\
         launcher marks running apps ● and ones Android keeps running ⊘ (live TV:\n\
         they can't be closed). The preview is scrcpy at 320 pixels with scrcpy\n\
         and ffmpeg installed (it sees video too), YouTube's storyboard frames\n\
         while it starts, else a screenshot every few seconds (video is black).\n\
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
            ),
    ];
    // what the ui does itself (the palette's "ui: ..."), for listings
    v.extend(palette::ui_actions());
    v
}

impl App {
    /// A screen on `engine`'s state, nothing open yet.
    fn new(ip: String, engine: Handle, actions: Vec<ActionDescriptor>, settle: bool) -> App {
        let actions = palette::with_ui(actions);
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
            wide: false,
            apps_tab: AppTab::Known,
            system: Default::default(),
            plugins: Default::default(),
            links: Default::default(),
            guide: Default::default(),
            preview: Default::default(),
            ambi: Default::default(),
            bars: Default::default(),
            list: Default::default(),
            log: Default::default(),
            options: Default::default(),
            frozen: None,
            opts: None,
            palette: Default::default(),
            prompt: String::new(),
            search_targets: Vec::new(),
            settling: None,
            settle,
            picker: None,
            quit: false,
        }
    }
}

impl App {
    fn handle(&mut self, e: CoreEvent) {
        match e {
            CoreEvent::Changed(change) => {
                self.st = self.engine.state();
                // a read (or a seek landing) says where it is now
                if matches!(change, Change::Snapshot | Change::Seeked) {
                    self.frozen = None;
                }
                self.list_sync(change);
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
                    self.preview.image = Some(picker.new_resize_protocol(image::DynamicImage::ImageRgb8(img)));
                    self.preview.source = p.source;
                }
            }
            CoreEvent::PreviewNote(n) => {
                if !n.is_empty() && n != self.preview.note {
                    tveee_core::tlog!("preview: {n}");
                }
                self.preview.note = n;
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
        let mut passing: Vec<String> =
            self.st.apps_list().iter().filter(|e| e.section == SECTIONS[0] || apps::passes_through(&e.package)).map(|e| e.package.clone()).collect();
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
    /// What's on's link, if it has one.
    fn url(&self) -> Option<String> {
        self.st.snap.as_ref()?.activity.as_ref()?.url.clone()
    }

    fn apply(&mut self, input: Input, actions: &Handle) -> bool {
        // playback asked to change: the bar stays where it is until a read
        // says what happened
        if let Input::Do(a) = &input
            && stops_prediction(a)
        {
            self.frozen = self.progress_now().map(|p| p.position);
        }
        match input {
            Input::Quit => return true,
            Input::ToggleMode => {
                self.mode = if self.mode == Mode::Player { Mode::Remote } else { Mode::Player };
                self.note = match self.mode {
                    Mode::Remote => "remote mode: keys go to the TV (typing too); Tab for player mode".into(),
                    Mode::Player => "player mode: arrows seek and change volume".into(),
                };
            }
            Input::ToggleHelp => self.overlay = Overlay::Help(0),
            Input::ToggleInfo => {
                self.system.shown = Some(!self.info_shown());
                self.system.frozen = None;
            }
            Input::PauseInfo => {
                self.note = if self.system.frozen.take().is_some() {
                    "system panel: updating again".into()
                } else if !self.info_shown() {
                    "the system panel isn't open (i opens it)".into()
                } else {
                    // kept as they are now; the TV isn't asked for them meanwhile
                    self.system.frozen = self.st.snap.clone().map(|s| (s, self.st.mem_history.clone()));
                    "system panel paused (I again resumes it)".into()
                };
            }
            Input::ToggleAmbilight if self.panel_id().is_none() => {
                self.note = "no Ambilight here that tveee can control".into();
            }
            Input::ToggleAmbilight => {
                self.ambi.open = !self.ambi.open;
                if let Some(id) = self.panel_id() {
                    let _ = actions.send(Action::WatchPanel(id, self.ambi.open));
                }
            }
            Input::TogglePreview => {
                self.preview.open = !self.preview.open;
                if !self.preview.open {
                    (self.preview.image, self.preview.source) = (None, "");
                    self.preview.note.clear();
                }
                let _ = actions.send(Action::PreviewPanel(self.preview.open));
            }
            Input::AmbilightDialog if self.panel_id().is_none() => {
                self.note = "no Ambilight here that tveee can control".into();
            }
            Input::AmbilightDialog => {
                // the panel's reads feed the dialog
                if let (false, Some(id)) = (self.ambi.open, self.panel_id()) {
                    self.ambi.open = true;
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
            Input::ToggleVisualiser => {
                self.note = "the music visualiser...".into();
                let _ = actions.send(Action::Visualiser);
            }
            Input::OpenList => self.open_list(actions),
            Input::OpenOptions => {
                self.options.on_tabs = false;
                self.overlay = Overlay::Options(0);
            }
            Input::OpenMenu if !self.menu.is_empty() => {
                // in standby, start on "wake"
                let standby = self.st.snap.as_ref().is_some_and(|s| s.standby);
                let wake = self.menu.iter().position(|m| m.id == "power.wake").filter(|_| standby);
                self.overlay = Overlay::Menu(wake.unwrap_or(0));
            }
            Input::OpenMenu => self.note = "no menu items in this build".into(),
            Input::OpenPalette => self.open_palette(),
            Input::ShowLogs => self.overlay = Overlay::Logs { at: None, from: None },
            Input::ToggleUrl => match self.url() {
                Some(_) => self.links.url_full = !self.links.url_full,
                None => self.note = "no link for what's on".into(),
            },
            Input::CopyUrl => self.note = self.url().map_or_else(|| "no link for what's on".into(), |u| links::copy(&u)),
            Input::OpenUrl => self.note = self.url().map_or_else(|| "no link for what's on".into(), |u| links::open(&u)),
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

    /// A mouse event. Returns true to quit.
    fn mouse(&mut self, m: ratatui::crossterm::event::MouseEvent, actions: &Handle) -> bool {
        use ratatui::crossterm::event::{MouseButton, MouseEventKind};
        match m.kind {
            // a scrollbar: grabbed (its thumb) or clicked (the track:
            // the thumb jumps there), then dragged; the mouse is its
            // until let go, wherever it goes
            MouseEventKind::Down(MouseButton::Left) if matches!(self.hit_at(m.column, m.row), Some(Hit::Scrollbar(_))) => {
                if let Some(Hit::Scrollbar(b)) = self.hit_at(m.column, m.row) {
                    let cell = m.row.saturating_sub(b.area.y);
                    let (start, len) = b.thumb();
                    let grab = if (start..start + len).contains(&cell) { cell - start } else { len / 2 };
                    self.bars.drag = Some((b, grab));
                    self.drag_bar(m.row);
                }
            }
            MouseEventKind::Drag(MouseButton::Left) if self.bars.drag.is_some() => self.drag_bar(m.row),
            MouseEventKind::Up(MouseButton::Left) if self.bars.drag.is_some() => self.bars.drag = None,
            // the log's lines: shift+click selects to there; a drag selects
            MouseEventKind::Down(MouseButton::Left)
                if matches!((self.overlay, self.hit_at(m.column, m.row)), (Overlay::Logs { .. }, Some(Hit::LogLine(_)))) =>
            {
                if let Some(Hit::LogLine(n)) = self.hit_at(m.column, m.row) {
                    self.log_click(n, m.modifiers.contains(KeyModifiers::SHIFT));
                    self.log.drag = true;
                }
            }
            MouseEventKind::Drag(MouseButton::Left) if self.log.drag => {
                if let Some(Hit::LogLine(n)) = self.hit_at(m.column, m.row) {
                    self.log_click(n, true);
                }
            }
            MouseEventKind::Up(MouseButton::Left) => self.log.drag = false,
            MouseEventKind::Down(MouseButton::Left) => {
                if self.click(m.column, m.row, actions) {
                    return true;
                }
            }
            MouseEventKind::Down(MouseButton::Right) => {
                if let (Overlay::Apps { tab, .. }, Some(Hit::AppItem(i))) = (self.overlay, self.hit_at(m.column, m.row)) {
                    self.overlay = Overlay::AppActions { tab, sel: i, at: 0 };
                }
            }
            MouseEventKind::ScrollUp => self.scroll(true, actions),
            MouseEventKind::ScrollDown => self.scroll(false, actions),
            _ => {}
        }
        false
    }

    /// The scrollbar being dragged, with the mouse at row `y`: its pane
    /// scrolled to match.
    fn drag_bar(&mut self, y: u16) {
        let Some((b, grab)) = self.bars.drag else { return };
        let top = b.top_at(i32::from(y) - i32::from(b.area.y) - i32::from(grab));
        let room = usize::from(b.area.height);
        // the row in the middle of what's to be shown: the nearest item to
        // it (panes that keep their highlight in view scroll by moving it)
        let middle = |rows: &[Option<usize>]| {
            let want = (top + room / 2).min(rows.len().saturating_sub(1));
            (0..rows.len()).filter_map(|r| rows[r].map(|i| (r.abs_diff(want), i))).min().map(|(_, i)| i)
        };
        match (b.of, self.overlay) {
            (drawing::Bar::Help, Overlay::Help(_)) => self.overlay = Overlay::Help(top),
            (drawing::Bar::Log, Overlay::Logs { at, from }) => {
                let (first, lines) = tveee_core::log::recent_numbered();
                let last = (first + lines.len()).saturating_sub(1);
                let top = first + top;
                // the highlight kept in what's shown (stops following the newest)
                let cur = at.unwrap_or(last).clamp(top, (top + room).saturating_sub(1).min(last));
                self.log.top = Some(top);
                self.overlay = Overlay::Logs { at: Some(cur), from };
            }
            (drawing::Bar::List, Overlay::List { .. }) => {
                if let Some(i) = middle(&self.bars.rows) {
                    self.overlay = Overlay::List { sel: i };
                }
            }
            (drawing::Bar::Options, Overlay::Options(_)) => {
                if let Some(i) = middle(&self.bars.rows) {
                    self.overlay = Overlay::Options(i);
                }
            }
            // the pane closed under it
            _ => self.bars.drag = None,
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
            // a button on the now-playing card: as if chosen from the menu
            (Overlay::None, Some(Hit::Chip(i))) => {
                let (next, action) = pick(&self.menu, i);
                self.apply_overlay(next, action, actions);
            }
            (Overlay::Menu(_), Some(Hit::MenuItem(i))) => {
                let (next, action) = pick(&self.menu, i);
                self.apply_overlay(next, action, actions);
            }
            (Overlay::Menu(_), Some(Hit::Popup)) => {}
            (Overlay::Confirm { id, .. }, Some(Hit::ConfirmYes)) => self.apply_overlay(Overlay::None, Some(run(id)), actions),
            (Overlay::Confirm { .. }, Some(Hit::Popup)) => {}
            (Overlay::Apps { tab, .. }, Some(Hit::AppTab(t))) if t != tab => {
                self.apply_overlay(Overlay::Apps { tab: t, sel: 0 }, None, actions);
            }
            (Overlay::Options(_), Some(Hit::OptionItem(i))) => self.toggle_option(i),
            (Overlay::Options(_), Some(Hit::OptionTab(plugins))) => {
                self.options.plugins = plugins;
                self.overlay = Overlay::Options(0);
            }
            (Overlay::Options(_), Some(Hit::Popup)) => {}
            (Overlay::Options(_), _) => self.overlay = Overlay::None,
            (Overlay::Guide { .. }, Some(Hit::Details(click))) => {
                self.guide.details = match click {
                    DetailsClick::Open => Details::Open,
                    DetailsClick::Focus => Details::Focused,
                    DetailsClick::Unfocus => Details::Open,
                    DetailsClick::Hide => Details::Closed,
                };
            }
            (Overlay::Guide { .. }, Some(Hit::GuideStar(id))) => self.toggle_favourite(id, actions),
            (Overlay::Guide { .. }, Some(Hit::GuideRow(id))) => {
                if self.guide.details == Details::Focused {
                    self.guide.details = Details::Open;
                }
                self.watch_channel(id, actions);
            }
            (Overlay::Guide { at, .. }, Some(Hit::GuideTab(t))) => self.guide_tab(t, at),
            (Overlay::None, Some(Hit::Open(o))) => self.open(o, actions),
            (Overlay::Guide { .. }, Some(Hit::Popup)) => {}
            // clicking outside closes it
            (Overlay::Guide { .. }, _) => self.overlay = Overlay::None,
            (Overlay::List { .. }, Some(Hit::ListRow(i))) => self.list_choose(i, actions),
            (Overlay::List { .. }, Some(Hit::Crumb(i))) => self.list_crumb(i, actions),
            (Overlay::List { .. }, Some(Hit::Popup)) => {}
            (Overlay::List { .. }, _) => self.close_list(actions),
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
            // the log: a click inside does nothing, outside closes it
            (Overlay::Logs { .. }, Some(Hit::LogLine(n))) => self.log_click(n, false),
            (Overlay::Logs { .. }, Some(Hit::Popup)) => {}
            (Overlay::Logs { .. }, _) => self.overlay = Overlay::None,
            (Overlay::SaveLog, Some(Hit::Popup)) => {}
            (Overlay::SaveLog, _) => self.overlay = self.log.back.take().unwrap_or(Overlay::None),
            // a click elsewhere closes the pop-up (cancelling a confirmation)
            (
                Overlay::Help(_)
                | Overlay::Menu(_)
                | Overlay::Confirm { .. }
                | Overlay::Apps { .. }
                | Overlay::Prompt(_)
                | Overlay::Search(_)
                | Overlay::Palette(_),
                _,
            ) => {
                let palette = matches!(self.overlay, Overlay::Palette(_));
                self.apply_overlay(Overlay::None, None, actions);
                if palette {
                    self.palette_closed();
                }
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
            // a choice in the panel: the cursor there, and changed
            (Overlay::None, Some(Hit::AmbiOption(row, col))) => {
                self.overlay = Overlay::Ambilight { row, col };
                self.ambi_pick(row, col, actions);
            }
            (Overlay::None, _) => {}
            (Overlay::Ambilight { .. }, Some(Hit::AmbiOption(row, col))) => self.ambi_pick(row, col, actions),
            (Overlay::Ambilight { .. }, Some(Hit::Input(Input::AmbilightDialog))) => {}
            (Overlay::Ambilight { .. }, Some(Hit::Popup)) => {}
            (Overlay::Ambilight { .. }, _) => self.overlay = Overlay::None,
            (Overlay::AmbiColour { row, col }, Some(Hit::AmbiPick(x, y))) => {
                self.ambi.pick = (x, y);
                self.ambi.typed.clear();
                self.ambi_set(row, col, actions);
            }
            // another light (or any choice in the panel) while picking a colour
            (Overlay::AmbiColour { .. }, Some(Hit::AmbiOption(r, c))) => self.ambi_switch(r, c, actions),
            (Overlay::AmbiColour { .. }, Some(Hit::Popup)) => {}
            // the panel beside it: not a reason to stop picking
            (Overlay::AmbiColour { .. }, Some(Hit::Input(Input::AmbilightDialog))) => {}
            (Overlay::AmbiColour { row, col }, _) => self.overlay = Overlay::Ambilight { row, col },
            // a list of choices: one clicked runs it; outside it, closed
            (Overlay::Choose { id, .. }, Some(Hit::ChoiceItem(i))) => self.run_action_choice(id, i, actions),
            (Overlay::Choose { .. }, Some(Hit::Popup)) => {}
            (Overlay::Choose { .. }, _) => self.overlay = Overlay::None,
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
            // the wheel scrolls the log, three lines a notch
            Overlay::Logs { .. } => {
                for _ in 0..3 {
                    let Overlay::Logs { at, from } = self.overlay else { break };
                    self.logs_key(at, from, if up { KeyCode::Up } else { KeyCode::Down }, KeyModifiers::NONE);
                }
            }
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
            Overlay::List { sel } => self.list_key(sel, if up { KeyCode::Up } else { KeyCode::Down }, KeyModifiers::NONE, actions),
            Overlay::None => {
                let _ = actions.send(Action::Volume(if up { 1 } else { -1 }));
            }
            _ => {}
        }
    }
}

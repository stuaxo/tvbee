//! `tvbee ui`: an interactive terminal remote (ratatui). Only built with the
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

const CAT: &str = "ui";
const REFRESH: Duration = Duration::from_secs(1);
/// What one fast-forward/rewind key press is assumed to skip, for apps we
/// can't seek exactly.
const KEY_STEP: f64 = 10.0;

/// A user request, handled on the worker thread.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    PlayPause,
    Seek(f64),
    Jump(f64),
    Volume(i32),
    Mute,
    /// Any remote key (back, home, d-pad, OK), by Android keycode.
    Key(u32),
    /// Run menu item N (an index into the plugins' menu items).
    Menu(usize),
    /// Run menu item N with the text the user typed for it.
    MenuText(usize, String),
    /// Start an app: an alias, package or package/activity.
    Launch(String),
    /// Search in an app; `keys` means by typing into it (no search intent).
    Search { pkg: String, query: String, keys: bool },
    Refresh,
}

pub const KEY_BACK: u32 = 4;
pub const KEY_HOME: u32 = 3;
const DPAD: [(KeyCode, u32, &str); 4] =
    [(KeyCode::Up, 19, "up"), (KeyCode::Down, 20, "down"), (KeyCode::Left, 21, "left"), (KeyCode::Right, 22, "right")];
pub const KEY_OK: u32 = 23;

/// What the arrow keys do: seek/volume, or move around the TV's menus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Player,
    Remote,
}

/// What a key press means to the UI.
#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    Quit,
    ToggleMode,
    ToggleHelp,
    ToggleInfo,
    OpenMenu,
    OpenApps,
    OpenSearch,
    Do(Action),
    Nothing,
}

pub fn key_name(code: u32) -> &'static str {
    match code {
        KEY_BACK => "back",
        KEY_HOME => "home",
        KEY_OK => "ok",
        c => DPAD.iter().find(|(_, k, _)| *k == c).map_or("key", |(_, _, n)| n),
    }
}

/// Map a key to what it means in `mode`.
pub fn input_for(code: KeyCode, mods: KeyModifiers, mode: Mode) -> Input {
    use Action::*;
    let act = |a| Input::Do(a);
    match (code, mode) {
        (KeyCode::Char('q') | KeyCode::Esc, _) => Input::Quit,
        (KeyCode::Char('c'), _) if mods.contains(KeyModifiers::CONTROL) => Input::Quit,
        (KeyCode::Tab, _) => Input::ToggleMode,
        (KeyCode::Char('?'), _) => Input::ToggleHelp,
        (KeyCode::Char('.'), _) => Input::OpenMenu,
        (KeyCode::Char('i'), _) => Input::ToggleInfo,
        (KeyCode::Char('a'), _) => Input::OpenApps,
        (KeyCode::Char('/'), _) => Input::OpenSearch,
        (KeyCode::Char(' ') | KeyCode::Char('p'), _) => act(PlayPause),
        (KeyCode::Backspace, _) => act(Key(KEY_BACK)),
        (KeyCode::Char('h'), _) => act(Key(KEY_HOME)),
        (KeyCode::Char('['), _) => act(Seek(-60.0)),
        (KeyCode::Char(']'), _) => act(Seek(60.0)),
        (KeyCode::Char(d @ '0'..='9'), _) => act(Jump(f64::from(d.to_digit(10).unwrap_or(0)) / 10.0)),
        (KeyCode::Char('+') | KeyCode::Char('='), _) => act(Volume(1)),
        (KeyCode::Char('-'), _) => act(Volume(-1)),
        (KeyCode::Char('m'), _) => act(Mute),
        (KeyCode::Char('r'), _) => act(Refresh),
        // player mode: arrows seek and change volume
        (KeyCode::Left, Mode::Player) => act(Seek(-10.0)),
        (KeyCode::Right, Mode::Player) => act(Seek(10.0)),
        (KeyCode::Up, Mode::Player) => act(Volume(1)),
        (KeyCode::Down, Mode::Player) => act(Volume(-1)),
        // remote mode: arrows are the d-pad, Enter is OK
        (KeyCode::Enter, Mode::Remote) => act(Key(KEY_OK)),
        (c, Mode::Remote) => DPAD.iter().find(|(k, _, _)| *k == c).map_or(Input::Nothing, |(_, code, _)| act(Key(*code))),
        _ => Input::Nothing,
    }
}

/// Fold queued actions: consecutive seeks/volume steps add up, so holding a
/// key doesn't leave a backlog of round trips.
pub fn coalesce(actions: Vec<Action>) -> Vec<Action> {
    let mut out: Vec<Action> = Vec::new();
    for a in actions {
        match (out.last_mut(), &a) {
            (Some(Action::Seek(x)), Action::Seek(y)) => *x += y,
            (Some(Action::Volume(x)), Action::Volume(y)) => *x += y,
            (Some(Action::Refresh), Action::Refresh) => {}
            _ => out.push(a),
        }
    }
    out
}

#[derive(Debug, PartialEq)]
pub enum Batch {
    Keys(Vec<u32>),
    One(Action),
}

/// Group runs of remote keys so typing ↓↓↓⏎ quickly is one round trip, in order.
pub fn batches(actions: Vec<Action>) -> Vec<Batch> {
    let mut out: Vec<Batch> = Vec::new();
    for a in actions {
        match (out.last_mut(), a) {
            (Some(Batch::Keys(ks)), Action::Key(k)) => ks.push(k),
            (_, Action::Key(k)) => out.push(Batch::Keys(vec![k])),
            (_, a) => out.push(Batch::One(a)),
        }
    }
    out
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
    /// "player" or "player, guessed": what kind of app is in front.
    kind: Option<String>,
    power: Option<String>,
    focus: Option<String>,
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
}

struct Worker {
    tv: Tv,
    conn: Option<devtools::Conn>,
    pkg: Option<String>,
    /// Set by the UI on quit: finish up, and don't open new port forwards.
    stop: Arc<AtomicBool>,
    menu: Vec<MenuItem>,
    /// Kinds of apps tvbee has no entry for (fetched once, cached a day).
    guesses: Vec<apps::Guess>,
    /// Apps with a proper search (fetched once).
    searchable: Vec<search::Searchable>,
}

impl Worker {
    /// The devtools connection for `pkg`, opened (or re-opened) on demand.
    fn conn(&mut self, pkg: &str) -> Option<&devtools::Conn> {
        if self.conn.as_ref().is_some_and(|c| c.pkg != pkg) {
            self.conn = None;
        }
        if self.conn.is_none() && !self.stopping() {
            self.conn = devtools::Conn::open(&self.tv, pkg);
        }
        self.conn.as_ref()
    }

    fn stopping(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    fn snapshot(&mut self) -> Result<Snapshot> {
        self.tv.connect()?;
        let tv = &self.tv;
        let (power, window, media_dump, audio, system) = std::thread::scope(|sc| {
            let p = sc.spawn(|| tv.shell(&["dumpsys", "power"]));
            let w = sc.spawn(|| tv.window_dump());
            let m = sc.spawn(|| tv.shell(&["dumpsys", "media_session"]));
            let y = sc.spawn(|| info::read_system(tv));
            let a = tv.shell(&["dumpsys", "audio"]);
            let j = |h: std::thread::ScopedJoinHandle<'_, Result<String>>| h.join().unwrap_or_else(|_| Ok(String::new()));
            // the system figures only feed the optional panel
            let y = y.join().ok().and_then(|r| r.ok()).unwrap_or_default();
            (j(p), j(w), j(m), a, y)
        });
        let (power, window, audio) = (power?, window?, audio?);
        let media_dump = media_dump.unwrap_or_default();
        let awake = power.lines().any(|l| l.trim() == "mWakefulness=Awake");
        let pkg = info::focus_package(&window);
        self.pkg = pkg.clone();
        let sessions = media::parse_media_sessions(&media_dump);
        let mut exact = false;
        let activity = match (&pkg, awake) {
            (_, false) => Some(Activity { what: "In standby".into(), title: None, url: None, progress: None }),
            (Some(pkg), true) => {
                let pkg = pkg.clone();
                let who = apps::identify(&pkg, &self.guesses);
                let use_dt = nowplaying::has_devtools(&pkg);
                let mut pages = || {
                    if !use_dt {
                        return Vec::new();
                    }
                    let got = self.conn(&pkg).map(|c| c.pages());
                    match got {
                        Some(Ok(p)) => p,
                        // stale forward (app restarted?): drop it, try once more
                        _ => {
                            self.conn = None;
                            self.conn(&pkg).and_then(|c| c.pages().ok()).unwrap_or_default()
                        }
                    }
                };
                let a = nowplaying::activity_with(&pkg, who, &sessions, &mut pages);
                exact = a.as_ref().is_some_and(|a| a.progress.is_some());
                a
            }
            (None, true) => None,
        };
        Ok(Snapshot {
            activity,
            volume: media::parse_volume(&audio),
            muted: media::parse_muted(&audio),
            exact,
            standby: !awake,
            kind: pkg.as_deref().and_then(|p| {
                let known = apps::by_package(p).is_some();
                apps::identify(p, &self.guesses)
                    .map(|(_, k)| if known { k.as_str().to_string() } else { format!("{}, guessed", k.as_str()) })
            }),
            power: power.lines().find_map(|l| l.trim().strip_prefix("mWakefulness=")).map(str::to_string),
            focus: info::focus_window(&window),
            system,
        })
    }

    /// Carry out one action; returns status-line feedback.
    fn act(&mut self, a: Action, tx: &Sender<Msg>) -> Result<String> {
        self.tv.connect()?;
        Ok(match a {
            Action::PlayPause => {
                self.tv.keyevent(85)?;
                "play/pause".into()
            }
            Action::Volume(0) => "volume".into(),
            Action::Volume(n) => {
                let key = if n > 0 { "24" } else { "25" };
                let mut cmd = vec!["input", "keyevent"];
                cmd.extend(std::iter::repeat_n(key, n.unsigned_abs() as usize));
                self.tv.shell(&cmd)?;
                format!("volume {}{}", if n > 0 { "+" } else { "" }, n)
            }
            Action::Mute => {
                let now = media::is_muted(&self.tv)?.unwrap_or(false);
                media::set_mute(&self.tv, !now)?;
                if now { "unmuted".into() } else { "muted".into() }
            }
            Action::Seek(d) => match self.exact_seek(SeekTo::By(d)) {
                Some(pos) => {
                    let _ = tx.send(Msg::Seeked(pos, Instant::now()));
                    format!("seeked {:+}s to {}", d, clock(pos))
                }
                None => {
                    // no video we can reach: fast-forward/rewind key presses
                    let n = media::presses(d.abs(), KEY_STEP).max(1);
                    let key = if d > 0.0 { "90" } else { "89" };
                    let mut cmd = vec!["input", "keyevent"];
                    cmd.extend(std::iter::repeat_n(key, n as usize));
                    self.tv.shell(&cmd)?;
                    format!("skipped ~{:+}s ({n} key presses)", d.signum() * n as f64 * KEY_STEP)
                }
            },
            Action::Jump(f) => match self.exact_seek(SeekTo::Fraction(f)) {
                Some(pos) => {
                    let _ = tx.send(Msg::Seeked(pos, Instant::now()));
                    format!("jumped to {}%: {}", (f * 100.0).round(), clock(pos))
                }
                None => "jumping needs a video tvbee can read (currently YouTube)".into(),
            },
            Action::Key(code) => {
                self.tv.keyevent(code)?;
                key_name(code).into()
            }
            Action::Menu(i) => match self.menu.get(i).map(|m| m.run) {
                Some(Run::Now(run)) => run(&self.tv)?,
                Some(Run::WithText { .. }) => "that needs some text".into(),
                None => "no such menu item".into(),
            },
            Action::MenuText(i, text) => match self.menu.get(i).map(|m| m.run) {
                Some(Run::WithText { run, .. }) => run(&self.tv, &text)?,
                _ => "no such menu item".into(),
            },
            Action::Launch(target) => {
                apps::start(&self.tv, &target)?;
                let pkg = apps::pkg_of(&target);
                let name = apps::by_package(pkg).map_or(pkg, |a| a.name);
                format!("launched {name}")
            }
            Action::Search { pkg, query, keys: true } => search::keys_search(&self.tv, &pkg, &query, false)?,
            Action::Search { pkg, query, keys: false } => {
                if self.searchable.is_empty() {
                    self.searchable = search::searchable(&self.tv)?;
                }
                search::search_package(&self.tv, &self.searchable, &pkg, &query, false, false)?
            }
            Action::Refresh => "refreshed".into(),
        })
    }

    /// Send several remote keys in one adb round trip.
    fn keys(&mut self, codes: &[u32]) -> Result<String> {
        self.tv.connect()?;
        let codes: Vec<String> = codes.iter().map(u32::to_string).collect();
        let mut cmd = vec!["input", "keyevent"];
        cmd.extend(codes.iter().map(String::as_str));
        self.tv.shell(&cmd)?;
        let names: Vec<&str> = codes.iter().filter_map(|c| c.parse().ok()).map(key_name).collect();
        Ok(names.join(" "))
    }

    fn exact_seek(&mut self, to: SeekTo) -> Option<f64> {
        let pkg = self.pkg.clone()?;
        if !nowplaying::has_devtools(&pkg) {
            return None;
        }
        self.conn(&pkg)?.seek(to).ok()
    }

    /// Runs until the UI hangs up, then drops the worker (removing its port
    /// forward) *before* the message channel, so the UI can wait for that.
    fn run(mut self, actions: Receiver<Action>, tx: Sender<Msg>) {
        self.serve(&actions, &tx);
        drop(self);
        drop(tx);
    }

    fn serve(&mut self, actions: &Receiver<Action>, tx: &Sender<Msg>) {
        let mut apps_sent = false;
        loop {
            let first = match actions.recv_timeout(REFRESH) {
                Ok(a) => Some(a),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            };
            // keys already pressed still reach the TV, even when quitting
            let mut queued: Vec<Action> = first.into_iter().collect();
            queued.extend(actions.try_iter());
            if self.stopping() {
                queued.retain(|a| *a != Action::Refresh);
            }
            for batch in batches(coalesce(queued)) {
                let result = match batch {
                    Batch::Keys(codes) => self.keys(&codes),
                    Batch::One(a) => self.act(a, tx),
                };
                let msg = match result {
                    Ok(note) => Msg::Note(note),
                    Err(e) => Msg::Failed(e.to_string()),
                };
                if tx.send(msg).is_err() {
                    return;
                }
            }
            if self.stopping() {
                return;
            }
            let msg = match self.snapshot() {
                Ok(s) => Msg::Snap(Box::new(s), Instant::now()),
                // e.g. while it reboots; keep trying every second
                Err(e) => Msg::Failed(format!("waiting for the TV ({e})")),
            };
            if tx.send(msg).is_err() {
                return;
            }
            // after the first refresh, so it doesn't delay the main screen
            if !apps_sent && !self.stopping() && self.tv.connect().is_ok()
                && let Ok(pm) = self.tv.shell(&["pm", "list", "packages"])
            {
                apps_sent = true;
                self.guesses = apps::guesses(&self.tv);
                let rows = launcher_entries(&apps::installed_aliases(&pm), &self.guesses);
                if tx.send(Msg::Apps(rows)).is_err() {
                    return;
                }
                let found = search::searchable(&self.tv).unwrap_or_default();
                self.searchable = found.clone();
                if tx.send(Msg::Searchable(found)).is_err() {
                    return;
                }
            }
        }
    }
}

struct App {
    ip: String,
    snap: Option<Snapshot>,
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
    /// Apps with a proper search; None until the worker has looked.
    searchable: Option<Vec<search::Searchable>>,
    /// The search box's app choices, made when it opens.
    search_targets: Vec<SearchTarget>,
}

const MEM_HISTORY: usize = 600;
/// Height of the memory graph.
const GRAPH_ROWS: u16 = 3;

/// What is drawn over the main screen, if anything.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Overlay {
    None,
    Help,
    /// The "more" menu, with the highlighted item.
    Menu(usize),
    /// "Are you sure?" for menu item N; `yes` is the highlighted answer.
    Confirm { item: usize, yes: bool },
    /// The app launcher: which tab, and the highlighted app in it.
    Apps { tab: AppTab, sel: usize },
    /// Typing the text for menu item N (the text itself is `App::prompt`).
    Prompt(usize),
    /// Typing a search; N picks the app from `App::search_targets`.
    Search(usize),
}

/// An app the search box can search in, and how.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchTarget {
    pub package: String,
    pub name: String,
    /// "search", "results link" or "keys" (typing into the app).
    pub method: &'static str,
}

/// The apps to offer, `first` (the app in front, or the launcher's pick)
/// leading, then everything with a proper search.
pub fn search_targets(first: Option<&str>, searchable: &[search::Searchable]) -> Vec<SearchTarget> {
    let name = |pkg: &str| apps::by_package(pkg).map_or_else(|| pkg.to_string(), |a| list_name(a.name));
    let mut out: Vec<SearchTarget> = Vec::new();
    let pkgs = first.into_iter().map(str::to_string).chain(searchable.iter().map(|s| s.package.clone()));
    for p in pkgs {
        // home screens have nothing to search
        if out.iter().any(|t| t.package == p) || apps::by_package(&p).is_some_and(|a| a.kind == apps::Kind::Launcher) {
            continue;
        }
        out.push(SearchTarget { name: name(&p), method: search::method(searchable, &p), package: p });
    }
    out
}

/// Keys in the search box: Tab / ↑ / ↓ change the app; the rest edit the
/// query (as in [`prompt_key`]); Enter searches; Esc cancels.
pub fn search_key(at: usize, code: KeyCode, mods: KeyModifiers, text: &mut String, targets: &[SearchTarget]) -> (Overlay, Option<Action>) {
    let n = targets.len().max(1);
    match code {
        KeyCode::Tab | KeyCode::Down => (Overlay::Search((at + 1) % n), None),
        KeyCode::BackTab | KeyCode::Up => (Overlay::Search((at + n - 1) % n), None),
        KeyCode::Enter => match targets.get(at) {
            Some(t) if !text.trim().is_empty() => (
                Overlay::None,
                Some(Action::Search { pkg: t.package.clone(), query: text.trim().to_string(), keys: t.method == "keys" }),
            ),
            _ => (Overlay::Search(at), None),
        },
        _ => match prompt_key(0, code, mods, text) {
            (Overlay::None, _) => (Overlay::None, None),
            _ => (Overlay::Search(at), None),
        },
    }
}

/// Keys while typing into a prompt: edit `text`; Enter runs item `item` with
/// it, Esc cancels. Ctrl-U clears.
pub fn prompt_key(item: usize, code: KeyCode, mods: KeyModifiers, text: &mut String) -> (Overlay, Option<Action>) {
    match code {
        KeyCode::Esc => (Overlay::None, None),
        KeyCode::Enter => {
            let t = text.trim().to_string();
            if t.is_empty() { (Overlay::Prompt(item), None) } else { (Overlay::None, Some(Action::MenuText(item, t))) }
        }
        KeyCode::Backspace => {
            text.pop();
            (Overlay::Prompt(item), None)
        }
        KeyCode::Char('u') if mods.contains(KeyModifiers::CONTROL) => {
            text.clear();
            (Overlay::Prompt(item), None)
        }
        KeyCode::Char(c) if !mods.contains(KeyModifiers::CONTROL) => {
            text.push(c);
            (Overlay::Prompt(item), None)
        }
        _ => (Overlay::Prompt(item), None),
    }
}

/// Pasted text, as one line (newlines and other control characters dropped).
pub fn paste_text(s: &str) -> String {
    s.trim_end_matches(['\n', '\r'])
        .chars()
        .map(|c| if c == '\n' || c == '\r' || c == '\t' { ' ' } else { c })
        .filter(|c| !c.is_control())
        .collect()
}

/// A row in the launcher.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub name: String,
    pub alias: Option<&'static str>,
    /// What to pass to `apps::start`.
    pub target: String,
    pub package: String,
    /// For apps without an alias: what kind they look like ("player").
    pub tag: Option<&'static str>,
}

/// The aliased apps first, then other TV-launcher apps with their guessed kind.
pub fn launcher_entries(known: &[&'static apps::App], guessed: &[apps::Guess]) -> Vec<Entry> {
    let mut rows: Vec<Entry> = known
        .iter()
        .map(|a| Entry {
            name: list_name(a.name),
            alias: a.alias,
            target: a.alias.unwrap_or(a.target).to_string(),
            package: a.package().to_string(),
            tag: None,
        })
        .collect();
    let mut others: Vec<Entry> = guessed
        .iter()
        .filter(|g| !rows.iter().any(|r| r.package == g.package))
        .filter_map(|g| {
            let component = g.component.clone()?;
            let table = apps::by_package(&g.package);
            // generic table names ("the home screen", "a NetTV app") don't
            // say which app this is; the package does
            let name = match table {
                Some(a) if !a.name.starts_with("a ") && !a.name.starts_with("the ") => list_name(a.name),
                _ => g.package.clone(),
            };
            let kind = table.map_or(g.kind, |a| a.kind);
            let tag = (kind != apps::Kind::Other).then(|| kind.as_str());
            Some(Entry { name, alias: None, target: component, package: g.package.clone(), tag })
        })
        .collect();
    others.sort_by_key(|e| e.name.to_lowercase());
    rows.extend(others);
    rows
}

/// The launcher's tabs: apps tvbee has aliases for, and the rest of the
/// TV-launcher apps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppTab {
    Known,
    Other,
}

impl AppTab {
    pub fn other(self) -> AppTab {
        if self == AppTab::Known { AppTab::Other } else { AppTab::Known }
    }

    pub fn holds(self, e: &Entry) -> bool {
        (self == AppTab::Known) == e.alias.is_some()
    }
}

/// The rows of one tab.
pub fn tab_list(all: &[Entry], tab: AppTab) -> Vec<Entry> {
    all.iter().filter(|e| tab.holds(e)).cloned().collect()
}

/// Keys in the launcher (`list` is the current tab's rows): arrows + Enter,
/// a letter to jump to the next app whose alias (or name) starts with it, so
/// every letter jumps; Tab, ← or → switch tabs; Esc or Backspace close.
pub fn apps_key(tab: AppTab, sel: usize, code: KeyCode, list: &[Entry]) -> (Overlay, Option<Action>) {
    let n = list.len();
    let at = |sel| (Overlay::Apps { tab, sel }, None);
    match code {
        KeyCode::Esc | KeyCode::Backspace => (Overlay::None, None),
        KeyCode::Tab | KeyCode::Left | KeyCode::Right => (Overlay::Apps { tab: tab.other(), sel: 0 }, None),
        KeyCode::Up if n > 0 => at(sel.checked_sub(1).unwrap_or(n - 1)),
        KeyCode::Down if n > 0 => at((sel + 1) % n),
        KeyCode::Enter => match list.get(sel) {
            Some(e) => (Overlay::None, Some(Action::Launch(e.target.clone()))),
            None => at(sel),
        },
        KeyCode::Char(c) if n > 0 => {
            // next match after the current one, wrapping
            let c = c.to_ascii_lowercase();
            let starts = |e: &Entry| e.alias.map_or_else(|| e.name.to_lowercase(), str::to_string).starts_with(c);
            let next = (1..=n).map(|k| (sel + k) % n).find(|&i| starts(&list[i]));
            at(next.unwrap_or(sel))
        }
        _ => at(sel),
    }
}

/// What a key does while an overlay is open: the new overlay, and maybe an
/// action to run. Esc (or the key that opened it) closes; it never quits.
pub fn overlay_key(overlay: Overlay, code: KeyCode, items: &[MenuItem]) -> (Overlay, Option<Action>) {
    let run = |i: usize| pick(items, i);
    match overlay {
        Overlay::None => (Overlay::None, None),
        // any key closes the help
        Overlay::Help => (Overlay::None, None),
        Overlay::Menu(sel) => match code {
            KeyCode::Esc | KeyCode::Char('.') | KeyCode::Char('q') | KeyCode::Backspace => (Overlay::None, None),
            KeyCode::Up => (Overlay::Menu(sel.checked_sub(1).unwrap_or(items.len().saturating_sub(1))), None),
            KeyCode::Down => (Overlay::Menu(if sel + 1 >= items.len() { 0 } else { sel + 1 }), None),
            KeyCode::Enter => run(sel),
            KeyCode::Char(c) => match items.iter().position(|it| it.key == c) {
                Some(i) => run(i),
                None => (overlay, None),
            },
            _ => (overlay, None),
        },
        // handled by apps_key / prompt_key
        Overlay::Apps { tab, sel } => (Overlay::Apps { tab, sel }, None),
        Overlay::Prompt(i) => (Overlay::Prompt(i), None),
        Overlay::Search(i) => (Overlay::Search(i), None),
        Overlay::Confirm { item, yes } => match code {
            KeyCode::Char('y') | KeyCode::Char('Y') => (Overlay::None, Some(Action::Menu(item))),
            KeyCode::Left | KeyCode::Right | KeyCode::Tab => (Overlay::Confirm { item, yes: !yes }, None),
            KeyCode::Enter if yes => (Overlay::None, Some(Action::Menu(item))),
            // anything else, including Enter on "No", cancels
            _ => (Overlay::None, None),
        },
    }
}

/// What a menu item does when chosen: run it, or ask first if it's risky.
pub fn pick(items: &[MenuItem], i: usize) -> (Overlay, Option<Action>) {
    match items.get(i) {
        Some(it) if matches!(it.run, Run::WithText { .. }) => (Overlay::Prompt(i), None),
        Some(it) if it.confirm.is_some() => (Overlay::Confirm { item: i, yes: false }, None),
        Some(_) => (Overlay::None, Some(Action::Menu(i))),
        None => (Overlay::None, None),
    }
}

/// Something on screen that responds to a click.
#[derive(Debug, Clone, PartialEq)]
pub enum Hit {
    Input(Input),
    /// The progress bar: clicking jumps there.
    Progress { x0: u16, width: u16 },
    /// The volume bar: clicking sets that level.
    Volume { x0: u16, width: u16 },
    MenuItem(usize),
    /// Row N of the launcher's current tab.
    AppItem(usize),
    AppTab(AppTab),
    ConfirmYes,
    ConfirmNo,
    /// Inside a pop-up but on nothing in particular.
    Popup,
}

/// `(c + 0.5) / width`: the fraction that puts a bar's marker on cell `c`.
pub fn bar_fraction(x: u16, x0: u16, width: u16) -> f64 {
    let c = f64::from(x.saturating_sub(x0).min(width.saturating_sub(1)));
    ((c + 0.5) / f64::from(width.max(1))).clamp(0.0, 1.0)
}

/// A clickable button in the footer: the key, what it does, and its effect.
struct Button {
    key: &'static str,
    label: &'static str,
    input: Input,
}

const fn button(key: &'static str, label: &'static str, input: Input) -> Button {
    Button { key, label, input }
}

fn buttons(mode: Mode, exact: bool) -> Vec<Button> {
    use Action::*;
    use Input::{Do, OpenMenu, Quit, ToggleHelp, ToggleMode};
    let mut b = match mode {
        Mode::Player => vec![
            button("space", "play/pause", Do(PlayPause)),
            // exact seeking (YouTube) vs fast-forward/rewind key presses
            button("←", if exact { "-10s" } else { "rew" }, Do(Seek(-10.0))),
            button("→", if exact { "+10s" } else { "ffw" }, Do(Seek(10.0))),
            button("↑", "vol+", Do(Volume(1))),
            button("↓", "vol-", Do(Volume(-1))),
            button("m", "mute", Do(Mute)),
        ],
        Mode::Remote => vec![
            button("↑", "up", Do(Key(19))),
            button("↓", "down", Do(Key(20))),
            button("←", "left", Do(Key(21))),
            button("→", "right", Do(Key(22))),
            button("⏎", "ok", Do(Key(KEY_OK))),
            button("space", "play/pause", Do(PlayPause)),
            button("+", "vol+", Do(Volume(1))),
            button("-", "vol-", Do(Volume(-1))),
        ],
    };
    b.extend([
        button("⌫", "back", Do(Key(KEY_BACK))),
        button("h", "home", Do(Key(KEY_HOME))),
        button("tab", if mode == Mode::Player { "remote" } else { "player" }, ToggleMode),
        button("i", "info", Input::ToggleInfo),
        button("a", "apps", Input::OpenApps),
        button("/", "search", Input::OpenSearch),
        button(".", "more", OpenMenu),
        button("?", "help", ToggleHelp),
        button("q", "quit", Quit),
    ]);
    b
}

fn button_width(b: &Button) -> u16 {
    // " key " + " label"
    u16::try_from(Span::raw(b.key).width() + Span::raw(b.label).width() + 3).unwrap_or(u16::MAX)
}

/// Lay buttons out left to right, wrapping at `width`: (row, x offset) of each.
fn flow(bs: &[Button], width: u16) -> Vec<(u16, u16)> {
    const GAP: u16 = 2;
    let (mut row, mut x) = (0u16, 0u16);
    bs.iter()
        .map(|b| {
            let w = button_width(b);
            if x > 0 && x + w > width {
                (row, x) = (row + 1, 0);
            }
            let at = (row, x);
            x += w + GAP;
            at
        })
        .collect()
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
                (self.snap, self.at, self.error) = (Some(*s), at, None);
            }
            Msg::Note(n) => self.note = n,
            Msg::Failed(e) => self.error = Some(e),
            Msg::Apps(list) => self.apps = Some(list),
            Msg::Searchable(list) => self.searchable = Some(list),
            Msg::Seeked(pos, at) => {
                if let Some(p) = self.snap.as_mut().and_then(|s| s.activity.as_mut()).and_then(|a| a.progress.as_mut()) {
                    p.position = pos;
                    self.at = at;
                }
            }
        }
    }

    /// Open the search box on `pkg` (or the app in front).
    fn open_search(&mut self, pkg: Option<String>) {
        let front = self.snap.as_ref().and_then(|s| s.focus.as_deref()).map(|f| f.split('/').next().unwrap_or(f).to_string());
        let first = pkg.or(front);
        self.search_targets = search_targets(first.as_deref(), self.searchable.as_deref().unwrap_or_default());
        self.prompt.clear();
        self.overlay = Overlay::Search(0);
    }

    /// Act on a key or button. Returns true to quit.
    fn apply(&mut self, input: Input, actions: &Sender<Action>) -> bool {
        match input {
            Input::Quit => return true,
            Input::ToggleMode => {
                self.mode = if self.mode == Mode::Player { Mode::Remote } else { Mode::Player };
                self.note = match self.mode {
                    Mode::Remote => "remote mode: arrows move, Enter = OK".into(),
                    Mode::Player => "player mode: arrows seek and change volume".into(),
                };
            }
            Input::ToggleHelp => self.overlay = Overlay::Help,
            Input::ToggleInfo => self.info = !self.info,
            Input::OpenSearch => self.open_search(None),
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
                self.note = format!("{}...", self.menu.get(i).map_or("", |m| m.label));
                let _ = actions.send(a);
            }
            (Overlay::Prompt(_) | Overlay::Search(_), Overlay::None, None) => self.note = "cancelled".into(),
            (_, _, Some(Action::Search { pkg, query, keys })) => {
                let name = apps::by_package(&pkg).map_or_else(|| pkg.clone(), |a| list_name(a.name));
                self.note = if keys { format!("typing \"{query}\" into {name}...") } else { format!("searching {name}...") };
                let _ = actions.send(Action::Search { pkg, query, keys });
            }
            (_, _, Some(Action::Launch(target))) => {
                self.note = format!("launching {}...", apps::pkg_of(&target));
                let _ = actions.send(Action::Launch(target));
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
            (Overlay::Apps { tab, .. }, Some(Hit::AppItem(i))) => {
                let rows = tab_list(self.apps.as_deref().unwrap_or_default(), tab);
                let target = rows.get(i).map(|e| e.target.clone());
                if let Some(target) = target {
                    self.apply_overlay(Overlay::None, Some(Action::Launch(target)), actions);
                }
            }
            (Overlay::Apps { .. }, Some(Hit::Popup | Hit::AppTab(_))) => {}
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
            Overlay::None => {
                let _ = actions.send(Action::Volume(if up { 1 } else { -1 }));
            }
            _ => {}
        }
    }

    fn draw(&mut self, f: &mut Frame) {
        self.hits.clear();
        let outer = Block::bordered()
            .title(Line::from(format!(" tvbee ui - {} ", self.ip)).bold())
            .title(
                Line::from(match self.mode {
                    Mode::Player => " player ",
                    Mode::Remote => " remote ",
                })
                .right_aligned()
                .style(Style::new().fg(Color::Black).bg(match self.mode {
                    Mode::Player => Color::Cyan,
                    Mode::Remote => Color::Magenta,
                })),
            )
            .border_style(Style::new().fg(Color::DarkGray));
        let area = outer.inner(f.area());
        f.render_widget(outer, f.area());

        let bs = buttons(self.mode, self.snap.as_ref().is_some_and(|s| s.exact));
        let places = flow(&bs, area.width);
        let rows = places.iter().map(|(r, _)| r + 1).max().unwrap_or(1);
        let [body, _, footer, status] = Layout::vertical([
            Constraint::Min(4),
            Constraint::Length(1),
            Constraint::Length(rows),
            Constraint::Length(1),
        ])
        .areas(area);

        let body = if self.info {
            let want = self.panel_height();
            // keep at least 4 rows for what's playing
            let h = want.min(body.height.saturating_sub(4));
            let [top, panel] = Layout::vertical([Constraint::Min(4), Constraint::Length(h)]).areas(body);
            self.draw_panel(f, panel);
            top
        } else {
            body
        };
        self.draw_body(f, body);

        // footer: one clickable button per action
        for (b, (row, x)) in bs.iter().zip(places) {
            let rect = ratatui::layout::Rect { x: footer.x + x, y: footer.y + row, width: button_width(b), height: 1 };
            if rect.y >= footer.y + footer.height || rect.x + rect.width > footer.x + footer.width {
                continue;
            }
            let line = Line::from(vec![
                Span::styled(format!(" {} ", b.key), Style::new().fg(Color::Black).bg(Color::Gray)),
                Span::styled(format!(" {}", b.label), Style::new().fg(Color::Gray)),
            ]);
            f.render_widget(Paragraph::new(line), rect);
            self.hits.push((rect, Hit::Input(b.input.clone())));
        }

        let line = match &self.error {
            Some(e) => Line::from(format!("! {e}")).style(Style::new().fg(Color::Red)),
            None => Line::from(self.note.clone()).style(Style::new().add_modifier(Modifier::ITALIC)),
        };
        f.render_widget(Paragraph::new(line), status);

        self.draw_overlay(f);
    }

    /// The what / title / progress / url / volume lines, each in its own rows so
    /// the bars' positions are known for clicks.
    fn draw_body(&mut self, f: &mut Frame, body: ratatui::layout::Rect) {
        const LABEL: u16 = 9;
        let width = usize::from(body.width);
        let label = |l: &str| Span::styled(format!("{l:<9}"), Style::new().fg(Color::DarkGray));
        // (line, what clicking it does)
        let mut lines: Vec<(Line, Option<Hit>)> = Vec::new();
        match &self.snap {
            None => lines.push((Line::from("waiting for the TV...").italic(), None)),
            Some(s) => {
                match &s.activity {
                    Some(a) => {
                        lines.push((Line::from(vec![label("what"), Span::raw(a.what.clone()).bold()]), None));
                        if let Some(t) = &a.title {
                            lines.push((Line::from(vec![label("title"), Span::raw(t.clone())]), None));
                        }
                        if let Some(mut p) = a.progress {
                            if !p.paused {
                                p.position += self.at.elapsed().as_secs_f64();
                                if let Some(d) = p.duration {
                                    p.position = p.position.min(d);
                                }
                            }
                            // leave room for the label and " 1:02:03 / 1:02:03 (100%)"
                            let w = width.saturating_sub(9 + 26).max(10);
                            let hit = p.duration.map(|_| Hit::Progress {
                                x0: body.x + LABEL,
                                width: u16::try_from(w).unwrap_or(u16::MAX),
                            });
                            let bar_line = Line::from(vec![
                                label("progress"),
                                Span::styled(progress_bar(&p, w), Style::new().fg(Color::Cyan)),
                            ]);
                            lines.push((bar_line, hit));
                        }
                        if let Some(u) = &a.url {
                            lines.push((Line::from(vec![label("url"), Span::raw(u.clone()).underlined()]), None));
                        }
                    }
                    None => lines.push((Line::from("nothing playing that tvbee recognises").italic(), None)),
                }
                if s.standby {
                    lines.push((Line::from("press . then w to wake the TV").fg(Color::Yellow), None));
                }
                lines.push((Line::default(), None));
                let vol_w = width.saturating_sub(9 + 8).clamp(10, 40);
                let (vol, hit) = match (s.volume, s.muted) {
                    (_, Some(true)) => (
                        Span::styled("muted (click to unmute)", Style::new().fg(Color::Yellow)),
                        Some(Hit::Input(Input::Do(Action::Mute))),
                    ),
                    (Some((v, max)), _) if max > 0 => (
                        Span::styled(
                            format!("{} {v}/{max}", bar(f64::from(v) / f64::from(max), vol_w)),
                            Style::new().fg(Color::Green),
                        ),
                        Some(Hit::Volume { x0: body.x + LABEL, width: u16::try_from(vol_w).unwrap_or(u16::MAX) }),
                    ),
                    _ => (Span::raw("unknown"), None),
                };
                lines.push((Line::from(vec![label("volume"), vol]), hit));
            }
        }
        let mut y = body.y;
        for (line, hit) in lines {
            let para = Paragraph::new(line).wrap(Wrap { trim: false });
            let h = u16::try_from(para.line_count(body.width).max(1)).unwrap_or(1);
            if y + h > body.y + body.height {
                break;
            }
            let rect = ratatui::layout::Rect { x: body.x, y, width: body.width, height: h };
            f.render_widget(para, rect);
            if let Some(hit) = hit {
                let r = match hit {
                    Hit::Progress { x0, width } | Hit::Volume { x0, width } => {
                        ratatui::layout::Rect { x: x0, y, width, height: 1 }
                    }
                    _ => rect,
                };
                self.hits.push((r, hit));
            }
            y += h;
        }
    }

    fn panel_disks(&self) -> Vec<(&'static str, info::Disk)> {
        self.snap.as_ref().map_or_else(Vec::new, |s| {
            info::interesting_disks(&s.system.disks).into_iter().map(|(l, d)| (l, d.clone())).collect()
        })
    }

    /// Rows the system panel wants, borders included.
    fn panel_height(&self) -> u16 {
        // power, app, memory text, graph (3), swap, disks..., load
        let disks = u16::try_from(self.panel_disks().len()).unwrap_or(0);
        2 + 1 + 1 + 1 + GRAPH_ROWS + 1 + disks + 1
    }

    /// The system panel: power, app, memory graph, swap, storage, load.
    fn draw_panel(&self, f: &mut Frame, area: ratatui::layout::Rect) {
        let block = Block::bordered()
            .title(" system - i hides ")
            .border_style(Style::new().fg(Color::DarkGray));
        let inner = block.inner(area);
        f.render_widget(block, area);
        let Some(s) = &self.snap else { return };
        let sys = &s.system;
        let label = |l: &str| Span::styled(format!("{l:<9}"), Style::new().fg(Color::DarkGray));
        let width = usize::from(inner.width);
        let bar_w = width.saturating_sub(9 + 34).clamp(10, 40);
        let gauge = |frac: f64, text: String| {
            let hot = frac >= 0.9;
            let colour = if hot { Color::Red } else { Color::Green };
            vec![Span::styled(bar(frac, bar_w), Style::new().fg(colour)), Span::raw(format!(" {text}"))]
        };

        let mut rows: Vec<(Line, u16)> = Vec::new();
        let uptime = sys.uptime_secs.map_or(String::new(), |u| format!("   up {}", info::human_uptime(u)));
        rows.push((Line::from(vec![label("power"), Span::raw(format!("{}{uptime}", s.power.as_deref().unwrap_or("?")))]), 1));
        // the kind first: the window name is long and gets cut off
        let kind = s.kind.as_ref().map_or(String::new(), |k| format!("[{k}] "));
        rows.push((
            Line::from(vec![
                label("app"),
                Span::styled(kind, Style::new().fg(Color::Cyan)),
                Span::raw(s.focus.clone().unwrap_or_default()),
            ]),
            1,
        ));
        match sys.mem {
            Some(m) => {
                let mb = |k: u64| k / 1024;
                let text = format!(
                    "{} MB used of {} MB ({} MB available)",
                    mb(m.used()),
                    mb(m.total),
                    mb(m.available)
                );
                rows.push((Line::from(vec![label("memory"), Span::raw(text)]), 1));
                rows.push((Line::default(), GRAPH_ROWS)); // the graph goes here
                let swap = if m.swap_total > 0 {
                    gauge(m.swap_used() as f64 / m.swap_total as f64, format!("{}/{} MB", mb(m.swap_used()), mb(m.swap_total)))
                } else {
                    vec![Span::raw("none")]
                };
                rows.push((Line::from([vec![label("swap")], swap].concat()), 1));
            }
            None => rows.push((Line::from(vec![label("memory"), Span::raw("unknown")]), 1)),
        }
        for (name, d) in self.panel_disks() {
            let frac = if d.total_kb > 0 { d.used_kb as f64 / d.total_kb as f64 } else { 0.0 };
            let text = info::disk_summary(name, &d);
            rows.push((Line::from([vec![label("storage")], gauge(frac, text)].concat()), 1));
        }
        if let Some((a, b, c)) = sys.load {
            let cores = sys.cores.map_or(String::new(), |n| format!(" ({n} cores)"));
            rows.push((Line::from(vec![label("load"), Span::raw(format!("{a:.2} {b:.2} {c:.2}{cores}"))]), 1));
        }

        let mut y = inner.y;
        for (line, h) in rows {
            if y + h > inner.y + inner.height {
                break;
            }
            let rect = ratatui::layout::Rect { x: inner.x, y, width: inner.width, height: h };
            if h == GRAPH_ROWS {
                self.draw_mem_graph(f, rect, sys.mem.map_or(0, |m| m.total / 1024));
            } else {
                f.render_widget(Paragraph::new(line), rect);
            }
            y += h;
        }
    }

    /// Memory used over time, newest on the right, scaled to total RAM.
    fn draw_mem_graph(&self, f: &mut Frame, rect: ratatui::layout::Rect, total_mb: u64) {
        let x = rect.x + 9;
        let w = rect.width.saturating_sub(9 + 1);
        let n = usize::from(w).min(self.mem_history.len());
        let recent = &self.mem_history[self.mem_history.len() - n..];
        let hot = recent.last().is_some_and(|u| total_mb > 0 && *u * 10 >= total_mb * 9);
        let graph = ratatui::widgets::Sparkline::default()
            .data(recent)
            .max(total_mb.max(1))
            .style(Style::new().fg(if hot { Color::Yellow } else { Color::Cyan }));
        f.render_widget(graph, ratatui::layout::Rect { x, y: rect.y, width: w, height: rect.height });
        let note = format!("last {}s", recent.len());
        f.render_widget(
            Paragraph::new(Line::from(note).fg(Color::DarkGray)),
            ratatui::layout::Rect { x: rect.x, y: rect.y + rect.height - 1, width: 9.min(rect.width), height: 1 },
        );
    }

    fn draw_overlay(&mut self, f: &mut Frame) {
        match self.overlay {
            Overlay::None => {}
            Overlay::Help => {
                let text: Vec<Line> = HELP.lines().map(Line::from).collect();
                let r = popup(f, " keys - any key closes ", text, Color::DarkGray);
                self.hits.push((r, Hit::Popup));
            }
            Overlay::Menu(sel) => {
                let (text, items) = self.menu_lines(sel);
                let r = popup(f, " more - esc closes ", text, Color::Cyan);
                self.hits.push((r, Hit::Popup));
                for (row, item) in items.into_iter().enumerate() {
                    if let Some(i) = item {
                        let y = r.y + 1 + u16::try_from(row).unwrap_or(0);
                        self.hits.push((ratatui::layout::Rect { x: r.x + 1, y, width: r.width.saturating_sub(2), height: 1 }, Hit::MenuItem(i)));
                    }
                }
            }
            Overlay::Apps { tab, sel } => {
                let pkg = self.snap.as_ref().and_then(|s| s.focus.as_deref()).map(|f| f.split('/').next().unwrap_or(f).to_string());
                let all = self.apps.as_deref().unwrap_or_default();
                let list = tab_list(all, tab);
                // the tab labels, with counts
                let labels: Vec<(AppTab, String)> = [AppTab::Known, AppTab::Other]
                    .into_iter()
                    .map(|t| {
                        let n = all.iter().filter(|e| t.holds(e)).count();
                        let name = if t == AppTab::Known { "Apps" } else { "Other TV apps" };
                        (t, format!(" {name} ({n}) "))
                    })
                    .collect();
                let tab_line = Line::from(
                    labels
                        .iter()
                        .flat_map(|(t, l)| {
                            let st = if *t == tab {
                                Style::new().fg(Color::Black).bg(Color::Green).bold()
                            } else {
                                Style::new().fg(Color::Gray)
                            };
                            [Span::styled(l.clone(), st), Span::raw(" ")]
                        })
                        .collect::<Vec<_>>(),
                );
                let mut rows: Vec<Line> = Vec::new();
                if self.apps.is_none() {
                    rows.push(Line::from("looking for installed apps...").italic());
                } else if list.is_empty() {
                    rows.push(Line::from("none on this TV").italic());
                }
                // wide enough for the other tab too, so switching doesn't resize the box
                let wide = all.iter().map(|e| e.name.chars().count()).max().unwrap_or(0).min(40);
                for (i, e) in list.iter().enumerate() {
                    let on = if Some(e.package.as_str()) == pkg.as_deref() { "  (on now)" } else { "" };
                    let extra = e.alias.or(e.tag).unwrap_or("");
                    // fixed columns, so both tabs draw the same width
                    let line = Line::from(format!("  {:<wide$}  {extra:<12}{on:<10}", e.name));
                    rows.push(if i == sel { line.reversed() } else { line });
                }
                // scroll so the highlighted row stays in view
                let room = usize::from(f.area().height.saturating_sub(10)).max(3);
                let start = sel.saturating_sub(room.saturating_sub(1)).min(rows.len().saturating_sub(room));
                let scrolls = rows.len() > room;
                let mut shown: Vec<Line> = rows.into_iter().skip(start).take(room).collect();
                let shown_n = if list.is_empty() { 0 } else { shown.len() };
                // pad to the taller tab, so switching doesn't move the box (or its tabs)
                let tallest = [AppTab::Known, AppTab::Other].iter().map(|t| all.iter().filter(|e| t.holds(e)).count()).max().unwrap_or(0);
                while shown.len() < tallest.min(room) {
                    shown.push(Line::default());
                }
                let mut text = vec![tab_line, Line::default()];
                text.extend(shown);
                text.push(Line::default());
                let more = if scrolls { "  (scrolls)" } else { "" };
                text.push(Line::from(format!("↑/↓ choose  ⏎ launch  / search it  letter = jump  tab/←/→ switch{more}")).fg(Color::DarkGray));
                text.push(Line::from("click a tab or an app   esc closes").fg(Color::DarkGray));
                let r = popup(f, " launch an app ", text, Color::Green);
                self.hits.push((r, Hit::Popup));
                // tab labels on the first row
                let mut x = r.x + 1;
                for (t, l) in &labels {
                    let w = u16::try_from(l.chars().count()).unwrap_or(0);
                    self.hits.push((ratatui::layout::Rect { x, y: r.y + 1, width: w, height: 1 }, Hit::AppTab(*t)));
                    x += w + 1;
                }
                // app rows start after the tabs and a blank line
                {
                    for row in 0..shown_n {
                        let y = r.y + 3 + u16::try_from(row).unwrap_or(0);
                        let width = r.width.saturating_sub(2);
                        self.hits.push((ratatui::layout::Rect { x: r.x + 1, y, width, height: 1 }, Hit::AppItem(start + row)));
                    }
                }
            }
            Overlay::Search(at) => {
                let field = 60usize;
                let n = self.prompt.chars().count();
                let shown: String = self.prompt.chars().skip(n.saturating_sub(field - 1)).collect();
                let target = self.search_targets.get(at);
                let app_name = target.map_or("(no app in front)".to_string(), |t| t.name.clone());
                let how = match target.map(|t| t.method) {
                    Some("keys") => "no search of its own: tvbee will press its search key and type (best effort)",
                    Some("results link") => "opens its search results",
                    Some(_) => "uses the app's own search",
                    None => "",
                };
                let mut text = vec![
                    Line::from(vec![
                        Span::raw("Search in  "),
                        Span::styled(format!(" {app_name} "), Style::new().fg(Color::Black).bg(Color::Cyan).bold()),
                        Span::styled("   tab / ↑↓ change app", Style::new().fg(Color::DarkGray)),
                    ]),
                    Line::from(how).fg(if target.is_some_and(|t| t.method == "keys") { Color::Yellow } else { Color::DarkGray }),
                    Line::default(),
                    Line::from(vec![
                        Span::styled(format!("{shown}█"), Style::new().fg(Color::White).bg(Color::DarkGray)),
                        Span::styled(" ".repeat(field.saturating_sub(shown.chars().count() + 1)), Style::new().bg(Color::DarkGray)),
                    ]),
                    Line::default(),
                ];
                if self.searchable.is_none() {
                    text.push(Line::from("(still checking which apps can search...)").italic());
                }
                text.push(Line::from("⏎ search   esc cancel   paste works   ctrl-u clears").fg(Color::DarkGray));
                let r = popup(f, " search ", text, Color::Cyan);
                self.hits.push((r, Hit::Popup));
            }
            Overlay::Prompt(item) => {
                let Some(it) = self.menu.get(item) else { return };
                let Run::WithText { prompt, hint, .. } = it.run else { return };
                let field = 60usize;
                // show the end of long input
                let shown: String = {
                    let n = self.prompt.chars().count();
                    self.prompt.chars().skip(n.saturating_sub(field - 1)).collect()
                };
                let text = vec![
                    Line::from(prompt),
                    Line::from(vec![
                        Span::styled(format!("{shown}█"), Style::new().fg(Color::White).bg(Color::DarkGray)),
                        Span::styled(" ".repeat(field.saturating_sub(shown.chars().count() + 1)), Style::new().bg(Color::DarkGray)),
                    ]),
                    Line::from(hint).fg(Color::DarkGray),
                    Line::default(),
                    Line::from("⏎ go   esc cancel   paste works   ctrl-u clears").fg(Color::DarkGray),
                ];
                let r = popup(f, &format!(" {} ", it.label), text, Color::Yellow);
                self.hits.push((r, Hit::Popup));
            }
            Overlay::Confirm { item, yes } => {
                let Some(it) = self.menu.get(item) else { return };
                let mut text: Vec<Line> = it.confirm.unwrap_or("Are you sure?").lines().map(Line::from).collect();
                text.push(Line::default());
                let answers_row = u16::try_from(text.len()).unwrap_or(0);
                let pick = |label: &'static str, on: bool| {
                    let st = if on { Style::new().fg(Color::Black).bg(Color::Red).bold() } else { Style::new().reversed() };
                    Span::styled(format!(" {label} "), st)
                };
                text.push(Line::from(vec![pick("No", !yes), Span::raw("   "), pick("Yes", yes)]));
                text.push(Line::from("y = yes   ←/→ choose   ⏎ confirm   any other key cancels").fg(Color::DarkGray));
                let r = popup(f, &format!(" {} ", it.label), text, Color::Red);
                self.hits.push((r, Hit::Popup));
                let y = r.y + 1 + answers_row;
                // " No " then 3 spaces then " Yes "
                self.hits.push((ratatui::layout::Rect { x: r.x + 1, y, width: 4, height: 1 }, Hit::ConfirmNo));
                self.hits.push((ratatui::layout::Rect { x: r.x + 8, y, width: 5, height: 1 }, Hit::ConfirmYes));
            }
        }
    }

    /// Items grouped under their plugin, the highlighted one reversed; and for
    /// each line, the item it shows (None for headings).
    fn menu_lines(&self, sel: usize) -> (Vec<Line<'static>>, Vec<Option<usize>>) {
        let (mut lines, mut items) = (Vec::new(), Vec::new());
        let mut group = "";
        for (i, it) in self.menu.iter().enumerate() {
            if it.group != group {
                group = it.group;
                lines.push(Line::from(group).bold());
                items.push(None);
            }
            let dots = if it.confirm.is_some() || matches!(it.run, Run::WithText { .. }) { "..." } else { "" };
            let line = Line::from(format!("  {}  {}{dots}", it.key, it.label));
            lines.push(if i == sel { line.reversed() } else { line });
            items.push(Some(i));
        }
        lines.push(Line::default());
        lines.push(Line::from("↑/↓ choose  ⏎ run  press the key, or click").fg(Color::DarkGray));
        (lines, items)
    }
}

/// A bordered box centred over the screen, sized to its text. Returns where.
fn popup(f: &mut Frame, title: &str, text: Vec<Line>, border: Color) -> ratatui::layout::Rect {
    let h = u16::try_from(text.len()).unwrap_or(u16::MAX).saturating_add(2);
    let widest = text.iter().map(Line::width).max().unwrap_or(0).max(title.chars().count());
    let w = u16::try_from(widest).unwrap_or(u16::MAX).saturating_add(4);
    let area = f.area();
    let rect = ratatui::layout::Rect {
        x: area.x + area.width.saturating_sub(w) / 2,
        y: area.y + area.height.saturating_sub(h) / 2,
        width: w.min(area.width),
        height: h.min(area.height),
    };
    f.render_widget(ratatui::widgets::Clear, rect);
    f.render_widget(
        Paragraph::new(text).block(Block::bordered().title(title.to_string()).border_style(Style::new().fg(border))),
        rect,
    );
    rect
}

const HELP: &str = " everywhere
   space p  play/pause       ⌫      back
   h        home             [ ]    seek -/+ 60s
   0-9      jump to 0%-90%   + -    volume
   m        mute/unmute      r      refresh
   tab      switch mode      .      more (power, mirror, ...)
   i        system info      a      launch an app
   /        search the app in front (tab picks another app)
   . o      open a link (YouTube, Netflix, media files)
   q Esc    quit

 player mode
   ← →      seek -/+ 10s (exact in YouTube; ff/rew keys elsewhere)
   ↑ ↓      volume

 remote mode
   arrows   move around the TV's menus
   ⏎        OK

 mouse
   click the buttons, the progress bar (jump) or volume bar (set)
   wheel    volume
   Shift+drag selects text as usual (to copy the url)";

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
    loop {
        for m in msgs.try_iter() {
            app.handle(m);
        }
        term.draw(|f| app.draw(f))?;
        // short poll: keeps the progress bar moving and keys snappy
        if !event::poll(Duration::from_millis(200))? {
            continue;
        }
        match event::read()? {
            Event::Key(k) if k.kind != KeyEventKind::Release => {
                if let Overlay::Search(at) = app.overlay {
                    let mut text = std::mem::take(&mut app.prompt);
                    let (next, action) = search_key(at, k.code, k.modifiers, &mut text, &app.search_targets);
                    app.prompt = text;
                    app.apply_overlay(next, action, actions);
                } else if let (Overlay::Apps { tab, sel }, KeyCode::Char('/')) = (app.overlay, k.code) {
                    // search the highlighted app
                    let rows = tab_list(app.apps.as_deref().unwrap_or_default(), tab);
                    let pkg = rows.get(sel).map(|e| e.package.clone());
                    app.open_search(pkg);
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
                if let Overlay::Prompt(_) | Overlay::Search(_) = app.overlay {
                    app.prompt.push_str(&paste_text(&p));
                }
            }
            Event::Mouse(m) => match m.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    if app.click(m.column, m.row, actions) {
                        return Ok(());
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
    let mut mouse = true;
    for a in args {
        match a.as_str() {
            "--no-mouse" => mouse = false,
            _ => return crate::error::usage(format!("usage: tvbee ui [--no-mouse]  (unexpected '{a}')")),
        }
    }
    use std::io::IsTerminal;
    if !std::io::stdout().is_terminal() {
        return crate::error::usage("ui needs a terminal");
    }
    let (act_tx, act_rx) = mpsc::channel();
    let (msg_tx, msg_rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let worker = Worker {
        tv: Tv { ip: c.tv.ip.clone() },
        conn: None,
        pkg: None,
        stop: Arc::clone(&stop),
        menu: c.menu.clone(),
        guesses: Vec::new(),
        searchable: Vec::new(),
    };
    std::thread::spawn(move || worker.run(act_rx, msg_tx));
    let _ = act_tx.send(Action::Refresh);
    let mut app = App {
        ip: c.tv.ip.clone(),
        snap: None,
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
        searchable: None,
        search_targets: Vec::new(),
    };
    // ratatui::run sets up the terminal, restores it afterwards (and on panic)
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
    Ok(result?)
}

pub fn commands() -> Vec<Command> {
    vec![Command::new(CAT, "ui", "[--no-mouse]", "interactive remote: what's on, volume, seeking", ui).long(
        "A full-screen remote showing what is playing (as 'tvbee nowplaying') and the\n\
         volume, refreshed every second. Two modes, switched with Tab: in player mode\n\
         the arrows seek and change volume; in remote mode they move around the TV's\n\
         menus and Enter is OK.\n\
         \n\
         \x20 space / p     play/pause   Backspace  back   h  home\n\
         \x20 left/right    seek 10s     [ / ]  seek 60s     0-9  jump to 0%-90%\n\
         \x20 up/down + -   volume       m      mute/unmute\n\
         \x20 r  refresh    .  more: power, mirror, screenshot, inputs (risky ones ask first)\n\
         \x20 i  system panel: memory graph, swap, storage, load, uptime\n\
         \x20 a  launcher: the installed apps tvbee has aliases for (see 'tvbee apps')\n\
         \x20 /  search the app in front, or (in the launcher) the highlighted app\n\
         \x20 ?  all keys   q / Esc / Ctrl-C  quit\n\
         \n\
         Seeking is exact for apps tvbee can read the video of (YouTube); elsewhere\n\
         left/right send fast-forward/rewind key presses (~10s each, apps differ)\n\
         and number keys do nothing.\n\
         \n\
         The mouse works too: click the buttons along the bottom, the progress bar\n\
         (jump there) or the volume bar (set that level); the wheel changes the\n\
         volume. Hold Shift to select text as usual. --no-mouse turns this off.",
    )]
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(k(KeyCode::Char('h'), Mode::Remote), Input::Do(Action::Key(KEY_HOME)));
        assert_eq!(k(KeyCode::Char('5'), Mode::Remote), Input::Do(Action::Jump(0.5)));
        assert_eq!(k(KeyCode::Tab, Mode::Player), Input::ToggleMode);
        assert_eq!(k(KeyCode::Char('x'), Mode::Player), Input::Nothing);
    }

    fn items() -> Vec<MenuItem> {
        fn ok(_: &Tv) -> Result<String> {
            Ok(String::new())
        }
        vec![
            MenuItem::new("power", 's', "sleep", ok),
            MenuItem::new("power", 'X', "power off", ok).confirm("sure?"),
        ]
    }

    #[test]
    fn menu_keys() {
        let it = items();
        let k = |o, c| overlay_key(o, c, &it);
        // a plain item runs straight away; a risky one asks first
        assert_eq!(k(Overlay::Menu(0), KeyCode::Char('s')), (Overlay::None, Some(Action::Menu(0))));
        assert_eq!(k(Overlay::Menu(0), KeyCode::Char('X')), (Overlay::Confirm { item: 1, yes: false }, None));
        assert_eq!(k(Overlay::Menu(1), KeyCode::Enter), (Overlay::Confirm { item: 1, yes: false }, None));
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
        let ask = Overlay::Confirm { item: 1, yes: false };
        assert_eq!(k(ask, KeyCode::Enter), (Overlay::None, None));
        assert_eq!(k(ask, KeyCode::Char('n')), (Overlay::None, None));
        assert_eq!(k(ask, KeyCode::Char(' ')), (Overlay::None, None));
        assert_eq!(k(ask, KeyCode::Char('y')), (Overlay::None, Some(Action::Menu(1))));
        let yes = Overlay::Confirm { item: 1, yes: true };
        assert_eq!(k(ask, KeyCode::Right), (yes, None));
        assert_eq!(k(yes, KeyCode::Enter), (Overlay::None, Some(Action::Menu(1))));
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
            let bs = buttons(mode, true);
            let mut keys: Vec<_> = bs.iter().map(|b| b.key).collect();
            keys.sort();
            keys.dedup();
            assert_eq!(keys.len(), bs.len(), "{mode:?}");
            // narrow terminal: several rows, and nothing past the edge
            let places = flow(&bs, 40);
            assert!(places.iter().any(|(r, _)| *r > 0));
            for (b, (_, x)) in bs.iter().zip(&places) {
                assert!(x + button_width(b) <= 40, "{} overflows", b.label);
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
        search::parse_searchable(
            "    org.xbmc.kodi/.XBMCSearchableActivity\n@@\n@@\n",
            Some("package:com.google.android.youtube.tv\n"),
        )
    }

    #[test]
    fn search_box_targets() {
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
            assert_eq!(prompt_key(3, KeyCode::Char(c), none, &mut t), (Overlay::Prompt(3), None));
        }
        assert_eq!(prompt_key(3, KeyCode::Backspace, none, &mut t), (Overlay::Prompt(3), None));
        assert_eq!(t, "youtu.be/");
        assert_eq!(prompt_key(3, KeyCode::Char('u'), KeyModifiers::CONTROL, &mut t), (Overlay::Prompt(3), None));
        assert_eq!(t, "");
        // Enter on nothing stays put; with text it runs the item
        assert_eq!(prompt_key(3, KeyCode::Enter, none, &mut t), (Overlay::Prompt(3), None));
        t.push_str("  vlc http://h/f.mkv ");
        assert_eq!(prompt_key(3, KeyCode::Enter, none, &mut t), (Overlay::None, Some(Action::MenuText(3, "vlc http://h/f.mkv".into()))));
        assert_eq!(prompt_key(3, KeyCode::Esc, none, &mut t), (Overlay::None, None));
        assert_eq!(paste_text("https://youtu.be/x\r\n"), "https://youtu.be/x");
        assert_eq!(paste_text("vlc\thttp://h/f.mkv"), "vlc http://h/f.mkv");
    }

    #[test]
    fn text_items_open_a_prompt() {
        fn ok(_: &Tv, _: &str) -> Result<String> {
            Ok(String::new())
        }
        let items = vec![MenuItem::with_text("apps", 'o', "open a link", "Link:", "", ok)];
        assert_eq!(pick(&items, 0), (Overlay::Prompt(0), None));
        assert_eq!(overlay_key(Overlay::Menu(0), KeyCode::Char('o'), &items), (Overlay::Prompt(0), None));
    }

    fn rows(pm: &str, guessed: &[apps::Guess]) -> Vec<Entry> {
        launcher_entries(&apps::installed_aliases(pm), guessed)
    }

    #[test]
    fn launcher_keys() {
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
        for key in [KeyCode::Tab, KeyCode::Left, KeyCode::Right] {
            assert_eq!(k(2, key), (Overlay::Apps { tab: AppTab::Other, sel: 0 }, None));
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
    fn launcher_lists_other_tv_apps() {
        let g = |p: &str, k, c: Option<&str>| apps::Guess { package: p.into(), kind: k, component: c.map(Into::into) };
        let guessed = vec![
            g("nextapp.fx", apps::Kind::Player, Some("nextapp.fx/.ui.Launcher")),
            g("com.example.game", apps::Kind::Other, Some("com.example.game/.Main")),
            // already listed by alias: not repeated
            g("org.videolan.vlc", apps::Kind::Player, Some("org.videolan.vlc/.StartActivity")),
            // not in the TV launcher: can't be launched from here
            g("com.android.tv.settings", apps::Kind::Launcher, None),
            // in tvbee's table without an alias: its kind, but a specific name
            g("com.spocky.projengmenu", apps::Kind::Launcher, Some("com.spocky.projengmenu/.ui.home.MainActivity")),
            g("org.droidtv.nettvbrowser", apps::Kind::Other, Some("org.droidtv.nettvbrowser/.Main")),
        ];
        let all = rows("package:org.videolan.vlc\n", &guessed);
        let shown = |tab| tab_list(&all, tab).iter().map(|e| (e.name.clone(), e.alias.or(e.tag))).collect::<Vec<_>>();
        assert_eq!(shown(AppTab::Known), vec![("Home screen".to_string(), Some("home")), ("VLC".into(), Some("vlc"))]);
        assert_eq!(
            shown(AppTab::Other),
            vec![
                ("com.example.game".to_string(), None),
                ("com.spocky.projengmenu".into(), Some("home")),
                ("nextapp.fx".into(), Some("player")),
                ("org.droidtv.nettvbrowser".into(), Some("video")),
            ]
        );
        // other apps launch by their TV-launcher activity, and jump by name
        let other = tab_list(&all, AppTab::Other);
        assert_eq!(apps_key(AppTab::Other, 0, KeyCode::Char('n'), &other), (Overlay::Apps { tab: AppTab::Other, sel: 2 }, None));
        assert_eq!(
            apps_key(AppTab::Other, 2, KeyCode::Enter, &other),
            (Overlay::None, Some(Action::Launch("nextapp.fx/.ui.Launcher".into())))
        );
    }

    #[test]
    fn batches_keys_in_order() {
        use Action::*;
        assert_eq!(
            batches(vec![Key(20), Key(20), Key(23), PlayPause, Key(4)]),
            vec![Batch::Keys(vec![20, 20, 23]), Batch::One(PlayPause), Batch::Keys(vec![4])]
        );
        assert_eq!(key_name(4), "back");
        assert_eq!(key_name(20), "down");
    }

    #[test]
    fn coalesces_repeats() {
        use Action::*;
        assert_eq!(
            coalesce(vec![Seek(10.0), Seek(10.0), Volume(1), Volume(1), Volume(-1), Refresh, Refresh, Seek(-60.0)]),
            vec![Seek(20.0), Volume(1), Refresh, Seek(-60.0)]
        );
    }
}

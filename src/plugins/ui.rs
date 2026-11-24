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

pub const KEY_BACK: u32 = 4;
pub const KEY_HOME: u32 = 3;
const DPAD: [(KeyCode, u32, &str); 4] =
    [(KeyCode::Up, 19, "up"), (KeyCode::Down, 20, "down"), (KeyCode::Left, 21, "left"), (KeyCode::Right, 22, "right")];
pub const KEY_OK: u32 = 23;
/// Delete (backspace), forward delete, page up / down.
pub const KEY_DEL: u32 = 67;
pub const KEY_FORWARD_DEL: u32 = 112;
pub const KEY_PAGE_UP: u32 = 92;
pub const KEY_PAGE_DOWN: u32 = 93;

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
    OpenGuide,
    OpenOptions,
    Do(Action),
    Nothing,
}

pub fn key_name(code: u32) -> &'static str {
    match code {
        KEY_BACK => "back",
        KEY_HOME => "home",
        KEY_OK => "ok",
        KEY_DEL => "delete",
        KEY_FORWARD_DEL => "forward delete",
        KEY_PAGE_UP => "page up",
        KEY_PAGE_DOWN => "page down",
        c => DPAD.iter().find(|(_, k, _)| *k == c).map_or("key", |(_, _, n)| n),
    }
}

/// Map a key to what it means in `mode`.
pub fn input_for(code: KeyCode, mods: KeyModifiers, mode: Mode) -> Input {
    use Action::*;
    let act = |a| Input::Do(a);
    match (code, mode) {
        (KeyCode::Char('c'), _) if mods.contains(KeyModifiers::CONTROL) => Input::Quit,
        (KeyCode::Tab, _) => Input::ToggleMode,
        // remote mode: the keyboard is the TV's (Tab leaves, Ctrl-C quits)
        (_, Mode::Remote) => remote_input(code, mods),
        (KeyCode::Char('q') | KeyCode::Esc, _) => Input::Quit,
        (KeyCode::Char('?'), _) => Input::ToggleHelp,
        (KeyCode::Char('.'), _) => Input::OpenMenu,
        (KeyCode::Char('i'), _) => Input::ToggleInfo,
        (KeyCode::Char('a'), _) => Input::OpenApps,
        (KeyCode::Char('g'), _) => Input::OpenGuide,
        (KeyCode::Char(','), _) => Input::OpenOptions,
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
        (KeyCode::Left, _) => act(Seek(-10.0)),
        (KeyCode::Right, _) => act(Seek(10.0)),
        (KeyCode::Up, _) => act(Volume(1)),
        (KeyCode::Down, _) => act(Volume(-1)),
        _ => Input::Nothing,
    }
}

/// A key in remote mode: arrows are the d-pad, Enter is OK, Esc is back,
/// and what you type is typed into the TV.
fn remote_input(code: KeyCode, mods: KeyModifiers) -> Input {
    let key = |k| Input::Do(Action::Key(k));
    match code {
        KeyCode::Enter => key(KEY_OK),
        KeyCode::Esc => key(KEY_BACK),
        KeyCode::Backspace => key(KEY_DEL),
        KeyCode::Delete => key(KEY_FORWARD_DEL),
        KeyCode::Home => key(KEY_HOME),
        KeyCode::PageUp => key(KEY_PAGE_UP),
        KeyCode::PageDown => key(KEY_PAGE_DOWN),
        // Android's `input text` takes plain ASCII
        KeyCode::Char(c) if !mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) && c.is_ascii() && !c.is_ascii_control() => {
            Input::Do(Action::Text(c.to_string()))
        }
        c => DPAD.iter().find(|(k, _, _)| *k == c).map_or(Input::Nothing, |(_, code, _)| key(*code)),
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
            // typing quickly: one round trip
            (Some(Action::Text(x)), Action::Text(y)) => x.push_str(y),
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
}

/// A channel in the guide pop-up, with its programmes for the next day.
#[derive(Debug, Clone, PartialEq)]
pub struct GuideRow {
    pub channel: crate::provider::Channel,
    /// Sorted by start.
    pub events: Vec<crate::provider::Event>,
}

impl GuideRow {
    /// What's on at time `t`, and what comes after it.
    pub fn at(&self, t: i64) -> (Option<&crate::provider::Event>, Option<&crate::provider::Event>) {
        let on = self.events.iter().find(|e| e.start <= t && t < e.end);
        let after = self.events.iter().find(|e| e.start >= on.map_or(t, |o| o.end));
        (on, after)
    }
}

/// The time `later` steps ahead of `now`: now itself, then on the hour and
/// half hour (19:30, 20:00, ...).
pub fn guide_time_at(now: i64, later: i64) -> i64 {
    if later == 0 { now } else { (now.div_euclid(GUIDE_STEP) + later) * GUIDE_STEP }
}

/// A run of the guide grid: a programme's edge, its start time, its title,
/// or empty time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seg {
    Edge,
    Time,
    Title,
    Gap,
}

/// One channel's row of the timeline grid: `width` cells from `start`,
/// `spc` seconds a cell. A programme is `│HH:MM Title` over the cells its
/// time covers; one that started before the left edge shows `◂Title`. Runs
/// of the programme starting at `focus` are flagged (to draw it marked).
pub fn grid_row(events: &[crate::provider::Event], start: i64, spc: i64, width: usize, offset: i64, focus: Option<i64>) -> Vec<(Seg, String, bool)> {
    let mut buf = vec![(' ', Seg::Gap, false); width];
    let end = start + spc * width as i64;
    let cell = |t: i64| (t - start).div_euclid(spc).clamp(0, width as i64) as usize;
    for e in events.iter().filter(|e| e.end > start && e.start < end) {
        let (x0, x1) = (cell(e.start), cell(e.end));
        if x1 <= x0 {
            continue;
        }
        let mut content: Vec<(char, Seg)> = Vec::new();
        if e.start >= start {
            content.push(('│', Seg::Edge));
            content.extend(format!("{} ", crate::plugins::guide::hhmm(e.start, offset)).chars().map(|c| (c, Seg::Time)));
        } else {
            content.push(('◂', Seg::Edge));
        }
        content.extend(e.title.chars().map(|c| (c, Seg::Title)));
        let w = x1 - x0;
        if content.len() > w && w > 2 {
            content.truncate(w - 1);
            content.push(('…', Seg::Title));
        }
        let marked = focus == Some(e.start);
        for (i, (c, seg)) in content.into_iter().take(w).enumerate() {
            buf[x0 + i] = (c, seg, marked);
        }
        // the whole block, spaces too
        if marked {
            for cell in &mut buf[x0..x1] {
                cell.2 = true;
            }
        }
    }
    let mut out: Vec<(Seg, String, bool)> = Vec::new();
    for (c, seg, marked) in buf {
        match out.last_mut() {
            Some((s, text, m)) if *s == seg && *m == marked => text.push(c),
            _ => out.push((seg, c.to_string(), marked)),
        }
    }
    out
}

/// What clicking part of the details panel does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailsClick {
    Open,
    Focus,
    Unfocus,
    Hide,
}

/// The details panel under the guide: hidden (one line), open, or open
/// with the keyboard in it (to scroll).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Details {
    Closed,
    Open,
    Focused,
}

/// Lines in the open details panel.
const DETAILS_ROWS: usize = 7;

/// The details panel's text for programme `e` on channel `ch`, wrapped to
/// `width`: title, then when/how long/channel/genre, then the description.
/// `info`: None while it's being read, Some(None) if the guide has none.
pub fn details_text(e: &crate::provider::Event, ch: &crate::provider::Channel, info: Option<Option<&str>>, width: usize, offset: i64) -> Vec<(String, Seg)> {
    use crate::plugins::guide::{day_hhmm, hhmm};
    let mins = (e.end - e.start) / 60;
    let length = if mins >= 60 { format!("{}h{:02}", mins / 60, mins % 60) } else { format!("{mins} min") };
    let mut meta = vec![format!("{}–{} ({length})", day_hhmm(e.start, offset), hhmm(e.end, offset)), ch.label()];
    meta.extend(e.genre_name().map(str::to_string));
    let mut out = vec![(e.title.clone(), Seg::Title), (meta.join(" · "), Seg::Time), (String::new(), Seg::Gap)];
    match info {
        Some(Some(info)) => {
            for para in info.split('\n') {
                out.extend(textwrap(para, width).into_iter().map(|l| (l, Seg::Gap)));
            }
        }
        Some(None) => out.push(("(no description in the broadcast guide)".into(), Seg::Time)),
        // still being read: empty, so the text just appears (the header says it's coming)
        None => {}
    }
    out
}

/// The time ruler over the grid: half-hour labels, and ▼ at `now`.
pub fn grid_ruler(start: i64, spc: i64, width: usize, now: i64, offset: i64) -> (String, Option<usize>) {
    let mut buf = vec![' '; width];
    let mut t = (start + GUIDE_STEP - 1).div_euclid(GUIDE_STEP) * GUIDE_STEP;
    while t < start + spc * width as i64 {
        let x = ((t - start) / spc) as usize;
        for (i, c) in crate::plugins::guide::hhmm(t, offset).chars().enumerate() {
            if x + i < width {
                buf[x + i] = c;
            }
        }
        t += GUIDE_STEP;
    }
    let marker = (now >= start && now < start + spc * width as i64).then(|| ((now - start) / spc) as usize);
    (buf.into_iter().collect(), marker)
}

/// Seconds a grid cell stands for, so about two hours fit.
pub fn grid_scale(width: usize) -> i64 {
    (7200 / width.max(1) as i64).clamp(40, 120)
}

/// How far ahead the guide goes (← → move through it in GUIDE_STEP steps).
const GUIDE_AHEAD: i64 = 24 * 3600;
const GUIDE_STEP: i64 = 30 * 60;

#[derive(Debug, Clone, PartialEq)]
pub struct Guide {
    pub provider: &'static str,
    /// "Freeview, United Kingdom (DVB-T, DVB-T2)", if the provider knows its network.
    pub place: Option<String>,
    /// How the network groups its channel numbers (for sub-headers).
    pub lineup: Option<&'static crate::lineups::Lineup>,
    pub rows: Vec<GuideRow>,
    /// The channel the TV is tuned to.
    pub current: Option<u32>,
    /// Favourite changes not on the TV yet.
    pub pending: std::collections::BTreeMap<u32, bool>,
    /// What syncing favourites found, for the status line.
    pub news: Option<String>,
}

/// The first provider with both channels and a guide.
fn live_provider() -> Option<&'static dyn crate::provider::Provider> {
    crate::provider::all().iter().copied().find(|p| p.channels().is_some() && p.guide().is_some())
}

fn load_guide(tv: &Tv) -> Result<Guide> {
    let Some(p) = live_provider() else { return Err(crate::error::Error::Failed("no live TV guide on this TV".into())) };
    let cx = crate::provider::Cx { tv };
    let (chans, guide) = (p.channels().expect("checked"), p.guide().expect("checked"));
    let (list, synced) = crate::favourites::channels(p, &cx)?;
    let now = crate::plugins::guide::now();
    let current = chans.current(&cx).ok().flatten().map(|c| c.id);
    let news = synced.as_ref().and_then(|s| s.news(&list));
    let pending = synced.map(|s| s.pending).unwrap_or_default();
    // (the guide is cached, so a schedule per channel is cheap)
    let rows = list
        .into_iter()
        .map(|channel| {
            let events = guide.schedule(&cx, channel.id, now, now + GUIDE_AHEAD + 3 * 3600).unwrap_or_default();
            GuideRow { channel, events }
        })
        .collect();
    let net = chans.network(&cx);
    let place = net.as_ref().map(crate::provider::describe);
    let lineup = net.as_ref().and_then(crate::lineups::find);
    Ok(Guide { provider: p.name(), place, lineup, rows, current, pending, news })
}

/// A line of the guide's channel list: a group's sub-header, or channel row N.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuideItem<'a> {
    Header(&'a str),
    Row(usize),
}

/// The lines to show of a list whose rows are in `groups` (each row's group,
/// None if ungrouped): a header where the group changes, `room` lines around
/// row `sel`. Scrolled into a group, its header stays at the top.
pub fn guide_items<'a>(groups: &[Option<&'a str>], sel: usize, room: usize) -> Vec<GuideItem<'a>> {
    let mut all = Vec::new();
    let mut last = None;
    for (i, g) in groups.iter().enumerate() {
        if g.is_some() && *g != last {
            all.push(GuideItem::Header(g.unwrap_or_default()));
        }
        last = *g;
        all.push(GuideItem::Row(i));
    }
    let at = all.iter().position(|it| *it == GuideItem::Row(sel)).unwrap_or(0);
    let start = at.saturating_sub(room / 2).min(all.len().saturating_sub(room));
    let mut out: Vec<GuideItem> = all.iter().copied().skip(start).take(room).collect();
    // the top line mid-group: show the group instead (not over the selected
    // row, and only if the group carries on below it)
    if let (Some(GuideItem::Row(i)), Some(GuideItem::Row(j)), true) = (out.first().copied(), out.get(1).copied(), start > 0) {
        if let (Some(g), true) = (groups[i], i != sel && groups[j] == groups[i]) {
            out[0] = GuideItem::Header(g);
        }
    }
    out
}

/// The guide pop-up's tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuideTab {
    All,
    Favourites,
    Radio,
}

impl GuideTab {
    pub const ALL: [GuideTab; 3] = [GuideTab::All, GuideTab::Favourites, GuideTab::Radio];

    /// The tab after this one (Tab cycles through them).
    pub fn next(self) -> GuideTab {
        match self {
            GuideTab::All => GuideTab::Favourites,
            GuideTab::Favourites => GuideTab::Radio,
            GuideTab::Radio => GuideTab::All,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            GuideTab::All => "All channels",
            GuideTab::Favourites => "Favourites",
            GuideTab::Radio => "Radio",
        }
    }

    pub fn holds(self, ch: &crate::provider::Channel) -> bool {
        match self {
            GuideTab::All => true,
            GuideTab::Favourites => ch.favourite,
            GuideTab::Radio => ch.radio,
        }
    }
}

/// Whether a guide row matches the filter: every word is the start of the
/// channel number or part of its name or of the programmes shown (on at
/// `t` and after), in any case.
pub fn guide_matches(r: &GuideRow, filter: &str, t: i64) -> bool {
    let num = r.channel.number.as_deref().unwrap_or("");
    let (on, after) = r.at(t);
    let text = [r.channel.name.as_deref(), on.map(|e| e.title.as_str()), after.map(|e| e.title.as_str())]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase();
    filter.to_lowercase().split_whitespace().all(|w| num.starts_with(w) || text.contains(w))
}

/// The rows a tab shows with this filter.
pub fn guide_shown<'a>(rows: &'a [GuideRow], tab: GuideTab, filter: &str, t: i64) -> Vec<&'a GuideRow> {
    rows.iter().filter(|r| tab.holds(&r.channel) && guide_matches(r, filter, t)).collect()
}

/// Where a movement key takes the highlight among `ids` (shown channel ids),
/// from channel `at` (if it's not shown, the first one).
pub fn guide_move(ids: &[u32], at: u32, code: KeyCode) -> u32 {
    let n = ids.len();
    let Some(i) = ids.iter().position(|&c| c == at) else { return ids.first().copied().unwrap_or(at) };
    let to = match code {
        KeyCode::Up => i.checked_sub(1).unwrap_or(n - 1),
        KeyCode::Down => (i + 1) % n,
        KeyCode::PageUp => i.saturating_sub(10),
        KeyCode::PageDown => (i + 10).min(n - 1),
        KeyCode::Home => 0,
        KeyCode::End => n - 1,
        _ => i,
    };
    ids[to]
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

    /// Tell the ui which apps are running.
    fn send_running(&self, tx: &Sender<Msg>) {
        let cmd = format!("ps -A -o NAME; echo @@; {}", apps::PERSISTENT_CMD);
        if let Ok(out) = self.tv.shell(&[&cmd]) {
            let (ps, pers) = out.split_once("@@").unwrap_or((&out, ""));
            let _ = tx.send(Msg::Running(apps::running(ps), apps::persistent(pers)));
        }
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
        // live TV: the channel and programme (when the TV can say)
        let mut live = match (&pkg, awake) {
            (Some(p), true) if live_provider().is_some_and(|lp| lp.hosts().contains(&p.as_str())) => nowplaying::live(&self.tv, p),
            _ => None,
        };
        let activity = match (&pkg, awake) {
            (_, false) => Some(Activity::just("In standby")),
            // live TV: the channel and programme (when the TV can say)
            (Some(_), true) if live.is_some() => live.take(),
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
            Action::PrefetchInfo => {
                let ip = self.tv.ip.clone();
                std::thread::spawn(move || {
                    let tv = Tv { ip };
                    // reads (and caches) every description; any one will do to start it
                    if let Some(g) = live_provider().and_then(|p| p.guide()) {
                        let _ = g.info(&crate::provider::Cx { tv: &tv }, 0, 0);
                    }
                });
                String::new()
            }
            Action::LoadInfo(ch, start) => {
                // the first read takes the TV a while: off the worker, so the screen keeps refreshing
                let (ip, tx) = (self.tv.ip.clone(), tx.clone());
                std::thread::spawn(move || {
                    let tv = Tv { ip };
                    let info = live_provider()
                        .and_then(|p| p.guide())
                        .map_or(Ok(None), |g| g.info(&crate::provider::Cx { tv: &tv }, ch, start))
                        .map_err(|e| e.to_string());
                    let _ = tx.send(Msg::Info((ch, start), info));
                });
                String::new()
            }
            Action::LoadGuide => {
                let _ = tx.send(Msg::Guide(load_guide(&self.tv).map_err(|e| e.to_string())));
                String::new()
            }
            Action::Favourite(ch, on) => {
                let Some(p) = live_provider() else { return Err(crate::error::Error::Failed("no live TV here".into())) };
                let done = crate::favourites::set(p, &crate::provider::Cx { tv: &self.tv }, ch.id, on);
                // the guide as synced (a failure puts the star back)
                let mut g = load_guide(&self.tv).map_err(|e| e.to_string());
                if let Ok(g) = &mut g {
                    g.news = None;
                }
                let _ = tx.send(Msg::Guide(g));
                crate::plugins::guide::fav_message(&ch, on, &done?)
            }
            Action::Watch(ch) => {
                let Some(p) = live_provider() else { return Err(crate::error::Error::Failed("no live TV here".into())) };
                p.channels().expect("checked").tune(&crate::provider::Cx { tv: &self.tv }, &ch)?
            }
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
            Action::Text(t) => {
                self.tv.shell(&["input", "text", &super::input::escape_text(&t)])?;
                format!("typed \"{t}\"")
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
                self.send_running(tx);
                format!("launched {}", app_name(apps::pkg_of(&target)))
            }
            Action::Front(target) => {
                apps::bring_to_front(&self.tv, &target)?;
                format!("brought {} to the front", app_name(apps::pkg_of(&target)))
            }
            Action::Close(pkg) => {
                self.tv.shell(&["am", "force-stop", &pkg])?;
                self.send_running(tx);
                format!("closed {}", app_name(&pkg))
            }
            Action::Restart { pkg, target } => {
                self.tv.shell(&["am", "force-stop", &pkg])?;
                apps::start(&self.tv, &target)?;
                self.send_running(tx);
                format!("restarted {}", app_name(&pkg))
            }
            Action::CheckRunning => {
                self.send_running(tx);
                String::new()
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
                    Ok(note) if note.is_empty() => continue,
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
                let default_home = apps::default_home(&self.tv).ok().flatten();
                let default_pkg = default_home.as_deref().map(|c| c.split('/').next().unwrap_or(c));
                let rows = launcher_entries(&apps::installed_aliases(&pm), &self.guesses, default_pkg);
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
    /// The guide pop-up's rows; None until fetched.
    guide: Option<std::result::Result<Guide, String>>,
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
    /// What to do with launcher row `sel` of `tab`; `at` is the highlighted choice.
    AppActions { tab: AppTab, sel: usize, at: usize },
    /// tvbee's options, with N highlighted.
    Options(usize),
    /// The live TV guide on `tab`, with channel `at` (an id) highlighted;
    /// the filter typed is `App::prompt`.
    Guide { tab: GuideTab, at: u32 },
}

/// Something the launcher's actions pop-up offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppChoice {
    Open,
    Front,
    Restart,
    Close,
    Search,
    Cancel,
}

impl AppChoice {
    /// What it says in the menu for the app called `app`.
    pub fn label(self, app: &str) -> String {
        match self {
            AppChoice::Open => "open".into(),
            AppChoice::Front => "bring to front".into(),
            AppChoice::Restart => "restart (close, then start fresh)".into(),
            AppChoice::Close => "close (force-stop)".into(),
            AppChoice::Search => format!("search {app}"),
            AppChoice::Cancel => "cancel".into(),
        }
    }
}

/// The choices for an app: running ones can be brought forward, closed or
/// restarted; "home" can't be closed.
pub fn app_choices(e: &Entry, running: bool, fixed: bool) -> Vec<AppChoice> {
    let home = e.target == "home";
    let mut v = vec![if running { AppChoice::Front } else { AppChoice::Open }];
    // Android restarts a persistent app as soon as it's stopped
    if running && !home && !fixed {
        v.extend([AppChoice::Restart, AppChoice::Close]);
    }
    if !home {
        v.push(AppChoice::Search);
    }
    v.push(AppChoice::Cancel);
    v
}

/// What a choice does: an action to run, or the search box (None, true).
pub fn choice_action(c: AppChoice, e: &Entry) -> (Option<Action>, bool) {
    match c {
        AppChoice::Open => (Some(Action::Launch(e.target.clone())), false),
        AppChoice::Front => (Some(Action::Front(e.target.clone())), false),
        AppChoice::Restart => (Some(Action::Restart { pkg: e.package.clone(), target: e.target.clone() }), false),
        AppChoice::Close => (Some(Action::Close(e.package.clone())), false),
        AppChoice::Search => (None, true),
        AppChoice::Cancel => (None, false),
    }
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
    /// What kind of app it is, shown beside apps without an alias ("player").
    pub tag: Option<&'static str>,
    pub tab: AppTab,
    /// The heading it's listed under in the Other tab.
    pub section: &'static str,
}

/// A package's friendly name, for the status line.
pub fn app_name(pkg: &str) -> String {
    apps::by_package(pkg).map_or_else(|| pkg.to_string(), |a| list_name(a.name))
}

/// Other-tab headings, in the order they're shown.
pub const SECTIONS: &[&str] = &["Other home apps", "Video & music", "Players", "Live TV", "Stores", "Other apps"];

fn section_of(kind: apps::Kind) -> &'static str {
    match kind {
        apps::Kind::Launcher => SECTIONS[0],
        apps::Kind::Video | apps::Kind::Music => SECTIONS[1],
        apps::Kind::Player => SECTIONS[2],
        apps::Kind::LiveTv => SECTIONS[3],
        apps::Kind::Store => SECTIONS[4],
        apps::Kind::Other => SECTIONS[5],
    }
}

/// The launcher's rows. Apps tab: the aliased apps, `home` standing for the
/// default home app. Other tab: home apps that aren't the default, then the
/// rest of the TV-launcher apps, by section.
pub fn launcher_entries(known: &[&'static apps::App], guessed: &[apps::Guess], default_home: Option<&str>) -> Vec<Entry> {
    let is_default = |pkg: &str| default_home == Some(pkg);
    let mut rows: Vec<Entry> = Vec::new();
    let mut others: Vec<Entry> = Vec::new();
    for a in known {
        let pkg = a.package();
        let other_home = a.kind == apps::Kind::Launcher && a.target != "@home";
        if other_home && is_default(pkg) {
            // that's what "home" opens
            continue;
        }
        let e = Entry {
            // "the home screen" says nothing about which one; the package does
            name: if other_home { pkg.to_string() } else { list_name(a.name) },
            alias: a.alias,
            target: a.alias.unwrap_or(a.target).to_string(),
            package: pkg.to_string(),
            tag: None,
            tab: if other_home { AppTab::Other } else { AppTab::Known },
            section: section_of(a.kind),
        };
        if other_home { others.push(e) } else { rows.push(e) }
    }
    for g in guessed {
        if is_default(&g.package) || rows.iter().chain(&others).any(|r| r.package == g.package) {
            continue;
        }
        let Some(component) = g.component.clone() else { continue };
        let table = apps::by_package(&g.package);
        // generic table names ("the home screen", "a NetTV app") don't say
        // which app this is; the package does
        let name = match table {
            Some(a) if !a.name.starts_with("a ") && !a.name.starts_with("the ") => list_name(a.name),
            _ => g.package.clone(),
        };
        let kind = table.map_or(g.kind, |a| a.kind);
        let tag = (kind != apps::Kind::Other).then(|| kind.as_str());
        others.push(Entry { name, alias: None, target: component, package: g.package.clone(), tag, tab: AppTab::Other, section: section_of(kind) });
    }
    let order = |e: &Entry| SECTIONS.iter().position(|s| *s == e.section).unwrap_or(SECTIONS.len());
    others.sort_by_key(|e| (order(e), e.name.to_lowercase()));
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
        e.tab == self
    }
}

/// How many lines a tab draws: its entries, plus section headings on Other.
pub fn tab_rows(all: &[Entry], tab: AppTab) -> usize {
    let list = tab_list(all, tab);
    let headings = if tab == AppTab::Other {
        let mut s: Vec<&str> = list.iter().map(|e| e.section).collect();
        s.dedup();
        s.len()
    } else {
        0
    };
    list.len() + headings
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
        KeyCode::Char(' ') if n > 0 => (Overlay::AppActions { tab, sel, at: 0 }, None),
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
        Overlay::AppActions { tab, sel, at } => (Overlay::AppActions { tab, sel, at }, None),
        // handled by App::guide_key
        Overlay::Guide { tab, at } => (Overlay::Guide { tab, at }, None),
        // handled by App::options_key
        Overlay::Options(i) => (Overlay::Options(i), None),
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
    /// Choice N in the launcher's actions pop-up.
    AppChoice(usize),
    /// A channel (by id) in the guide.
    GuideRow(u32),
    /// The star of a channel in the guide: never switches channel.
    GuideStar(u32),
    GuideTab(GuideTab),
    /// Option N in the options dialog.
    OptionItem(usize),
    /// Part of the guide's details panel.
    Details(DetailsClick),
    /// A field of what's on that opens something.
    Open(nowplaying::Open),
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
        // keys go to the TV: the rest are buttons to click (no key)
        Mode::Remote => {
            return vec![
                button("↑", "up", Do(Key(19))),
                button("↓", "down", Do(Key(20))),
                button("←", "left", Do(Key(21))),
                button("→", "right", Do(Key(22))),
                button("⏎", "ok", Do(Key(KEY_OK))),
                button("esc", "back", Do(Key(KEY_BACK))),
                button("⌫", "delete", Do(Key(KEY_DEL))),
                button("home", "home", Do(Key(KEY_HOME))),
                button("tab", "player mode", ToggleMode),
                button("^c", "quit", Quit),
                button("", "play/pause", Do(PlayPause)),
                button("", "vol+", Do(Volume(1))),
                button("", "vol-", Do(Volume(-1))),
                button("", "info", Input::ToggleInfo),
                button("", "apps", Input::OpenApps),
                button("", "search", Input::OpenSearch),
                button("", "guide", Input::OpenGuide),
                button("", "options", Input::OpenOptions),
                button("", "more", OpenMenu),
                button("", "help", ToggleHelp),
            ];
        }
    };
    b.extend([
        button("⌫", "back", Do(Key(KEY_BACK))),
        button("h", "home", Do(Key(KEY_HOME))),
        button("tab", if mode == Mode::Player { "remote" } else { "player" }, ToggleMode),
        button("i", "info", Input::ToggleInfo),
        button("a", "apps", Input::OpenApps),
        button("/", "search", Input::OpenSearch),
        button("g", "guide", Input::OpenGuide),
        button(",", "options", Input::OpenOptions),
        button(".", "more", OpenMenu),
        button("?", "help", ToggleHelp),
        button("q", "quit", Quit),
    ]);
    b
}

fn button_width(b: &Button) -> u16 {
    // " key " + " label", or " label " for a button with no key
    let key = if b.key.is_empty() { 0 } else { Span::raw(b.key).width() + 2 };
    u16::try_from(key + Span::raw(b.label).width() + 1 + usize::from(b.key.is_empty())).unwrap_or(u16::MAX)
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
                // the latest snapshot is always kept; while an action
                // settles it's only shown once the action has got there
                let headline = s.activity.as_ref().map(Activity::headline);
                if let Some(st) = &mut self.settling {
                    st.observe(front_pkg(&s).as_deref(), headline.as_deref(), Instant::now());
                }
                (self.snap, self.at, self.error) = (Some(*s), at, None);
            }
            Msg::Note(n) => self.note = n,
            Msg::Failed(e) => {
                // the action didn't happen: show what's really on
                self.settling = None;
                self.error = Some(e);
            }
            Msg::Apps(list) => self.apps = Some(list),
            Msg::Searchable(list) => self.searchable = Some(list),
            Msg::Running(set, fixed) => (self.running, self.fixed) = (set, fixed),
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
            }
            Msg::Seeked(pos, at) => {
                if let Some(p) = self.snap.as_mut().and_then(|s| s.activity.as_mut()).and_then(|a| a.progress.as_mut()) {
                    p.position = pos;
                    self.at = at;
                }
            }
        }
    }

    /// Carry out choice `at` of the actions pop-up for launcher row `sel`.
    fn run_choice(&mut self, tab: AppTab, sel: usize, at: usize, actions: &Sender<Action>) {
        let rows = tab_list(self.apps.as_deref().unwrap_or_default(), tab);
        let Some(e) = rows.get(sel) else {
            self.overlay = Overlay::None;
            return;
        };
        let running = self.running.contains(&e.package);
        let Some(choice) = app_choices(e, running, self.fixed.contains(&e.package)).get(at).copied() else { return };
        match choice_action(choice, e) {
            // done: back to the list, with what's running brought up to date
            (Some(a), _) => {
                self.apply_overlay(Overlay::Apps { tab, sel }, Some(a), actions);
                let _ = actions.send(Action::CheckRunning);
            }
            (None, true) => self.open_search(Some(e.package.clone())),
            // cancel: back to the list
            (None, false) => self.overlay = Overlay::Apps { tab, sel },
        }
    }

    /// Keys in the actions pop-up.
    fn choice_key(&mut self, tab: AppTab, sel: usize, at: usize, code: KeyCode, actions: &Sender<Action>) {
        let rows = tab_list(self.apps.as_deref().unwrap_or_default(), tab);
        let n = rows.get(sel).map_or(1, |e| app_choices(e, self.running.contains(&e.package), self.fixed.contains(&e.package)).len());
        match code {
            KeyCode::Up => self.overlay = Overlay::AppActions { tab, sel, at: (at + n - 1) % n },
            KeyCode::Down | KeyCode::Tab => self.overlay = Overlay::AppActions { tab, sel, at: (at + 1) % n },
            KeyCode::Enter | KeyCode::Char(' ') => self.run_choice(tab, sel, at, actions),
            // anything else goes back to the list
            _ => self.overlay = Overlay::Apps { tab, sel },
        }
    }

    /// Show `label` in place of the what line until the action lands (with
    /// `expect` in front, if given) or it's had time to.
    fn settle(&mut self, label: String, expect: Option<String>) {
        if !self.settle {
            return;
        }
        let snap = self.snap.as_ref();
        // home screens on the way: the ones tvbee knows, and those on the TV
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

    /// The guide's rows, once fetched.
    fn guide_rows(&self) -> &[GuideRow] {
        self.guide.as_ref().and_then(|g| g.as_ref().ok()).map_or(&[][..], |g| &g.rows[..])
    }

    /// Open the guide on `channel`, else the channel that's on. It's
    /// fetched afresh each time (what's on moves on); the last copy shows
    /// meanwhile.
    fn open_guide(&mut self, channel: Option<u32>, actions: &Sender<Action>) {
        if live_provider().is_none() {
            self.note = "no live TV guide on this TV".into();
            return;
        }
        let on_now = self.snap.as_ref().and_then(|s| s.activity.as_ref()).and_then(|a| a.field("channel")).and_then(|f| match f.open {
            Some(nowplaying::Open::Guide { channel }) => channel,
            None => None,
        });
        self.prompt.clear();
        self.guide_later = 0;
        self.guide_view = self.guide_start();
        self.overlay = Overlay::Guide { tab: GuideTab::All, at: channel.or(on_now).unwrap_or(0) };
        let _ = actions.send(Action::LoadGuide);
        // the details panel's descriptions take the TV a while the first time
        let _ = actions.send(Action::PrefetchInfo);
    }

    /// Run what a clicked field opens.
    fn open(&mut self, o: nowplaying::Open, actions: &Sender<Action>) {
        match o {
            nowplaying::Open::Guide { channel } => self.open_guide(channel, actions),
        }
    }

    /// Where the grid's left edge should be: the half hour before now, plus
    /// the steps taken.
    fn guide_start(&self) -> i64 {
        crate::plugins::guide::now().div_euclid(GUIDE_STEP) * GUIDE_STEP + self.guide_later * GUIDE_STEP
    }

    /// Whether the guide is sliding (so frames should come quickly).
    fn animating(&self) -> bool {
        matches!(self.overlay, Overlay::Guide { .. }) && self.guide_view != self.guide_start()
    }

    /// Move the drawn grid toward where it should be: one cell every 10ms
    /// with scroll animations on, else straight there.
    fn slide(&mut self, spc: i64) {
        let target = self.guide_start();
        if !crate::options::get(crate::options::SCROLL_ANIMATION) {
            self.guide_view = target;
            return;
        }
        let cells = (self.guide_slid.elapsed().as_millis() / 10).max(1) as i64;
        self.guide_slid = Instant::now();
        let step = (cells * spc).min((target - self.guide_view).abs());
        self.guide_view += step * (target - self.guide_view).signum();
    }

    /// Keys in the options dialog: ↑/↓ choose, Space/Enter toggle, Esc closes.
    fn options_key(&mut self, i: usize, code: KeyCode) {
        let n = crate::options::OPTIONS.len().max(1);
        match code {
            KeyCode::Esc | KeyCode::Char(',') | KeyCode::Char('q') | KeyCode::Backspace => self.overlay = Overlay::None,
            KeyCode::Up => self.overlay = Overlay::Options((i + n - 1) % n),
            KeyCode::Down | KeyCode::Tab => self.overlay = Overlay::Options((i + 1) % n),
            KeyCode::Enter | KeyCode::Char(' ') => self.toggle_option(i),
            _ => {}
        }
    }

    /// Flip option `i` and save it.
    fn toggle_option(&mut self, i: usize) {
        let Some(o) = crate::options::OPTIONS.get(i) else { return };
        let on = !crate::options::get(o.name);
        self.note = match crate::options::set(o.name, on) {
            Ok(()) => format!("{}: {}", o.label, if on { "on" } else { "off" }),
            Err(e) => format!("couldn't save the option: {e}"),
        };
        self.overlay = Overlay::Options(i);
    }

    /// The time the guide shows.
    fn guide_time(&self) -> i64 {
        guide_time_at(crate::plugins::guide::now(), self.guide_later)
    }

    /// Keys in the guide: typing filters, ↑/↓/page keys/home/end move, ←/→
    /// go earlier/later in time, Tab switches tabs, Enter watches, Esc closes.
    fn guide_key(&mut self, tab: GuideTab, at: u32, code: KeyCode, mods: KeyModifiers, actions: &Sender<Action>) {
        let mut tab = tab;
        // keys for the details panel while it has the keyboard
        if self.details == Details::Focused {
            match code {
                KeyCode::Esc | KeyCode::BackTab | KeyCode::Tab => self.details = Details::Open,
                KeyCode::Backspace | KeyCode::Char('h') => self.details = Details::Closed,
                KeyCode::Up => self.details_scroll = self.details_scroll.saturating_sub(1),
                KeyCode::Down => self.details_scroll += 1,
                KeyCode::PageUp => self.details_scroll = self.details_scroll.saturating_sub(DETAILS_ROWS),
                KeyCode::PageDown => self.details_scroll += DETAILS_ROWS,
                KeyCode::Home => self.details_scroll = 0,
                KeyCode::End => self.details_scroll = usize::MAX / 2,
                _ => {}
            }
            return;
        }
        match code {
            // shift+tab: into the details panel (opening it)
            KeyCode::BackTab => {
                self.details = Details::Focused;
                return;
            }
            KeyCode::Esc => {
                self.overlay = Overlay::None;
                return;
            }
            KeyCode::Char('c') if mods.contains(KeyModifiers::CONTROL) => {
                self.overlay = Overlay::None;
                return;
            }
            KeyCode::Enter => {
                self.watch_channel(at, actions);
                return;
            }
            // * isn't in any channel name, so it can't be part of a filter
            KeyCode::Char('*') => {
                self.toggle_favourite(at, actions);
                return;
            }
            KeyCode::Tab => tab = tab.next(),
            KeyCode::Right => self.guide_later = (self.guide_later + 1).min(GUIDE_AHEAD / GUIDE_STEP),
            KeyCode::Left => self.guide_later = self.guide_later.saturating_sub(1).max(0),
            KeyCode::Backspace => {
                self.prompt.pop();
            }
            KeyCode::Char(c) => self.prompt.push(c),
            _ => {}
        }
        let ids: Vec<u32> = guide_shown(self.guide_rows(), tab, &self.prompt, self.guide_time()).iter().map(|r| r.channel.id).collect();
        // keep the highlight on its channel while it's shown; else the first
        self.overlay = Overlay::Guide { tab, at: guide_move(&ids, at, code) };
    }

    /// Star or unstar channel `id`: the star changes now, and the guide is
    /// read back from the TV once it's done (so a refusal puts it back).
    fn toggle_favourite(&mut self, id: u32, actions: &Sender<Action>) {
        let Some(Ok(g)) = self.guide.as_mut() else { return };
        let Some(row) = g.rows.iter_mut().find(|r| r.channel.id == id) else { return };
        row.channel.favourite = !row.channel.favourite;
        let (ch, on) = (row.channel.clone(), row.channel.favourite);
        self.note = format!("{} {}...", if on { "adding to favourites:" } else { "removing from favourites:" }, ch.label());
        let _ = actions.send(Action::Favourite(ch, on));
    }

    /// Switch to channel `id` and close the guide.
    fn watch_channel(&mut self, id: u32, actions: &Sender<Action>) {
        let ch = self.guide_rows().iter().find(|r| r.channel.id == id).map(|r| r.channel.clone());
        self.overlay = Overlay::None;
        if let Some(ch) = ch {
            self.note = format!("switching to {}...", ch.label());
            self.settle(self.note.clone(), None);
            let _ = actions.send(Action::Watch(ch));
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
                    Mode::Remote => "remote mode: keys go to the TV (typing too); Tab for player mode".into(),
                    Mode::Player => "player mode: arrows seek and change volume".into(),
                };
            }
            Input::ToggleHelp => self.overlay = Overlay::Help,
            Input::ToggleInfo => self.info = !self.info,
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
            (Overlay::Guide { at, .. }, Some(Hit::GuideTab(t))) => self.overlay = Overlay::Guide { tab: t, at },
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
            let line = if b.key.is_empty() {
                // click only: a button of its own
                Line::from(Span::styled(format!(" {} ", b.label), Style::new().fg(Color::Black).bg(Color::DarkGray)))
            } else {
                Line::from(vec![
                    Span::styled(format!(" {} ", b.key), Style::new().fg(Color::Black).bg(Color::Gray)),
                    Span::styled(format!(" {}", b.label), Style::new().fg(Color::Gray)),
                ])
            };
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
        let settling = self.settling_label();
        match &self.snap {
            None => lines.push((Line::from("waiting for the TV...").italic(), None)),
            Some(s) => {
                match &s.activity {
                    // still settling: the action's label, not the half-way state
                    _ if settling.is_some() => {
                        let label_text = settling.clone().unwrap_or_default();
                        lines.push((Line::from(vec![label("what"), Span::raw(label_text).italic()]), None));
                    }
                    Some(a) => {
                        lines.push((Line::from(vec![label("what"), Span::raw(a.what.clone()).bold()]), None));
                        // fields that open something look like links
                        let field = |f: &nowplaying::Field| {
                            let v = Span::raw(f.value.clone());
                            let v = if f.open.is_some() { v.fg(Color::Cyan).underlined() } else { v };
                            (Line::from(vec![label(f.label), v]), f.open.map(Hit::Open))
                        };
                        lines.extend(a.fields.iter().filter(|f| f.place == nowplaying::Place::Top).map(field));
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
                            // jumping needs a video tvbee can seek in (not live TV)
                            let hit = p.duration.filter(|_| s.exact).map(|_| Hit::Progress {
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
                        lines.extend(a.fields.iter().filter(|f| f.place == nowplaying::Place::Bottom).map(field));
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

    /// The launcher pop-up; `behind` another pop-up it's dimmed and inert.
    fn draw_apps(&mut self, f: &mut Frame, tab: AppTab, sel: usize, behind: bool) {
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
        // (line, the entry it shows); section headings show none
        let mut rows: Vec<(Line, Option<usize>)> = Vec::new();
        if self.apps.is_none() {
            rows.push((Line::from("looking for installed apps...").italic(), None));
        } else if list.is_empty() {
            rows.push((Line::from("none on this TV").italic(), None));
        }
        // wide enough for the other tab too, so switching doesn't resize the box
        let wide = all.iter().map(|e| e.name.chars().count()).max().unwrap_or(0).min(40);
        let mut section = "";
        for (i, e) in list.iter().enumerate() {
            if tab == AppTab::Other && e.section != section {
                section = e.section;
                rows.push((Line::from(format!(" {section}")).bold(), None));
            }
            let on = if Some(e.package.as_str()) == pkg.as_deref() { "  (on now)" } else { "" };
            let extra = e.alias.or(e.tag).unwrap_or("");
            let dot = match (self.running.contains(&e.package) && e.target != "home", self.fixed.contains(&e.package)) {
                (true, true) => "⊘",
                (true, false) => "●",
                _ => " ",
            };
            // fixed columns, so both tabs draw the same width; a space after
            // the dot, which some terminals draw wider than one cell
            let line = Line::from(format!(" {dot} {:<wide$}  {extra:<12}{on:<10}", e.name));
            rows.push((if i == sel { line.reversed() } else { line }, Some(i)));
        }
        // scroll so the highlighted row stays in view
        let room = usize::from(f.area().height.saturating_sub(11)).max(3);
        let at = rows.iter().position(|(_, i)| *i == Some(sel)).unwrap_or(0);
        let start = at.saturating_sub(room.saturating_sub(1)).min(rows.len().saturating_sub(room));
        let scrolls = rows.len() > room;
        let shown: Vec<(Line, Option<usize>)> = rows.into_iter().skip(start).take(room).collect();
        let ids: Vec<Option<usize>> = shown.iter().map(|(_, i)| *i).collect();
        let mut shown: Vec<Line> = shown.into_iter().map(|(l, _)| l).collect();
        // pad to the taller tab, so switching doesn't move the box (or its tabs)
        let tallest = [AppTab::Known, AppTab::Other].iter().map(|t| tab_rows(all, *t)).max().unwrap_or(0);
        while shown.len() < tallest.min(room) {
            shown.push(Line::default());
        }
        let mut text = vec![tab_line, Line::default()];
        text.extend(shown);
        text.push(Line::default());
        let more = if scrolls { "  (scrolls)" } else { "" };
        text.push(Line::from(format!("↑/↓ choose  ⏎ open (or to front)  space actions  / search{more}")).fg(Color::DarkGray));
        text.push(Line::from("● running  ⊘ always running: Android won't let it close").fg(Color::DarkGray));
        text.push(Line::from("letter = jump  tab/←/→ switch  click / right-click  esc closes").fg(Color::DarkGray));
        let r = popup(f, " launch an app ", text, Color::Green);
        if behind {
            // under another pop-up: dim, and not clickable
            f.buffer_mut().set_style(r, Style::new().fg(Color::DarkGray).bg(Color::Reset).remove_modifier(Modifier::BOLD));
            return;
        }
        self.hits.push((r, Hit::Popup));
        // tab labels on the first row
        let mut x = r.x + 1;
        for (t, l) in &labels {
            let w = u16::try_from(l.chars().count()).unwrap_or(0);
            self.hits.push((ratatui::layout::Rect { x, y: r.y + 1, width: w, height: 1 }, Hit::AppTab(*t)));
            x += w + 1;
        }
        // app rows start after the tabs and a blank line
        for (row, id) in ids.into_iter().enumerate() {
            if let Some(i) = id {
                let y = r.y + 3 + u16::try_from(row).unwrap_or(0);
                let width = r.width.saturating_sub(2);
                self.hits.push((ratatui::layout::Rect { x: r.x + 1, y, width, height: 1 }, Hit::AppItem(i)));
            }
        }
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
            Overlay::Options(sel) => {
                let mut text = Vec::new();
                for (i, o) in crate::options::OPTIONS.iter().enumerate() {
                    let tick = if crate::options::get(o.name) { "[x]" } else { "[ ]" };
                    let line = Line::from(vec![Span::raw(format!(" {tick} {:<20}", o.label)), Span::styled(format!("  {}", o.help), Style::new().fg(Color::DarkGray))]);
                    text.push(if i == sel { line.reversed() } else { line });
                }
                text.push(Line::default());
                text.push(Line::from("↑/↓ choose  space/⏎/click toggle  esc closes  (saved in ~/.config/tvbee/options.txt)").fg(Color::DarkGray));
                let r = popup(f, " options ", text, Color::Blue);
                self.hits.push((r, Hit::Popup));
                for i in 0..crate::options::OPTIONS.len() {
                    let y = r.y + 1 + u16::try_from(i).unwrap_or(0);
                    self.hits.push((ratatui::layout::Rect { x: r.x + 1, y, width: r.width.saturating_sub(2), height: 1 }, Hit::OptionItem(i)));
                }
            }
            Overlay::Guide { tab, at } => {
                let area = f.area();
                // the text, and the channel each line shows (for clicks)
                let mut text: Vec<Line> = Vec::new();
                let mut ids: Vec<Option<u32>> = Vec::new();
                let mut tabs: Vec<(GuideTab, String)> = Vec::new();
                // (text row, x within the box, width, what clicking does) for the details panel
                let mut details_hits: Vec<(usize, u16, u16, DetailsClick)> = Vec::new();
                let total = usize::from(area.width.saturating_sub(8)).clamp(50, 140);
                // the timeline grid, right of the channel names
                let ch_w = 24;
                let grid_w = total.saturating_sub(ch_w + 4).max(10);
                let spc = grid_scale(grid_w);
                if self.guide_slid.elapsed() > Duration::from_millis(200) {
                    // not sliding lately: a slide starts from now
                    self.guide_slid = Instant::now() - Duration::from_millis(10);
                }
                self.slide(spc);
                let title = match &self.guide {
                    None => {
                        text.push(Line::from("reading the guide (the first time takes a few seconds)...").italic());
                        " guide ".to_string()
                    }
                    Some(Err(e)) => {
                        text.extend(textwrap(e, total).into_iter().map(|l| Line::from(l).fg(Color::Yellow)));
                        " guide ".to_string()
                    }
                    Some(Ok(g)) => {
                        let off = crate::plugins::guide::utc_offset();
                        let fit = |s: &str, w: usize| -> String {
                            if s.chars().count() <= w { format!("{s:<w$}") } else { format!("{}…", s.chars().take(w.saturating_sub(1)).collect::<String>()) }
                        };
                        // tabs, with counts
                        for t in GuideTab::ALL {
                            let n = guide_shown(&g.rows, t, "", 0).len();
                            let name = t.label();
                            tabs.push((t, format!(" {name} ({n}) ")));
                        }
                        text.push(Line::from(
                            tabs.iter()
                                .flat_map(|(t, l)| {
                                    let st = if *t == tab { Style::new().fg(Color::Black).bg(Color::Magenta).bold() } else { Style::new().fg(Color::Gray) };
                                    [Span::styled(l.clone(), st), Span::raw(" ")]
                                })
                                .collect::<Vec<_>>(),
                        ));
                        ids.push(None);
                        let filter = if self.prompt.is_empty() {
                            Line::from("type to filter by number, name or programme").fg(Color::DarkGray)
                        } else {
                            Line::from(vec![Span::raw("filter: "), Span::raw(self.prompt.clone()).bold(), Span::raw("▏")])
                        };
                        text.push(filter);
                        ids.push(None);
                        let view = self.guide_view;
                        let t = self.guide_time();
                        let dim = Style::new().fg(Color::DarkGray);
                        let (ruler, marker) = grid_ruler(view, spc, grid_w, crate::plugins::guide::now(), off);
                        let mut head = vec![Span::styled(format!("  {}  ", fit(&format!("channel   {}", crate::plugins::guide::day_hhmm(view, off).split(' ').next().unwrap_or("")), ch_w)), dim)];
                        match marker {
                            Some(m) => {
                                let r: Vec<char> = ruler.chars().collect();
                                head.push(Span::styled(r[..m].iter().collect::<String>(), dim));
                                head.push(Span::styled("▼", Style::new().fg(Color::Magenta).bold()));
                                head.push(Span::styled(r[m + 1..].iter().collect::<String>(), dim));
                            }
                            None => head.push(Span::styled(ruler, dim)),
                        }
                        text.push(Line::from(head));
                        ids.push(None);
                        let shown = guide_shown(&g.rows, tab, &self.prompt, t);
                        // the details panel takes its rows from the list
                        let panel_h = if self.details == Details::Closed { 1 } else { DETAILS_ROWS + 1 };
                        let room = usize::from(area.height.saturating_sub(11)).saturating_sub(panel_h + 1).max(3);
                        if shown.is_empty() {
                            let why = match (tab, self.prompt.is_empty()) {
                                (GuideTab::Favourites, true) => "no favourites yet: add channels to a favourites list on the TV".to_string(),
                                (GuideTab::Radio, true) => "no radio stations in the TV's channel list".to_string(),
                                _ => format!("nothing matches \"{}\"", self.prompt),
                            };
                            text.push(Line::from(why).italic());
                            ids.push(None);
                        }
                        let sel = shown.iter().position(|r| r.channel.id == at).unwrap_or(0);
                        // sub-headers from the country's lineup (Entertainment, HD, ...)
                        let groups: Vec<Option<&str>> = shown.iter().map(|r| g.lineup.map(|l| l.group(r.channel.order().0))).collect();
                        for item in guide_items(&groups, sel, room) {
                            let i = match item {
                                GuideItem::Header(name) => {
                                    let rule = "─".repeat(total.saturating_sub(name.chars().count() + 4));
                                    text.push(Line::from(vec![Span::styled(format!(" ─ {name} "), Style::new().fg(Color::Magenta).bold()), Span::styled(rule, dim)]));
                                    ids.push(None);
                                    continue;
                                }
                                GuideItem::Row(i) => i,
                            };
                            let r = shown[i];
                            let mark = if g.current == Some(r.channel.id) { "▶" } else { " " };
                            let star = if r.channel.favourite { "★" } else { "☆" };
                            // not on the TV yet: tvbee will send it when it can
                            let wait = if g.pending.contains_key(&r.channel.id) { " ·" } else { "" };
                            // "(radio)" tells them apart in the other tabs; not needed in Radio
                            let radio = if r.channel.radio && tab != GuideTab::Radio { " (radio)" } else { "" };
                            let name = format!("{star} {}{radio}{wait}", r.channel.label());
                            let mut spans = vec![Span::raw(format!("{mark}{}  ", fit(&name, ch_w + 1)))];
                            // times and edges dimmed, so the titles stand out (not on the highlighted row)
                            // on the highlighted row, the programme the details are about is marked
                            let focus = (i == sel).then(|| r.at(t)).and_then(|(on, after)| on.or(after)).map(|e| e.start);
                            for (seg, run, marked) in grid_row(&r.events, view, spc, grid_w, off, focus) {
                                let span = match seg {
                                    Seg::Edge | Seg::Time if i != sel => Span::styled(run, dim),
                                    _ => Span::raw(run),
                                };
                                // marked: drawn the other way round, within the reversed row
                                spans.push(if marked { span.style(Style::new().fg(Color::Magenta).bg(Color::Black).bold()) } else { span });
                            }
                            let line = Line::from(spans);
                            text.push(if i == sel { line.reversed() } else { line });
                            ids.push(Some(r.channel.id));
                        }
                        // keep the box the same height while filtering
                        while text.len() < room + 3 {
                            text.push(Line::default());
                            ids.push(None);
                        }
                        // the details panel: the highlighted channel's programme at the guide's time
                        let picked = shown.get(sel).and_then(|r| {
                            let (on, after) = r.at(t);
                            on.or(after).map(|e| (e.clone(), r.channel.clone()))
                        });
                        let key = picked.as_ref().map(|(e, ch)| (ch.id, e.start));
                        if key != self.details_of {
                            self.details_of = key;
                            self.details_scroll = 0;
                        }
                        let panel_top = text.len();
                        let focused = self.details == Details::Focused;
                        let bar_style = if focused { Style::new().fg(Color::Black).bg(Color::Magenta).bold() } else { Style::new().fg(Color::Magenta) };
                        match (self.details, &picked) {
                            (Details::Closed, _) => {
                                let what = picked.as_ref().map_or(String::new(), |(e, _)| format!(": {}", e.title));
                                text.push(Line::from(vec![Span::styled("▸ details", bar_style), Span::styled(format!("{what}   (shift+tab or click to open)"), dim)]));
                                details_hits.push((panel_top, 0, total as u16, DetailsClick::Open));
                            }
                            (_, None) => {
                                text.push(Line::from(vec![Span::styled("▾ details", bar_style), Span::styled("  nothing on then", dim)]));
                                details_hits.push((panel_top, 0, total as u16, DetailsClick::Hide));
                                // the same height as with something in it
                                for k in 0..DETAILS_ROWS {
                                    text.push(Line::default());
                                    details_hits.push((panel_top + 1 + k, 0, total as u16, DetailsClick::Focus));
                                }
                            }
                            (_, Some((e, ch))) => {
                                let key = (ch.id, e.start);
                                let info = match (e.info.as_deref(), self.infos.get(&key)) {
                                    (Some(i), _) => Some(Some(i)),
                                    (None, Some(i)) => Some(i.as_deref()),
                                    (None, None) => {
                                        if self.infos_asked.insert(key) {
                                            self.want_info = Some(key);
                                        }
                                        None
                                    }
                                };
                                let all = details_text(e, ch, info, total.saturating_sub(2), off);
                                self.details_scroll = self.details_scroll.min(all.len().saturating_sub(DETAILS_ROWS));
                                let more = if info.is_none() {
                                    "  reading descriptions from the TV...".to_string()
                                } else if all.len() > DETAILS_ROWS {
                                    format!("  {}-{} of {} lines", self.details_scroll + 1, (self.details_scroll + DETAILS_ROWS).min(all.len()), all.len())
                                } else {
                                    String::new()
                                };
                                let head = "▾ details ";
                                let (focus_b, hide_b) = (if focused { "[back to the guide]" } else { "[focus: scroll]" }, "[hide]");
                                text.push(Line::from(vec![
                                    Span::styled(head, bar_style),
                                    Span::raw(" "),
                                    Span::styled(focus_b, Style::new().fg(Color::Cyan)),
                                    Span::raw(" "),
                                    Span::styled(hide_b, Style::new().fg(Color::Cyan)),
                                    Span::styled(more, dim),
                                ]));
                                let fx = (head.chars().count() + 1) as u16;
                                details_hits.push((panel_top, fx, focus_b.chars().count() as u16, if focused { DetailsClick::Unfocus } else { DetailsClick::Focus }));
                                details_hits.push((panel_top, fx + focus_b.chars().count() as u16 + 1, hide_b.len() as u16, DetailsClick::Hide));
                                for (k, (line, seg)) in all.iter().skip(self.details_scroll).take(DETAILS_ROWS).enumerate() {
                                    text.push(match seg {
                                        Seg::Title => Line::from(format!(" {line}")).bold(),
                                        Seg::Time => Line::from(format!(" {line}")).fg(Color::DarkGray),
                                        _ => Line::from(format!(" {line}")),
                                    });
                                    details_hits.push((panel_top + 1 + k, 0, total as u16, DetailsClick::Focus));
                                }
                                for k in all.len().saturating_sub(self.details_scroll)..DETAILS_ROWS {
                                    text.push(Line::default());
                                    details_hits.push((panel_top + 1 + k, 0, total as u16, DetailsClick::Focus));
                                }
                            }
                        }
                        while ids.len() < text.len() {
                            ids.push(None);
                        }
                        text.push(Line::default());
                        let more = if shown.len() > room { format!("  ({} of {})", sel + 1, shown.len()) } else { String::new() };
                        let keys = if focused {
                            "details: ↑/↓ pgup/pgdn scroll  esc/shift+tab back to the guide  h hide".to_string()
                        } else {
                            format!("↑/↓ choose  ←/→ half an hour  tab switch  ⏎/click watch  * favourite  shift+tab details  esc{more}")
                        };
                        text.push(Line::from(keys).fg(Color::DarkGray));
                        format!(" guide: {} ", g.place.as_deref().unwrap_or(g.provider))
                    }
                };
                let r = popup(f, &title, text, Color::Magenta);
                self.hits.push((r, Hit::Popup));
                for (row, id) in ids.into_iter().enumerate() {
                    if let Some(id) = id {
                        let y = r.y + 1 + u16::try_from(row).unwrap_or(0);
                        self.hits.push((ratatui::layout::Rect { x: r.x + 1, y, width: r.width.saturating_sub(2), height: 1 }, Hit::GuideRow(id)));
                        // the star (after the ▶ column), with a cell either side to be easy to hit
                        self.hits.push((ratatui::layout::Rect { x: r.x + 1, y, width: 4.min(r.width), height: 1 }, Hit::GuideStar(id)));
                    }
                }
                for (row, x, w, click) in details_hits {
                    let y = r.y + 1 + u16::try_from(row).unwrap_or(0);
                    self.hits.push((ratatui::layout::Rect { x: r.x + 1 + x, y, width: w.min(r.width.saturating_sub(2)), height: 1 }, Hit::Details(click)));
                }
                // tab labels on the first row
                let mut x = r.x + 1;
                for (t, l) in &tabs {
                    let w = u16::try_from(l.chars().count()).unwrap_or(0);
                    self.hits.push((ratatui::layout::Rect { x, y: r.y + 1, width: w, height: 1 }, Hit::GuideTab(*t)));
                    x += w + 1;
                }
            }
            Overlay::Apps { tab, sel } => self.draw_apps(f, tab, sel, false),
            Overlay::AppActions { tab, sel, at } => {
                // the list stays behind it, dimmed
                self.draw_apps(f, tab, sel, true);
                let rows = tab_list(self.apps.as_deref().unwrap_or_default(), tab);
                let Some(e) = rows.get(sel) else { return };
                let running = self.running.contains(&e.package);
                let fixed = self.fixed.contains(&e.package);
                let choices = app_choices(e, running, fixed);
                let mut text: Vec<Line> = vec![
                    Line::from(format!(
                        "{}  {}",
                        e.package,
                        match (running, fixed) {
                            (true, true) => "(always running: Android won't let it close)",
                            (true, false) => "(running)",
                            _ => "(not running)",
                        }
                    )).fg(Color::DarkGray),
                    Line::default(),
                ];
                for (i, c) in choices.iter().enumerate() {
                    let line = Line::from(format!("  {}", c.label(&e.name)));
                    text.push(if i == at { line.reversed() } else { line });
                }
                text.push(Line::default());
                text.push(Line::from("↑/↓ choose  ⏎ do it  esc back  or click").fg(Color::DarkGray));
                let r = popup(f, &format!(" {} ", e.name), text, Color::Yellow);
                self.hits.push((r, Hit::Popup));
                for i in 0..choices.len() {
                    let y = r.y + 3 + u16::try_from(i).unwrap_or(0);
                    self.hits.push((ratatui::layout::Rect { x: r.x + 1, y, width: r.width.saturating_sub(2), height: 1 }, Hit::AppChoice(i)));
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
/// Break `s` into lines of at most `w` characters, at spaces.
fn textwrap(s: &str, w: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for word in s.split_whitespace() {
        match out.last_mut() {
            Some(l) if l.chars().count() + 1 + word.chars().count() <= w => {
                l.push(' ');
                l.push_str(word);
            }
            _ => out.push(word.to_string()),
        }
    }
    out
}

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
   i        system info      a      launch an app (space there: front / restart / close)
   /        search the app in front (tab picks another app)
   g        live TV guide: now/next on every channel (type to filter, ←/→ later
            or earlier, tab favourites/radio, shift+tab programme details, ⏎ watch,
            * or click the star to add/remove a favourite);
            clicking the channel line opens it too
   . o      open a link (YouTube, Netflix, media files)
   ,        options (scroll animations, ...)
   q Esc    quit

 player mode
   ← →      seek -/+ 10s (exact in YouTube; ff/rew keys elsewhere)
   ↑ ↓      volume

 remote mode: the keyboard is the TV's (tab leaves, ctrl-c quits)
   arrows   move around the TV's menus
   ⏎ esc    OK, back
   ⌫ del    delete in a text box (backward, forward)
   home     the TV's home screen    pgup pgdn  page up/down
   typing   typed into the TV (a search box, say); pasting too
   other actions: the buttons along the bottom

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
        // drawing the details panel can find a description it needs
        if let Some((ch, start)) = app.want_info.take() {
            let _ = actions.send(Action::LoadInfo(ch, start));
        }
        // short poll: keeps the progress bar moving and keys snappy; shorter
        // still while the guide slides
        let wait = if app.animating() { Duration::from_millis(10) } else { Duration::from_millis(200) };
        if !event::poll(wait)? {
            continue;
        }
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
            _ => return crate::error::usage(format!("usage: tvbee ui [--no-mouse] [--no-settle]  (unexpected '{a}')")),
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
        running: std::collections::BTreeSet::new(),
        fixed: std::collections::BTreeSet::new(),
        searchable: None,
        search_targets: Vec::new(),
        settling: None,
        settle,
        guide: None,
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
    vec![Command::new(CAT, "ui", "[--no-mouse] [--no-settle]", "interactive remote: what's on, volume, seeking", ui).long(
        "A full-screen remote showing what is playing (as 'tvbee nowplaying') and the\n\
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
         \x20 a  launcher: the installed apps tvbee has aliases for (see 'tvbee apps'); ● marks\n\
         \x20    running ones (Enter brings them to the front), ⊘ ones Android keeps running\n\
         \x20    (live TV: they can't be closed); Space or right-click opens actions:\n\
         \x20    restart, close, search\n\
         \x20 /  search the app in front, or (in the launcher) the highlighted app\n\
         \x20 ?  all keys   q / Esc / Ctrl-C  quit\n\
         \n\
         Seeking is exact for apps tvbee can read the video of (YouTube); elsewhere\n\
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

#[cfg(test)]
mod tests {

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
            channel: crate::provider::Channel { id, number: Some(n.into()), name: Some(name.into()), radio: false, favourite: fav, delivery: None, dvb: None },
            events: vec![crate::provider::Event { channel: id, title: now.into(), start: 0, end: 1, genre: None, info: None }],
        };
        let rows = [
            row(159, "1", "BBC ONE Lon", "Countryfile", true),
            row(158, "2", "BBC TWO", "Race Across the World", false),
            row(156, "4", "Channel 4", "50 First Dates", false),
            row(143, "18", "More 4", "Come Dine with Me", true),
            GuideRow { channel: crate::provider::Channel { radio: true, ..row(12, "722", "BBC Essex", "Dotun Adebayo", false).channel }, events: Vec::new() },
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
        let ev = |title: &str, start, end| crate::provider::Event { channel: 1, title: title.into(), start, end, genre: None, info: None };
        let day = GuideRow { channel: rows[0].channel.clone(), events: vec![ev("Apple", 0, 100), ev("Banana", 100, 200), ev("Cherry", 250, 300)] };
        let titles = |t| { let (a, b) = day.at(t); (a.map(|e| e.title.clone()), b.map(|e| e.title.clone())) };
        assert_eq!(titles(50), (Some("Apple".into()), Some("Banana".into())));
        assert_eq!(titles(150), (Some("Banana".into()), Some("Cherry".into())));
        assert_eq!(titles(220), (None, Some("Cherry".into()))); // a gap
        assert_eq!(titles(400), (None, None));
        assert!(guide_matches(&day, "banana", 150) && !guide_matches(&day, "banana", 260));
        // the grid: 60s a cell, from 0
        let grid = |w| grid_row(&day.events, 0, 60, w, 0, None).into_iter().map(|(_, t, _)| t).collect::<String>();
        // Apple 0-100s (1 cell), Banana 100-200s (cells 1-3), gap, Cherry 250-300s (cell 4)
        assert_eq!(grid(6), "││0 │ ");
        let long = [ev("A long title here", 0, 1800), ev("B", 1800, 2400)];
        let row = grid_row(&long, 0, 60, 40, 0, Some(1800));
        let flat: String = row.iter().map(|(_, t, _)| t.as_str()).collect();
        assert_eq!(flat, "│00:00 A long title here      │00:30 B  ");
        assert_eq!(row[0], (Seg::Edge, "│".into(), false));
        assert_eq!(row[1], (Seg::Time, "00:00 ".into(), false));
        // the focused programme (B, from 1800) is flagged, its spaces too
        let marked: String = row.iter().filter(|(_, _, m)| *m).map(|(_, t, _)| t.as_str()).collect();
        assert_eq!(marked, "│00:30 B  ");
        // started before the left edge; and a cut-off title
        let flat: String = grid_row(&long, 300, 60, 10, 0, None).iter().map(|(_, t, _)| t.as_str()).collect();
        assert_eq!(flat, "◂A long t…");
        let (ruler, mark) = grid_ruler(0, 60, 40, 600, 0);
        assert_eq!((ruler.trim_end(), mark), ("00:00                         00:30", Some(10)));
        assert_eq!(grid_scale(90), 80);
        // the details text
        let mut e = ev("Garage Squad", 0, 3600);
        e.genre = Some(6);
        e.info = Some("The team restore a classic car for a family.\nNew series.".into());
        let info = e.info.clone();
        let lines: Vec<String> = details_text(&e, &rows[3].channel, Some(info.as_deref()), 30, 0).into_iter().map(|(l, _)| l).collect();
        assert_eq!(lines, ["Garage Squad", "Thu 00:00–01:00 (1h00) · 18 More 4 · documentary", "", "The team restore a classic car", "for a family.", "New series."]);
        // still loading: no body yet (the panel keeps its size with blank lines)
        let loading: Vec<String> = details_text(&e, &rows[3].channel, None, 30, 0).into_iter().map(|(l, _)| l).collect();
        assert_eq!(loading.len(), 3);
        // steps land on the half hour
        assert_eq!(guide_time_at(1_000, 0), 1_000);
        assert_eq!(guide_time_at(1_000, 1), 1_800);
        assert_eq!(guide_time_at(1_800, 2), 5_400);
        assert_eq!(textwrap("needs tvbee to be paired", 12), ["needs tvbee", "to be paired"]);
        assert_eq!(input_for(KeyCode::Char('g'), KeyModifiers::NONE, Mode::Player), Input::OpenGuide);
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
        launcher_entries(&apps::installed_aliases(pm), guessed, None)
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
    fn app_actions() {
        let all = launcher_entries(&apps::installed_aliases("package:org.videolan.vlc\n"), &[], None);
        let home = &all[0];
        let vlc = all.iter().find(|e| e.target == "vlc").unwrap();
        assert_eq!(app_choices(home, true, false), vec![AppChoice::Front, AppChoice::Cancel]);
        assert_eq!(app_choices(vlc, false, false), vec![AppChoice::Open, AppChoice::Search, AppChoice::Cancel]);
        assert_eq!(
            app_choices(vlc, true, false),
            vec![AppChoice::Front, AppChoice::Restart, AppChoice::Close, AppChoice::Search, AppChoice::Cancel]
        );
        // persistent (live TV): no closing or restarting
        assert_eq!(app_choices(vlc, true, true), vec![AppChoice::Front, AppChoice::Search, AppChoice::Cancel]);
        assert_eq!(choice_action(AppChoice::Close, vlc), (Some(Action::Close("org.videolan.vlc".into())), false));
        assert_eq!(
            choice_action(AppChoice::Restart, vlc),
            (Some(Action::Restart { pkg: "org.videolan.vlc".into(), target: "vlc".into() }), false)
        );
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
        let shown = |tab| {
            tab_list(&all, tab).iter().map(|e| (e.section, e.name.clone(), e.alias.or(e.tag))).collect::<Vec<_>>()
        };
        assert_eq!(
            shown(AppTab::Known),
            vec![("Other home apps", "Home screen".to_string(), Some("home")), ("Players", "VLC".into(), Some("vlc"))]
        );
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
        // typing fast: one `input text`
        let t = |s: &str| Text(s.into());
        assert_eq!(coalesce(vec![t("c"), t("a"), t("t"), Key(66), t("s")]), vec![t("cat"), Key(66), t("s")]);
    }
}

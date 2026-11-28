//! The engine: everything a front end needs from the TV, on worker threads,
//! so a front end never waits on adb. It reads what's on (the refresh), runs
//! what the user asks for (keys, actions, reads, background work), makes
//! pictures for a preview, and listens for players that push changes. A front
//! end sends [`Action`]s through a [`Handle`] (or runs any registered action
//! by id: [`Handle::dispatch`]), reads the [`TvState`], and is told what
//! changed with [`CoreEvent`]s ([`Engine::start`]).
//!
//! No terminal in here: `tveee ui` is one front end, a GUI could be another.


use crate::actions::{ActionDescriptor, Params};
use crate::devtools::{self, SeekTo};
use crate::error::Result;
use crate::nowplaying::{self, clock, Activity};
use crate::builtin::apps::list_name;
use crate::builtin::{apps, info, media, search};
use crate::tv::Tv;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

mod apps_rows;
mod guide;
mod lanes;
pub mod last;
mod preview;
mod state;

pub use self::apps_rows::*;
pub use self::guide::*;
pub use self::lanes::*;
pub use self::last::LastSeen;
pub use self::preview::*;
pub use self::state::*;

pub const KEY_BACK: u32 = 4;
pub const KEY_HOME: u32 = 3;
pub const KEY_OK: u32 = 23;
/// Delete (backspace), forward delete, page up / down.
pub const KEY_DEL: u32 = 67;
pub const KEY_FORWARD_DEL: u32 = 112;
pub const KEY_PAGE_UP: u32 = 92;
pub const KEY_PAGE_DOWN: u32 = 93;

/// A remote key's name, for the status line.
pub fn key_name(code: u32) -> &'static str {
    match code {
        KEY_BACK => "back",
        KEY_HOME => "home",
        KEY_OK => "ok",
        KEY_DEL => "delete",
        KEY_FORWARD_DEL => "forward delete",
        KEY_PAGE_UP => "page up",
        KEY_PAGE_DOWN => "page down",
        19 => "up",
        20 => "down",
        21 => "left",
        22 => "right",
        _ => "key",
    }
}

/// How often the refresh reads what's on.
pub const REFRESH: Duration = Duration::from_secs(1);
/// How often the TV is read with the "low bandwidth" option on.
pub const REFRESH_LOW: Duration = Duration::from_secs(4);
/// How often the system figures are read with it on (while they're shown).
pub const SYSTEM_LOW: Duration = Duration::from_secs(10);

/// What one fast-forward/rewind key press is assumed to skip, for apps we
/// can't seek exactly.
pub const KEY_STEP: f64 = 10.0;

/// A user request, handled by one of the lanes (see [`route`]).
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    PlayPause,
    Seek(f64),
    Jump(f64),
    Volume(i32),
    Mute,
    /// A panel (`crate::panel`, by id) opened (true) or closed: the refresh
    /// reads it while it's open.
    WatchPanel(&'static str, bool),
    /// The preview panel opened or closed: pictures are made while it's open.
    PreviewPanel(bool),
    /// Ask the TV to pair (it shows a PIN), then finish with the PIN typed.
    /// (A pairing, by id: `crate::pairing`.)
    PairStart(&'static str),
    PairFinish(String),
    /// Mute or unmute, the ui knowing which it is now (true: muted): one key,
    /// without reading the audio state first.
    MuteKnown(bool),
    /// Any remote key (back, home, d-pad, OK), by Android keycode.
    Key(u32),
    /// Type this text into the TV (remote mode).
    Text(String),
    /// Run an action (`crate::actions`) by id, with these arguments (for one
    /// that asks for text: the line typed).
    Run { id: &'static str, args: Vec<String> },
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
    /// Show or hide the app in front's music visualiser (switch).
    Visualiser,
    /// The system panel opened (true) or closed: the refresh reads the
    /// system figures while it's open (or the top line shows them).
    WatchSystem(bool),
    /// The list pane opened (true) or closed: while it's open the refresh
    /// reads where the app's list is ([`Snapshot::list_place`]).
    WatchList(bool),
    /// Read the app in front's list ([`crate::provider::Player::list`]).
    LoadList,
    /// Choose an item from it; its list is read again after.
    ListGo(crate::more::Go),
    /// Open a browsed list's folder (a link, see
    /// [`crate::provider::ContextList::browsed`]): read here, and shown on
    /// the app's screen too when `show`.
    ListOpen { at: String, show: bool },
    Refresh,
    /// Read the TV now, saying nothing: something the refresh feeds (the
    /// list pane, a panel) was just opened, and shouldn't wait for the next
    /// read (4s with low bandwidth).
    Wake,
}

#[derive(Default, Clone)]
pub struct Snapshot {
    pub activity: Option<Activity>,
    pub volume: Option<(u32, u32)>,
    pub muted: Option<bool>,
    /// whether exact seeking is available (devtools video found)
    pub exact: bool,
    pub standby: bool,
    /// For the system panel.
    pub system: info::System,
    /// The open panels, as they are now.
    pub panels: Vec<(&'static str, crate::panel::View)>,
    /// "player" or "player, guessed": what kind of app is in front.
    pub kind: Option<String>,
    pub power: Option<String>,
    pub focus: Option<String>,
    /// Where the app's list is (which list, and its highlighted row), while
    /// the list pane is open and the app can say.
    pub list_place: Option<crate::provider::ListPlace>,
}

/// The package of the app in front in a snapshot.
pub fn front_pkg(s: &Snapshot) -> Option<String> {
    s.focus.as_deref().map(|f| f.split('/').next().unwrap_or(f).to_string())
}

pub enum Msg {
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
    /// The app in front's list (None: it isn't showing one), or why not.
    List(std::result::Result<Option<crate::provider::ContextList>, String>),
    /// The panels this TV has (`crate::panel`), by id.
    Panels(Vec<&'static str>),
    /// Pairing: the TV is showing a PIN; ask for it.
    AskPin,
    /// A picture for the preview panel.
    Preview(crate::preview::Picture),
    /// What the preview is doing, or why there's no picture.
    PreviewNote(String),
}


/// The engine's threads, running for one TV.
pub struct Engine {
    /// Send what the user asks for, read the state.
    pub handle: Handle,
    /// What it has to say: changes, notes, pictures.
    pub events: Receiver<CoreEvent>,
    stop: Arc<AtomicBool>,
}

impl Engine {
    /// Start the lanes for the TV at `ip`; `actions` are those `Action::Run`
    /// can run (the enabled plugins'). Reads what's on at once; meanwhile the
    /// state is what was seen last run.
    pub fn start(ip: &str, actions: Vec<ActionDescriptor>) -> Engine {
        let (act_tx, act_rx) = mpsc::channel();
        let (msg_tx, msg_rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(Shared::new(ip.to_string(), Arc::clone(&stop)));
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
        let actor = Actor { tv: shared.tv(), shared: Arc::clone(&shared), actions, wake: wake_tx, reads: read_tx, pairing: None };
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
        let refresher = Refresher {
            tv: shared.tv(),
            shared: Arc::clone(&shared),
            channel: None,
            named: None,
            kept_power: Default::default(),
            kept_window: Default::default(),
            last_activity: None,
            said: String::new(),
            system: None,
            read_at: Instant::now(),
        };
        spawn_named("refresh", move || refresher.run(&wake_rx, msg_tx));
        let _ = act_tx.send(Action::Refresh);
        // the keeper: what the lanes find, into the state
        let seen = last::load(ip);
        let state = Arc::new(std::sync::Mutex::new(Arc::new(TvState::seeded(seen.as_ref()))));
        let (event_tx, event_rx) = mpsc::channel();
        let (st, ip) = (Arc::clone(&state), ip.to_string());
        spawn_named("keeper", move || keep(&ip, &st, seen, &msg_rx, &event_tx));
        Engine { handle: Handle::new(act_tx, state), events: event_rx, stop }
    }

    /// Stop the lanes and give them a moment to remove their port forwards
    /// (one may be mid-refresh); don't wait forever on a TV slow to answer.
    pub fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        drop(self.handle);
        let deadline = Instant::now() + Duration::from_secs(3);
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            if let Err(RecvTimeoutError::Disconnected) = self.events.recv_timeout(left) {
                break;
            }
        }
        // and close the kept connection (its port forward)
        devtools::keep_open(false);
    }
}

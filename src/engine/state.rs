//! What the engine knows about the TV ([`TvState`]), kept up to date from
//! what the lanes find, and what it tells front ends ([`CoreEvent`]).

use super::*;
use std::collections::{BTreeSet, HashMap};
use std::sync::Mutex;

/// How many memory readings the history keeps (one a refresh).
pub const MEM_HISTORY: usize = 600;

/// How long the last known volume counts as current when a refresh can't
/// read it; after that it's shown as old (greyed).
pub const VOLUME_STALE: Duration = Duration::from_secs(10);

/// Everything the engine knows about the TV. Plain data: a front end reads
/// it ([`Handle::state`]) whenever it draws.
#[derive(Clone)]
pub struct TvState {
    /// What's on, as of the last refresh.
    pub snap: Option<Snapshot>,
    /// When `snap` was taken (or a seek moved it), to move the progress bar
    /// along between refreshes.
    pub at: Instant,
    /// `snap` is what was last seen in an earlier run, not read yet.
    pub seeded: bool,
    /// Why the last refresh or action failed; cleared by the next refresh.
    pub error: Option<String>,
    /// The launcher's rows; None until read.
    pub apps: Option<Arc<Vec<Entry>>>,
    /// Apps with a proper search; None until read.
    pub searchable: Option<Arc<Vec<search::Searchable>>>,
    /// Apps with a process, and those of them Android keeps running.
    pub running: BTreeSet<String>,
    pub fixed: BTreeSet<String>,
    /// The panels this TV has (`crate::panel`), by id.
    pub panels: Vec<&'static str>,
    /// The guide, or why there isn't one; None until read.
    pub guide: Option<Arc<std::result::Result<Guide, String>>>,
    /// The guide is being read afresh (the last copy is still here).
    pub guide_loading: bool,
    /// Programme descriptions read so far (None: there is none), by
    /// (channel, start).
    pub infos: Arc<HashMap<(u32, i64), Option<String>>>,
    /// Memory used (MB) at each refresh, oldest first.
    pub mem_history: Vec<u64>,
    /// The last volume the TV reported (level, max), and when.
    pub last_volume: Option<((u32, u32), Instant)>,
}

impl TvState {
    /// To start with: what was seen last run, if anything.
    pub(crate) fn seeded(seen: Option<&LastSeen>) -> TvState {
        TvState {
            snap: seen.map(last::seed),
            at: Instant::now(),
            seeded: seen.is_some(),
            error: None,
            apps: None,
            searchable: None,
            running: BTreeSet::new(),
            fixed: BTreeSet::new(),
            panels: Vec::new(),
            guide: None,
            guide_loading: false,
            infos: Arc::default(),
            mem_history: Vec::new(),
            // shown as old: it's from before
            last_volume: seen.and_then(|l| Some((l.volume?, Instant::now().checked_sub(VOLUME_STALE * 2)?))),
        }
    }

    /// The guide, if it was read.
    pub fn guide_ok(&self) -> Option<&Guide> {
        self.guide.as_deref().and_then(|g| g.as_ref().ok())
    }

    /// The launcher's rows (none until read).
    pub fn apps_list(&self) -> &[Entry] {
        self.apps.as_deref().map_or(&[], |v| &v[..])
    }

    /// The apps with a proper search (none until read).
    pub fn searchable_list(&self) -> &[search::Searchable] {
        self.searchable.as_deref().map_or(&[], |v| &v[..])
    }
}

/// What changed in the [`TvState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Snapshot,
    /// A seek landed: the position moved before the next refresh.
    Seeked,
    Apps,
    Searchable,
    Running,
    Panels,
    Guide,
    Info,
}

/// What the engine tells front ends.
pub enum CoreEvent {
    /// The state changed: read it again.
    Changed(Change),
    /// Feedback for a status line.
    Note(String),
    /// An action or refresh failed: `TvState::error` says why.
    Failed,
    /// Pairing: the TV is showing a PIN; ask for it.
    AskPin,
    /// A picture for a preview.
    Preview(crate::preview::Picture),
    /// What the preview is doing, or why there's no picture.
    PreviewNote(String),
}

/// A front end's way in: send what the user asks for, read the state.
#[derive(Clone)]
pub struct Handle {
    actions: Sender<Action>,
    state: Arc<Mutex<Arc<TvState>>>,
}

impl Handle {
    pub(crate) fn new(actions: Sender<Action>, state: Arc<Mutex<Arc<TvState>>>) -> Handle {
        Handle { actions, state }
    }

    /// The state now. Cheap: front ends can call it every frame.
    pub fn state(&self) -> Arc<TvState> {
        Arc::clone(&lanes::lock(&self.state))
    }

    /// Change the state now (copied first if a front end is reading it).
    fn update(&self, f: impl FnOnce(&mut TvState)) {
        f(Arc::make_mut(&mut lanes::lock(&self.state)));
    }

    /// Run the action `id` (`crate::actions`) with `args`, in turn with the
    /// user's other actions.
    pub fn dispatch(&self, id: &'static str, args: Vec<String>) -> std::result::Result<(), mpsc::SendError<Action>> {
        self.send(Action::Run { id, args })
    }

    /// Ask for `a`. What shows at once does now (the guide loading, a star
    /// toggled); the rest when the lanes are done.
    pub fn send(&self, a: Action) -> std::result::Result<(), mpsc::SendError<Action>> {
        match &a {
            Action::LoadGuide => self.update(|s| s.guide_loading = true),
            Action::Favourite(ch, on) => self.update(|s| {
                if let Some(Ok(g)) = s.guide.as_mut().map(Arc::make_mut) {
                    if let Some(row) = g.rows.iter_mut().find(|r| r.channel.id == ch.id) {
                        row.channel.favourite = *on;
                    }
                }
            }),
            _ => {}
        }
        self.actions.send(a)
    }
}

/// Fold what the lanes say into the state, and tell the front end. Ends
/// when the lanes have all gone (or the front end has).
pub(crate) fn keep(ip: &str, state: &Mutex<Arc<TvState>>, mut kept: Option<LastSeen>, lanes: &Receiver<Msg>, events: &Sender<CoreEvent>) {
    let update = |f: &mut dyn FnMut(&mut TvState)| f(Arc::make_mut(&mut lanes::lock(state)));
    for m in lanes {
        let event = match m {
            Msg::Snap(s, at) => {
                // kept for the next run's first frame, when what's on changes
                if let Some(l) = last::of(&s) {
                    let same = kept.as_ref().is_some_and(|k| (&k.what, &k.title, k.volume) == (&l.what, &l.title, l.volume));
                    if !same {
                        last::save(ip, &l);
                        kept = Some(l);
                    }
                }
                update(&mut |st| {
                    if let Some(m) = s.system.mem {
                        st.mem_history.push(m.used() / 1024);
                        if st.mem_history.len() > MEM_HISTORY {
                            st.mem_history.remove(0);
                        }
                    }
                    if let Some(v) = s.volume {
                        st.last_volume = Some((v, Instant::now()));
                    }
                    (st.snap, st.at, st.error, st.seeded) = (Some((*s).clone()), at, None, false);
                });
                CoreEvent::Changed(Change::Snapshot)
            }
            Msg::Seeked(pos, at) => {
                update(&mut |st| {
                    if let Some(p) = st.snap.as_mut().and_then(|s| s.activity.as_mut()).and_then(|a| a.progress.as_mut()) {
                        p.position = pos;
                        st.at = at;
                    }
                });
                CoreEvent::Changed(Change::Seeked)
            }
            Msg::Note(n) => CoreEvent::Note(n),
            Msg::Failed(e) => {
                crate::tlog!("failed: {e}");
                update(&mut |st| st.error = Some(e.clone()));
                CoreEvent::Failed
            }
            Msg::Apps(list) => {
                update(&mut |st| st.apps = Some(Arc::new(list.clone())));
                CoreEvent::Changed(Change::Apps)
            }
            Msg::Searchable(list) => {
                update(&mut |st| st.searchable = Some(Arc::new(list.clone())));
                CoreEvent::Changed(Change::Searchable)
            }
            Msg::Running(running, fixed) => {
                update(&mut |st| (st.running, st.fixed) = (running.clone(), fixed.clone()));
                CoreEvent::Changed(Change::Running)
            }
            Msg::Panels(ids) => {
                update(&mut |st| st.panels = ids.clone());
                CoreEvent::Changed(Change::Panels)
            }
            Msg::Info(key, info) => {
                let info = info.unwrap_or_else(|e| Some(format!("(couldn't read the description: {e})")));
                update(&mut |st| {
                    Arc::make_mut(&mut st.infos).insert(key, info.clone());
                });
                CoreEvent::Changed(Change::Info)
            }
            Msg::Guide(g) => {
                let news = g.as_ref().ok().and_then(|g| g.news.clone());
                update(&mut |st| (st.guide, st.guide_loading) = (Some(Arc::new(g.clone())), false));
                if events.send(CoreEvent::Changed(Change::Guide)).is_err() {
                    return;
                }
                match news {
                    Some(n) => CoreEvent::Note(n),
                    None => continue,
                }
            }
            Msg::AskPin => CoreEvent::AskPin,
            Msg::Preview(p) => CoreEvent::Preview(p),
            Msg::PreviewNote(n) => CoreEvent::PreviewNote(n),
        };
        if events.send(event).is_err() {
            return;
        }
    }
}

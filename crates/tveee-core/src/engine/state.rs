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
    /// When that was (unix seconds), while `seeded`.
    pub seen_at: Option<i64>,
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
    /// The app in front's list (the list pane), as last read: None until
    /// read; Ok(None): it isn't showing one.
    pub list: Option<Arc<std::result::Result<Option<crate::provider::ContextList>, String>>>,
    /// A read of the list has been asked for and hasn't come yet.
    pub list_loading: bool,
    /// The last read of the list failed, but the one before is kept (a
    /// connection dropping out doesn't take the list away): why.
    pub list_failed: Option<String>,
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
    pub fn seeded(seen: Option<&LastSeen>) -> TvState {
        TvState {
            snap: seen.map(last::seed),
            at: Instant::now(),
            seeded: seen.is_some(),
            seen_at: seen.map(|l| l.at),
            error: None,
            apps: None,
            searchable: None,
            running: BTreeSet::new(),
            fixed: BTreeSet::new(),
            panels: Vec::new(),
            guide: None,
            guide_loading: false,
            list: None,
            list_loading: false,
            list_failed: None,
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

/// Whether reading `new` should keep the list `old` instead: a failed
/// read (the connection dropping out) keeps one read before, as does a read
/// saying there's no list when the one shown was browsed here (it doesn't
/// depend on what the app shows).
pub fn keeps_list(
    old: Option<&std::result::Result<Option<crate::provider::ContextList>, String>>,
    new: &std::result::Result<Option<crate::provider::ContextList>, String>,
) -> bool {
    let Some(Ok(Some(old))) = old else { return false };
    match new {
        Err(_) => true,
        Ok(None) => old.browsed,
        Ok(Some(_)) => false,
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
    List,
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
    pub fn new(actions: Sender<Action>, state: Arc<Mutex<Arc<TvState>>>) -> Handle {
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
            Action::LoadList | Action::ListGo(_) | Action::ListOpen { .. } => self.update(|s| s.list_loading = true),
            Action::Favourite(ch, on) => self.update(|s| {
                if let Some(Ok(g)) = s.guide.as_mut().map(Arc::make_mut)
                    && let Some(row) = g.rows.iter_mut().find(|r| r.channel.id == ch.id)
                {
                    row.channel.favourite = *on;
                }
            }),
            _ => {}
        }
        self.actions.send(a)
    }
}

/// Fold what the lanes say into the state, and tell the front end. Ends
/// when the lanes have all gone (or the front end has).
pub fn keep(ip: &str, state: &Mutex<Arc<TvState>>, mut kept: Option<LastSeen>, lanes: &Receiver<Msg>, events: &Sender<CoreEvent>) {
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
            Msg::Note(n) => CoreEvent::Note(note_line(&n)),
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
            Msg::List(l) => {
                if let Err(e) = &l {
                    crate::tlog!("list FAILED: {e}");
                }
                let mut kept = false;
                update(&mut |st| {
                    st.list_loading = false;
                    kept = keeps_list(st.list.as_deref(), &l);
                    if kept {
                        // the list shown stays; only why it couldn't be read again
                        st.list_failed = l.as_ref().err().cloned().or_else(|| Some("it said there's no list".into()));
                    } else {
                        st.list_failed = None;
                        st.list = Some(Arc::new(l.clone()));
                    }
                });
                CoreEvent::Changed(if kept { Change::Snapshot } else { Change::List })
            }
            Msg::Info(key, info) => {
                let info = info.unwrap_or_else(|e| Some(format!("(couldn't read the description: {e})")));
                update(&mut |st| {
                    Arc::make_mut(&mut st.infos).insert(key, info.clone());
                });
                CoreEvent::Changed(Change::Info)
            }
            Msg::Guide(g) => {
                if let Err(e) = &g {
                    // the ui shows it short: the whole of it goes here
                    crate::tlog!("guide FAILED: {e}");
                }
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

/// A note for the one status line: a note of several lines (a list) goes
/// to the log whole, and its first line says so.
pub fn note_line(n: &str) -> String {
    let mut lines = n.trim_end().lines();
    let first = lines.next().unwrap_or("").to_string();
    let more = lines.count();
    if more == 0 {
        return first;
    }
    for l in n.trim_end().lines() {
        crate::tlog!("  {l}");
    }
    format!("{first}  (+{more} more: the log has it all, : log)")
}

#[cfg(test)]
mod note_tests {
    #[test]
    fn long_notes_point_to_the_log() {
        assert_eq!(super::note_line("muted"), "muted");
        assert_eq!(super::note_line("a\nb\nc\n"), "a  (+2 more: the log has it all, : log)");
    }
}

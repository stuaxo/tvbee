//! The worker lanes: actions are routed to keys, act, read or idle lanes,
//! and the refresh reads the TV's state; they share [`Shared`].

use super::*;

/// A thread with a name (shown in the debug log, `TVEEE_LOG`).
pub(crate) fn spawn_named(name: &str, f: impl FnOnce() + Send + 'static) {
    let _ = std::thread::Builder::new().name(name.to_string()).spawn(f);
}

/// Which lane an action goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    /// Remote keys and typing ([`send_keys`]).
    Keys,
    /// Things the user did that change the TV ([`Actor`]), one at a time.
    Act,
    /// Things the user is waiting to see (the guide, what's running).
    Read,
    /// Work nobody is waiting for, done when the user pauses ([`background`]).
    Idle,
}

pub fn lane(a: &Action) -> Lane {
    match a {
        Action::Key(_) | Action::Text(_) => Lane::Keys,
        Action::LoadGuide | Action::CheckRunning | Action::AmbilightPanel(_) | Action::PreviewPanel(_) => Lane::Read,
        Action::PrefetchInfo => Lane::Idle,
        _ => Lane::Act,
    }
}

/// Hand each action to its lane, noting when the user last asked for
/// something. Ends when the UI hangs up.
pub(crate) fn route(actions: &Receiver<Action>, shared: &Shared, lanes: &[(Lane, Sender<Action>)]) {
    for a in actions {
        // not work for anyone: what the refresh should read
        if let Action::AmbilightPanel(on) = a {
            shared.ambilight.store(on, Ordering::Relaxed);
            continue;
        }
        if let Action::PreviewPanel(on) = a {
            shared.preview.store(on, Ordering::Relaxed);
            continue;
        }
        let l = lane(&a);
        if l != Lane::Idle {
            shared.touched();
        }
        let Some((_, to)) = lanes.iter().find(|(x, _)| *x == l) else { continue };
        if to.send(a).is_err() {
            return;
        }
    }
}

/// Send remote keys and typing as they come, over a shell kept open on the
/// TV, written raw to an input device where possible (see `crate::keys`).
pub(crate) fn send_keys(tv: Tv, keys: &Receiver<Action>, tx: &Sender<Msg>) {
    let mut session = crate::keys::Session::new(tv);
    session.warm();
    while let Ok(first) = keys.recv() {
        let mut queued = vec![first];
        queued.extend(keys.try_iter());
        for batch in batches(coalesce(queued)) {
            let result = match batch {
                Batch::Keys(codes) => {
                    let presses: Vec<(u32, bool)> = codes.iter().map(|c| (*c, false)).collect();
                    session.press(&presses).map(|()| codes.iter().map(|c| key_name(*c)).collect::<Vec<_>>().join(" "))
                }
                Batch::One(Action::Text(t)) => session.text(&t).map(|()| format!("typed \"{t}\"")),
                Batch::One(_) => continue,
            };
            let msg = match result {
                Ok(note) => Msg::Note(note),
                Err(e) => Msg::Failed(e.to_string()),
            };
            if tx.send(msg).is_err() {
                return;
            }
        }
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

/// What the lanes share.
pub(crate) struct Shared {
    pub(crate) ip: String,
    /// Set by the UI on quit: finish up, and don't open new port forwards.
    pub(crate) stop: Arc<AtomicBool>,
    /// The app in front, as of the last refresh.
    pub(crate) pkg: std::sync::Mutex<Option<String>>,
    /// Kinds of apps tveee has no entry for (fetched in the background).
    pub(crate) guesses: std::sync::Mutex<Vec<apps::Guess>>,
    /// Apps with a proper search (fetched in the background, or when needed).
    pub(crate) searchable: std::sync::Mutex<Vec<search::Searchable>>,
    /// Actions running now: the refresh holds off for them.
    pub(crate) busy: std::sync::atomic::AtomicUsize,
    /// When the user last asked for something: background work waits for a lull.
    pub(crate) input_at: std::sync::Mutex<Instant>,
    /// Something the user did may have changed live TV's channel.
    pub(crate) channel_stale: AtomicBool,
    /// The first refresh has been shown (background work waits for it).
    pub(crate) shown: AtomicBool,
    /// The Ambilight panel is open: the refresh reads the lights too.
    pub(crate) ambilight: AtomicBool,
    /// The preview panel is open: its lane makes pictures.
    pub(crate) preview: AtomicBool,
    /// A picture of what an app's player is playing, for the preview.
    pub(crate) art: std::sync::Mutex<Option<ArtNow>>,

    /// The launcher's apps have been sent (held while they're being read, so
    /// they're read once, by whichever lane gets there first).
    pub(crate) apps_sent: std::sync::Mutex<bool>,
}

/// How long the user has to leave things alone before background work runs.
pub(crate) const LULL: Duration = Duration::from_secs(2);
/// How long the refresh holds off for an action before going ahead anyway.
pub(crate) const HOLD_OFF: Duration = Duration::from_millis(1500);
/// How often live TV's channel is read again when nothing suggests it changed.
pub(crate) const CHANNEL_EVERY: Duration = Duration::from_secs(5);

pub(crate) fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Shared {
    pub(crate) fn new(ip: String, stop: Arc<AtomicBool>) -> Shared {
        Shared {
            ip,
            stop,
            pkg: Default::default(),
            guesses: Default::default(),
            searchable: Default::default(),
            busy: Default::default(),
            input_at: std::sync::Mutex::new(Instant::now()),
            channel_stale: AtomicBool::new(true),
            shown: AtomicBool::new(false),
            ambilight: AtomicBool::new(false),
            preview: AtomicBool::new(false),
            art: Default::default(),

            apps_sent: std::sync::Mutex::new(false),
        }
    }

    pub(crate) fn tv(&self) -> Tv {
        Tv { ip: self.ip.clone() }
    }

    pub(crate) fn stopping(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// The user asked for something.
    pub(crate) fn touched(&self) {
        *lock(&self.input_at) = Instant::now();
        self.channel_stale.store(true, Ordering::Relaxed);
    }

    /// Nothing asked for lately and nothing running: background work may go.
    pub(crate) fn idle(&self) -> bool {
        self.shown.load(Ordering::Relaxed) && self.busy.load(Ordering::Relaxed) == 0 && lock(&self.input_at).elapsed() >= LULL
    }

    /// Wait for a lull; false if quitting meanwhile.
    pub(crate) fn wait_idle(&self) -> bool {
        while !self.idle() {
            if self.stopping() {
                return false;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        !self.stopping()
    }
}

/// Read the launcher's apps and send them, unless that's been done. False if
/// the TV couldn't say (try again later).
pub(crate) fn send_apps(shared: &Shared, tv: &Tv, tx: &Sender<Msg>) -> bool {
    let mut sent = lock(&shared.apps_sent);
    if *sent {
        return true;
    }
    let Some(rows) = app_rows(shared, tv) else { return false };
    let _ = tx.send(Msg::Apps(rows));
    *sent = true;
    true
}

/// The launcher's rows, read from the TV now.
pub(crate) fn app_rows(shared: &Shared, tv: &Tv) -> Option<Vec<Entry>> {
    let pm = tv.shell(&["cmd", "package", "list", "packages"]).ok()?;
    let guesses = apps::guesses(tv);
    *lock(&shared.guesses) = guesses.clone();
    let default_home = apps::default_home(tv).ok().flatten();
    let default_pkg = default_home.as_deref().map(|c| c.split('/').next().unwrap_or(c));
    Some(launcher_entries(&apps::installed_aliases(&pm), &guesses, default_pkg))
}

/// Tell the ui which apps are running.
pub(crate) fn send_running(tv: &Tv, tx: &Sender<Msg>) {
    let cmd = format!("ps -A -o NAME; echo @@; {}", apps::PERSISTENT_CMD);
    if let Ok(out) = tv.shell(&[&cmd]) {
        let (ps, pers) = out.split_once("@@").unwrap_or((&out, ""));
        let _ = tx.send(Msg::Running(apps::running(ps), apps::persistent(pers)));
    }
}

/// The refresh lane: what's on, every [`REFRESH`], or sooner when woken
/// (after an action). Holds off while an action runs.
/// An app's name and kind, if known.
type Who = Option<(String, apps::Kind)>;

/// How often the refresh asks again which NetTV app is showing.
const NAMED_EVERY: Duration = Duration::from_secs(30);

pub(crate) struct Refresher {
    pub(crate) tv: Tv,
    pub(crate) shared: Arc<Shared>,
    /// Live TV's channel, and when it was read.
    pub(crate) channel: Option<(crate::provider::Channel, Instant)>,
    /// The app's better name ([`nowplaying::named`]): for which package,
    /// when it was read, and the name.
    pub(crate) named: Option<(String, Instant, Who)>,
    /// The last window dump that said what's in front: a busy TV's dumpsys
    /// sometimes answers with nothing, which isn't "nothing in front".
    pub(crate) last_window: String,
}

impl Refresher {
    pub(crate) fn run(mut self, wake: &Receiver<()>, tx: Sender<Msg>) {
        // every REFRESH, or when woken; until the action lane has gone
        while let Ok(()) | Err(RecvTimeoutError::Timeout) = wake.recv_timeout(REFRESH) {
            // several wake-ups at once: one refresh
            while wake.try_recv().is_ok() {}
            let held = Instant::now();
            while self.shared.busy.load(Ordering::Relaxed) > 0 && held.elapsed() < HOLD_OFF && !self.shared.stopping() {
                std::thread::sleep(Duration::from_millis(20));
            }
            if self.shared.stopping() {
                break;
            }
            let msg = match self.snapshot() {
                Ok(s) => Msg::Snap(Box::new(s), Instant::now()),
                // e.g. while it reboots; keep trying every second
                Err(e) => Msg::Failed(format!("waiting for the TV ({e})")),
            };
            if tx.send(msg).is_err() {
                break;
            }
            self.shared.shown.store(true, Ordering::Relaxed);
        }
        // the port forward goes before the message channel, so the UI can wait for it
        drop(self);
        drop(tx);
    }

    /// Live TV in `pkg`: its channel is read again only now and then, or
    /// when the user may have changed it.
    pub(crate) fn live(&mut self, pkg: &str) -> Option<Activity> {
        let stale = self.shared.channel_stale.swap(false, Ordering::Relaxed);
        if stale || self.channel.as_ref().is_none_or(|(_, at)| at.elapsed() >= CHANNEL_EVERY) {
            self.channel = nowplaying::live_channel(&self.tv, pkg).map(|c| (c, Instant::now()));
        }
        let (ch, _) = self.channel.as_ref()?;
        nowplaying::live_on(&self.tv, pkg, ch)
    }

    /// [`nowplaying::named`], read again every [`NAMED_EVERY`] (it costs an adb call).
    fn named(&mut self, pkg: &str, who: Who) -> Who {
        if !nowplaying::has_namer(pkg) {
            return who;
        }
        match &self.named {
            Some((p, at, name)) if p == pkg && at.elapsed() < NAMED_EVERY => name.clone(),
            _ => {
                let name = nowplaying::named(&self.tv, pkg, who);
                self.named = Some((pkg.to_string(), Instant::now(), name.clone()));
                name
            }
        }
    }

    pub(crate) fn snapshot(&mut self) -> Result<Snapshot> {
        let tv = &self.tv;
        let watch_lights = self.shared.ambilight.load(Ordering::Relaxed);
        let (power, window, media_dump, audio, system, ambilight) = std::thread::scope(|sc| {
            let l = sc.spawn(move || watch_lights.then(|| crate::plugins::ambilight::panel(tv)));
            let p = sc.spawn(|| tv.shell(&["dumpsys", "power"]));
            let w = sc.spawn(|| tv.window_dump());
            let m = sc.spawn(|| tv.shell(&["dumpsys", "media_session"]));
            let y = sc.spawn(|| info::read_system(tv));
            let a = tv.shell(&["dumpsys", "audio"]);
            let j = |h: std::thread::ScopedJoinHandle<'_, Result<String>>| h.join().unwrap_or_else(|_| Ok(String::new()));
            // the system figures only feed the optional panel
            let y = y.join().ok().and_then(|r| r.ok()).unwrap_or_default();
            (j(p), j(w), j(m), a, y, l.join().ok().flatten())
        });
        let (power, mut window, audio) = (power?, window?, audio?);
        if !window.contains("mCurrentFocus") {
            crate::tlog!("refresh: empty window dump ({} bytes); asking again", window.len());
            window = tv.window_dump().ok().filter(|w| w.contains("mCurrentFocus")).unwrap_or_else(|| self.last_window.clone());
        } else {
            self.last_window.clone_from(&window);
        }
        let media_dump = media_dump.unwrap_or_default();
        let awake = power.lines().any(|l| l.trim() == "mWakefulness=Awake");
        let pkg = info::focus_package(&window);
        *lock(&self.shared.pkg) = pkg.clone();
        let sessions = media::parse_media_sessions(&media_dump);
        let sessions = match &pkg {
            Some(p) => media::with_audio_players(sessions, p, info::window_uid(&window, p), &media::parse_audio_players(&audio)),
            None => sessions,
        };
        let guesses = lock(&self.shared.guesses).clone();
        let mut exact = false;
        // live TV: the channel and programme (when the TV can say)
        let mut live = match (&pkg, awake) {
            (Some(p), true) if live_provider().is_some_and(|lp| lp.hosts().contains(&p.as_str())) => self.live(p),
            _ => None,
        };
        let activity = match (&pkg, awake) {
            (_, false) => Some(Activity::just("In standby")),
            (Some(_), true) if live.is_some() => live.take(),
            (Some(pkg), true) => nowplaying::activity_with(pkg, self.named(pkg, apps::identify(pkg, &guesses)), &sessions),
            (None, true) => None,
        };
        // an app with its own player (YouTube, Kodi): exact position, the
        // show, stream details, a link, and a picture for the preview
        let mut art = None;
        let activity = match (pkg.as_deref(), awake) {
            (Some(p), true) => match crate::provider::for_package(p).and_then(|pr| pr.player()) {
                Some(player) => {
                    let (a, playing) = nowplaying::with_player(&self.tv, p, player, activity);
                    exact = a.as_ref().is_some_and(|a| a.progress.is_some());
                    art = playing.and_then(|pl| {
                        let progress = pl.progress;
                        Some(ArtNow {
                            pkg: p.to_string(),
                            art: pl.art?,
                            moves: pl.art_moves,
                            position: progress.map(|x| x.position),
                            paused: progress.is_none_or(|x| x.paused),
                            at: Instant::now(),
                        })
                    });
                    a
                }
                None => activity,
            },
            _ => activity,
        };
        *lock(&self.shared.art) = art;
        Ok(Snapshot {
            activity,
            volume: media::parse_volume(&audio),
            muted: media::parse_muted(&audio),
            exact,
            standby: !awake,
            kind: pkg.as_deref().and_then(|p| {
                let known = apps::by_package(p).is_some();
                apps::identify(p, &guesses).map(|(_, k)| if known { k.as_str().to_string() } else { format!("{}, guessed", k.as_str()) })
            }),
            power: power.lines().find_map(|l| l.trim().strip_prefix("mWakefulness=")).map(str::to_string),
            focus: info::focus_window(&window),
            system,
            ambilight,
        })
    }
}

/// The action lane: what the user did, one at a time, as soon as it comes.
/// Each wakes the refresh when done, so the screen catches up.
pub(crate) struct Actor {
    pub(crate) tv: Tv,
    pub(crate) shared: Arc<Shared>,
    /// What `Action::Run` can run.
    pub(crate) actions: Vec<ActionDescriptor>,
    /// Wakes the refresh.
    pub(crate) wake: Sender<()>,
    /// The read lane (to update the running apps after launching, closing).
    pub(crate) reads: Sender<Action>,
    /// A pairing under way: the TV is showing a PIN.
    pub(crate) pairing: Option<crate::plugins::philips::PairStart>,
}

impl Actor {
    pub(crate) fn run(mut self, actions: &Receiver<Action>, tx: Sender<Msg>) {
        while let Ok(first) = actions.recv() {
            // keys already pressed still go, even when quitting
            let mut queued = vec![first];
            queued.extend(actions.try_iter());
            if self.shared.stopping() {
                queued.retain(|a| *a != Action::Refresh);
            }
            for a in coalesce(queued) {
                self.shared.busy.fetch_add(1, Ordering::Relaxed);
                let result = self.act(a, &tx);
                self.shared.busy.fetch_sub(1, Ordering::Relaxed);
                let _ = self.wake.send(());
                let msg = match result {
                    Ok(note) if note.is_empty() => continue,
                    Ok(note) => Msg::Note(note),
                    Err(e) => Msg::Failed(e.to_string()),
                };
                if tx.send(msg).is_err() {
                    return;
                }
            }
        }
        drop(self);
        drop(tx);
    }

    /// Seek exactly through the app's own player (YouTube, Kodi); None: it
    /// has none, or it can't now (then keys do it).
    pub(crate) fn exact_seek(&mut self, to: SeekTo) -> Option<f64> {
        let pkg = lock(&self.shared.pkg).clone()?;
        crate::provider::for_package(&pkg)?.player()?.seek(&self.tv, to).handled()
    }

    /// Carry out one action; returns status-line feedback.
    pub(crate) fn act(&mut self, a: Action, tx: &Sender<Msg>) -> Result<String> {
        Ok(match a {
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
                let said = p.channels().expect("checked").tune(&crate::provider::Cx { tv: &self.tv }, &ch)?;
                self.shared.channel_stale.store(true, Ordering::Relaxed);
                said
            }
            Action::PlayPause => {
                let pkg = lock(&self.shared.pkg).clone();
                let player = pkg.as_deref().and_then(crate::provider::for_package).and_then(|p| p.player());
                // through the app's own player API when it has one, else the key
                if player.and_then(|p| p.play_pause(&self.tv).handled()).is_none() {
                    self.tv.keyevent(85)?;
                }
                "play/pause".into()
            }
            Action::Volume(0) => "volume".into(),
            Action::Volume(n) => {
                let key = if n > 0 { 24 } else { 25 };
                self.tv.keys(&vec![key; n.unsigned_abs() as usize])?;
                format!("volume {}{}", if n > 0 { "+" } else { "" }, n)
            }
            Action::Mute => {
                let now = media::is_muted(&self.tv)?.unwrap_or(false);
                media::set_mute(&self.tv, !now)?;
                if now { "unmuted".into() } else { "muted".into() }
            }
            Action::MuteKnown(now) => {
                self.tv.keyevent(164)?;
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
                    let key = if d > 0.0 { 90 } else { 89 };
                    self.tv.keys(&vec![key; n as usize])?;
                    format!("skipped ~{:+}s ({n} key presses)", d.signum() * n as f64 * KEY_STEP)
                }
            },
            Action::Jump(f) => match self.exact_seek(SeekTo::Fraction(f)) {
                Some(pos) => {
                    let _ = tx.send(Msg::Seeked(pos, Instant::now()));
                    format!("jumped to {}%: {}", (f * 100.0).round(), clock(pos))
                }
                None => "jumping needs a video tveee can read (currently YouTube)".into(),
            },
            Action::Run { id, args } => match crate::actions::find(&self.actions, id) {
                Some(a) if args.is_empty() && matches!(a.params, Params::Text { .. }) => "that needs some text".into(),
                // one line, for a status line
                Some(a) => a.run(&self.tv, &args)?.text.replace('\n', "; "),
                None => format!("no action '{id}' here"),
            },
            Action::Launch(target) => {
                apps::start(&self.tv, &target)?;
                let _ = self.reads.send(Action::CheckRunning);
                format!("launched {}", app_name(apps::pkg_of(&target)))
            }
            Action::Front(target) => {
                apps::bring_to_front(&self.tv, &target)?;
                format!("brought {} to the front", app_name(apps::pkg_of(&target)))
            }
            Action::Close(pkg) => {
                self.tv.shell(&["am", "force-stop", &pkg])?;
                let _ = self.reads.send(Action::CheckRunning);
                format!("closed {}", app_name(&pkg))
            }
            Action::Restart { pkg, target } => {
                // back where it was, for apps whose session tveee can save
                let said = apps::restart(&self.tv, &pkg, &target, true)?;
                let _ = self.reads.send(Action::CheckRunning);
                said
            }
            Action::Search { pkg, query, keys: true } => search::keys_search(&self.tv, &pkg, &query, false)?,
            Action::Search { pkg, query, keys: false } => {
                let mut found = lock(&self.shared.searchable).clone();
                if found.is_empty() {
                    found = search::searchable(&self.tv)?;
                    *lock(&self.shared.searchable) = found.clone();
                }
                search::search_package(&self.tv, &found, &pkg, &query, false, false)?
            }
            Action::LoadInfo(ch, start) => {
                // the first read takes the TV a while: a thread of its own
                let (tv, tx) = (self.shared.tv(), tx.clone());
                std::thread::spawn(move || {
                    let info = live_provider()
                        .and_then(|p| p.guide())
                        .map_or(Ok(None), |g| g.info(&crate::provider::Cx { tv: &tv }, ch, start))
                        .map_err(|e| e.to_string());
                    let _ = tx.send(Msg::Info((ch, start), info));
                });
                String::new()
            }
            Action::Refresh => "refreshed".into(),
            // other lanes' actions don't come here
            Action::AmbilightChange(change) => crate::plugins::ambilight::apply(&self.tv, &change)?,
            Action::PairStart => {
                self.pairing = Some(crate::plugins::philips::pair_start(&self.tv)?);
                let _ = tx.send(Msg::AskPin);
                "the TV is showing a PIN: type it in".into()
            }
            Action::PairFinish(pin) => {
                let Some(started) = self.pairing.take() else { return Err(crate::error::Error::Failed("pairing: start again (P)".into())) };
                let saved = crate::plugins::philips::pair_finish(&self.tv, &started, &pin)?;
                crate::pairing::save("jointspace", &self.tv.ip, &saved)?;
                "paired with the TV".into()
            }
            Action::Key(_) | Action::Text(_) | Action::LoadGuide | Action::CheckRunning | Action::PrefetchInfo | Action::AmbilightPanel(_) | Action::PreviewPanel(_) => String::new(),
        })
    }
}

/// The read lane: things the user is waiting to see, kept apart from
/// actions so a slow read doesn't hold one up.
pub(crate) fn reads(shared: &Shared, actions: &Receiver<Action>, tx: &Sender<Msg>) {
    let tv = &shared.tv();
    while let Ok(first) = actions.recv() {
        let mut queued = vec![first];
        queued.extend(actions.try_iter());
        queued.dedup();
        for a in queued {
            match a {
                Action::LoadGuide => {
                    let _ = tx.send(Msg::Guide(load_guide(tv).map_err(|e| e.to_string())));
                }
                // the launcher is open: its apps can't wait for a lull
                Action::CheckRunning => std::thread::scope(|sc| {
                    // side by side: neither needs the other
                    sc.spawn(|| send_apps(shared, tv, tx));
                    send_running(tv, tx);
                }),
                _ => {}
            }
        }
    }
}

/// The background lane: work nobody is waiting for, each piece only once the
/// user has paused ([`LULL`]): the launcher's apps and what can be searched,
/// reading the guide ahead of it being opened, and its descriptions when
/// asked to.
pub(crate) fn background(shared: &Shared, asked: &Receiver<Action>, tx: Sender<Msg>) {
    let tv = shared.tv();
    // whether there's an Ambilight panel to offer (one quick, public read)
    if tx.send(Msg::Ambilight(crate::plugins::ambilight::has_ambilight(&tv))).is_err() {
        return;
    }
    // the launcher's apps (unless opening the launcher got them first), then
    // the apps the search box can search
    loop {
        if !shared.wait_idle() {
            return;
        }
        if send_apps(shared, &tv, &tx) {
            break;
        }
        std::thread::sleep(REFRESH);
    }
    if !shared.wait_idle() {
        return;
    }
    let found = search::searchable(&tv).unwrap_or_default();
    *lock(&shared.searchable) = found.clone();
    if tx.send(Msg::Searchable(found)).is_err() {
        return;
    }
    // apps' own names, for those tveee only knows by package: when some
    // turn up, the launcher again with them (on a TV that can't say,
    // nothing changes)
    if !shared.wait_idle() {
        return;
    }
    if crate::labels::refresh(&tv).unwrap_or(0) > 0 {
        if let Some(rows) = app_rows(shared, &tv) {
            let _ = tx.send(Msg::Apps(rows));
        }
    }
    // nothing more to tell the ui: don't keep it waiting on quit
    drop(tx);
    let warm_guide = |tv: &Tv| {
        // the programmes (cached), not the favourites sync that opening the guide does
        if let Some(g) = live_provider().and_then(|p| p.guide()) {
            let _ = g.now_next(&crate::provider::Cx { tv }, crate::plugins::guide::now());
        }
    };
    let mut warmed: Option<Instant> = None;
    loop {
        // asked for (descriptions), or every so often: keep the guide warm
        let a = match asked.recv_timeout(Duration::from_secs(60)) {
            Ok(a) => Some(a),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        if !shared.wait_idle() {
            return;
        }
        match a {
            Some(Action::PrefetchInfo) => {
                // reads (and caches) every description; any one will do to start it
                if let Some(g) = live_provider().and_then(|p| p.guide()) {
                    let _ = g.info(&crate::provider::Cx { tv: &tv }, 0, 0);
                }
            }
            Some(_) => {}
            None if warmed.is_none_or(|w| w.elapsed() >= Duration::from_secs(20 * 60)) => {
                warm_guide(&tv);
                warmed = Some(Instant::now());
            }
            None => {}
        }
    }
}

/// After a push connection ends or can't be made, how long before trying
/// that app again (Kodi not running, say).
const PUSH_RETRY: Duration = Duration::from_secs(10);

/// The push lane: while the app in front can say when playback changes
/// (Kodi, YouTube: [`crate::provider::Player::watch`]), the refresh is woken
/// as soon as it does, rather than up to a second later.
pub(crate) fn push_lane(shared: &Shared, wake: &Sender<()>) {
    use crate::provider::Outcome;
    let tv = shared.tv();
    // the app given up on lately, and when
    let mut resting: Option<(String, Instant)> = None;
    while !shared.stopping() {
        let pkg = lock(&shared.pkg).clone();
        let player = pkg.as_deref().and_then(crate::provider::for_package).and_then(|p| p.player());
        let (Some(pkg), Some(player)) = (pkg, player) else {
            std::thread::sleep(REFRESH);
            continue;
        };
        if resting.as_ref().is_some_and(|(p, at)| *p == pkg && at.elapsed() < PUSH_RETRY) {
            std::thread::sleep(REFRESH);
            continue;
        }
        let still = || !shared.stopping() && lock(&shared.pkg).as_deref() == Some(pkg.as_str());
        let changed = |what: &str| {
            crate::tlog!("push: {pkg}: {what}");
            let _ = wake.send(());
        };
        let outcome = player.watch(&tv, &changed, &still);
        if let Outcome::Failed(e) = &outcome {
            crate::tlog!("push: {pkg}: {e}");
        }
        // it ended with the app still in front: it can't push now
        if still() {
            resting = Some((pkg, Instant::now()));
        }
    }
}

use super::{info, media};
use crate::error::{failed, usage, Result};
use crate::registry::Command;
use crate::tv::Tv;
use crate::Ctx;

const CAT: &str = "apps";

/// What an app is for; decides how `status` describes it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Video,
    /// Music and podcasts: "Listening to ...".
    Music,
    /// Plays files and links (VLC, MX Player, ...).
    Player,
    Store,
    Launcher,
    LiveTv,
    Other,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Video => "video",
            Kind::Music => "music",
            Kind::Player => "player",
            Kind::Store => "store",
            Kind::Launcher => "home",
            Kind::LiveTv => "live-tv",
            Kind::Other => "app",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        [Kind::Video, Kind::Music, Kind::Player, Kind::Store, Kind::Launcher, Kind::LiveTv, Kind::Other]
            .into_iter()
            .find(|k| k.as_str() == s)
    }
}

/// An app tveee knows about. Apps with an alias can be launched by it; the
/// rest are only recognised in the foreground. The core lists general apps
/// ([`CORE`]); plugins add their own (YouTube, Philips' NetTV apps).
#[derive(Clone, Copy)]
pub struct App {
    pub alias: Option<&'static str>,
    /// Human name, e.g. "YouTube".
    pub name: &'static str,
    /// package, package/activity, or `@home` for the default launcher.
    pub target: &'static str,
    pub kind: Kind,
}

impl App {
    pub fn package(&self) -> &'static str {
        self.target.split('/').next().unwrap_or_default()
    }
}

pub const fn app(alias: &'static str, name: &'static str, target: &'static str, kind: Kind) -> App {
    App { alias: Some(alias), name, target, kind }
}

/// Recognised in the foreground only (no alias).
pub const fn known(name: &'static str, package: &'static str, kind: Kind) -> App {
    App { alias: None, name, target: package, kind }
}

/// The general apps. Plugins add theirs after `home` (see [`registry`]).
pub const CORE: &[App] = &[
    app("home", "the home screen", "@home", Kind::Launcher),
    app("kodi", "Kodi", "org.xbmc.kodi", Kind::Video),
    app("appletv", "Apple TV", "com.apple.atve.androidtv.appletv", Kind::Video),
    app("netflix", "Netflix", "com.netflix.ninja", Kind::Video),
    app("mubi", "MUBI", "com.mubi", Kind::Video),
    app("prime", "Prime Video", "com.amazon.amazonvideo.livingroom", Kind::Video),
    app("playstore", "the Play Store", "com.android.vending", Kind::Store),
    app("fdroid", "F-Droid", "org.fdroid.fdroid", Kind::Store),
    app("vlc", "VLC", "org.videolan.vlc", Kind::Player),
    app("mx", "MX Player", "com.mxtech.videoplayer.pro", Kind::Player),
    // streaming apps installed on this TV (checked with `tveee apps --others`)
    app("channel4", "Channel 4", "com.channel4.ondemand", Kind::Video),
    app("vimeo", "Vimeo", "com.vimeo.android.videoapp", Kind::Video),
    app("twitch", "Twitch", "tv.twitch.android.app", Kind::Video),
    // other common Android TV streaming apps, by their usual package names.
    // Not checked on a TV: an entry whose package isn't installed never
    // matches, and `apps` / the launcher only list installed ones.
    app("my5", "My5", "com.mobileiq.demand5", Kind::Video),
    app("disney", "Disney+", "com.disney.disneyplus", Kind::Video),
    app("max", "Max", "com.wbd.stream", Kind::Video),
    app("paramount", "Paramount+", "com.cbs.ott", Kind::Video),
    app("peacock", "Peacock", "com.peacocktv.peacockandroid", Kind::Video),
    app("hulu", "Hulu", "com.hulu.livingroomplus", Kind::Video),
    app("dazn", "DAZN", "com.dazn", Kind::Video),
    app("crunchyroll", "Crunchyroll", "com.crunchyroll.crunchyroid", Kind::Video),
    app("rakuten", "Rakuten TV", "tv.wuaki", Kind::Video),
    app("pluto", "Pluto TV", "tv.pluto.android", Kind::Video),
    app("tubi", "Tubi", "com.tubitv", Kind::Video),
    app("ytkids", "YouTube Kids", "com.google.android.youtube.tvkids", Kind::Video),
    app("plex", "Plex", "com.plexapp.android", Kind::Video),
    app("jellyfin", "Jellyfin", "org.jellyfin.androidtv", Kind::Video),
    app("emby", "Emby", "tv.emby.embyatv", Kind::Video),
    app("spotify", "Spotify", "com.spotify.tv.android", Kind::Music),
    app("ytmusic", "YouTube Music", "com.google.android.youtube.tvmusic", Kind::Music),
    app("projectivy", "the home screen", "com.spocky.projengmenu", Kind::Launcher),
    app("tvlauncher", "the home screen", "com.google.android.tvlauncher", Kind::Launcher),
    known("the home screen", "com.google.android.leanbacklauncher", Kind::Launcher),
];

static REGISTRY: std::sync::OnceLock<Vec<App>> = std::sync::OnceLock::new();

/// The core apps with plugins' apps (`extra`) placed after `home`, so they
/// list near the top.
pub fn registry(extra: Vec<App>) -> Vec<App> {
    let mut v = vec![CORE[0]];
    v.extend(extra);
    v.extend_from_slice(&CORE[1..]);
    v
}

/// Set the apps of the enabled plugins. Call once, before anything looks an
/// app up (main does).
pub fn install(plugin_apps: Vec<App>) {
    let _ = REGISTRY.set(registry(plugin_apps));
}

/// Every app tveee knows. Without [`install`] (tests) that's every plugin's.
pub fn all() -> &'static [App] {
    REGISTRY.get_or_init(|| registry(crate::plugins::all().iter().flat_map(|p| (p.apps)()).collect()))
}

/// "the home screen" -> "Home screen": app names are written for sentences.
pub fn list_name(name: &str) -> String {
    match name.strip_prefix("the ") {
        Some(n) => {
            let mut c = n.chars();
            c.next().map_or_else(String::new, |f| f.to_uppercase().chain(c).collect())
        }
        // sentence-style names ("live TV") get a capital; brand spellings
        // ("iPlayer") are one word and are left alone
        None if name.contains(' ') && name.starts_with(|c: char| c.is_lowercase()) => {
            let mut c = name.chars();
            c.next().map_or_else(String::new, |f| f.to_uppercase().chain(c).collect())
        }
        None => name.to_string(),
    }
}


pub fn is_alias(name: &str) -> bool {
    by_alias(name).is_some()
}

/// An app the TV has that isn't in [`all`], with its kind worked out from
/// what it declares.
#[derive(Debug, Clone, PartialEq)]
pub struct Guess {
    pub package: String,
    pub kind: Kind,
    /// Its TV-launcher activity, if it shows in the launcher.
    pub component: Option<String>,
}

/// Players, app stores, home screens and TV-launcher apps, in one round trip.
const GUESS_CMD: &str = "\
cmd package query-activities --brief -a android.intent.action.MAIN -c android.intent.category.HOME; echo @@; \
cmd package query-activities --brief -a android.intent.action.MAIN -c android.intent.category.APP_MARKET; echo @@; \
cmd package query-activities --brief -a android.intent.action.VIEW -d file:///x -t video/mp4; echo @@; \
cmd package query-activities --brief -a android.intent.action.VIEW -d file:///x -t audio/mpeg; echo @@; \
cmd package query-activities --brief -a android.intent.action.MAIN -c android.intent.category.LEANBACK_LAUNCHER";

/// Placeholders that claim everything; never worth naming.
const NOT_APPS: &[&str] = &["android", "com.google.android.tv.frameworkpackagestubs"];

/// Work out kinds from [`GUESS_CMD`] output. A home screen that can also
/// show videos (as a wallpaper) is still a home screen: home > store > player.
pub fn parse_guesses(out: &str) -> Vec<Guess> {
    let parts: Vec<Vec<String>> = out.split("@@").map(super::open::parse_candidates).collect();
    let part = |i: usize| parts.get(i).cloned().unwrap_or_default();
    let pkg = |c: &String| c.split('/').next().unwrap_or(c).to_string();
    let (homes, stores) = (part(0), part(1));
    let players: Vec<String> = part(2).into_iter().chain(part(3)).collect();
    let mut out: Vec<Guess> = Vec::new();
    let mut add = |p: String, kind: Kind, component: Option<String>| {
        if NOT_APPS.contains(&p.as_str()) {
            return;
        }
        match out.iter_mut().find(|g| g.package == p) {
            Some(g) => g.component = g.component.take().or(component),
            None => out.push(Guess { package: p, kind, component }),
        }
    };
    for c in &homes {
        add(pkg(c), Kind::Launcher, None);
    }
    for c in &stores {
        add(pkg(c), Kind::Store, None);
    }
    for c in &players {
        add(pkg(c), Kind::Player, None);
    }
    for c in part(4) {
        add(pkg(&c), Kind::Other, Some(c.clone()));
    }
    out
}

fn guess_cache(ip: &str) -> Option<std::path::PathBuf> {
    crate::plugin::cache_path(ip).map(|p| p.with_extension("apps"))
}

/// Cache lines: `kind<TAB>package<TAB>component-or--`.
pub fn format_guesses(gs: &[Guess]) -> String {
    gs.iter()
        .map(|g| format!("{}\t{}\t{}\n", g.kind.as_str(), g.package, g.component.as_deref().unwrap_or("-")))
        .collect()
}

pub fn read_guesses(s: &str) -> Vec<Guess> {
    s.lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            let kind = Kind::parse(f.next()?)?;
            let package = f.next()?.to_string();
            let component = f.next().filter(|c| *c != "-").map(str::to_string);
            Some(Guess { package, kind, component })
        })
        .collect()
}

const GUESS_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

/// The guessed apps, from the per-TV cache when it's under a day old, else
/// asked from the TV (and cached). Never fails: no answer means no guesses.
pub fn guesses(tv: &Tv) -> Vec<Guess> {
    let cache = guess_cache(&tv.ip);
    let fresh = |path: &std::path::Path| -> Option<String> {
        let age = std::fs::metadata(path).ok()?.modified().ok()?.elapsed().ok()?;
        if age >= GUESS_MAX_AGE {
            return None;
        }
        std::fs::read_to_string(path).ok()
    };
    if let Some(text) = cache.as_deref().and_then(fresh) {
        return read_guesses(&text);
    }
    let Ok(out) = tv.shell(&[GUESS_CMD]) else { return Vec::new() };
    let gs = parse_guesses(&out);
    if let Some(path) = cache {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, format_guesses(&gs));
    }
    gs
}

/// A name and kind for `pkg`: from [`all`] if tveee knows it, else guessed
/// (named by its package).
pub fn identify(pkg: &str, guessed: &[Guess]) -> Option<(String, Kind)> {
    if let Some(a) = by_package(pkg) {
        return Some((a.name.to_string(), a.kind));
    }
    guessed.iter().find(|g| g.package == pkg).map(|g| (pkg.to_string(), g.kind))
}

fn by_alias(alias: &str) -> Option<&'static App> {
    all().iter().find(|a| a.alias == Some(alias))
}

/// The app whose package is `pkg`, if tveee knows it.
pub fn by_package(pkg: &str) -> Option<&'static App> {
    all().iter().find(|a| a.target != "@home" && a.package() == pkg)
}

/// Idle apps that hog RAM; force-stopped by `free`.
const HOGS: &[&str] = &[
    "com.netflix.ninja",
    "com.android.vending",
    "com.mubi",
    "com.google.android.youtube.tv",
    "org.fdroid.fdroid",
    "com.apple.atve.androidtv.appletv",
    "com.amazon.amazonvideo.livingroom",
    "com.amazon.aiv.eu",
];

/// Whether the TV passes through `pkg` on the way from one app to another:
/// a home screen tveee knows, or Android's own screens.
#[cfg_attr(not(feature = "ui"), allow(dead_code))]
pub fn passes_through(pkg: &str) -> bool {
    pkg == "android" || pkg == "com.android.systemui" || by_package(pkg).is_some_and(|a| a.kind == Kind::Launcher)
}

/// Never force-stopped by `quit` without --force: launchers, live TV, system UI.
const PROTECTED: &[&str] = &[
    "com.spocky.projengmenu",
    "com.google.android.tvlauncher",
    "com.google.android.leanbacklauncher",
    "org.droidtv.zapster",
    "com.android.systemui",
    "android",
];

const LAUNCHER: &str = "android.intent.category.LAUNCHER";

/// Alias lookup; anything else is taken as a package or package/activity.
pub fn target(app: &str) -> &str {
    by_alias(app).map_or(app, |a| a.target)
}

pub fn pkg_of(app: &str) -> &str {
    target(app).split('/').next().unwrap_or_default()
}

fn free_ram(tv: &Tv) -> Result<()> {
    for p in HOGS {
        tv.shell(&["am", "force-stop", p])?;
    }
    if let Some(r) = info::read_ram(tv)? {
        println!("ram: {r}");
    }
    Ok(())
}

fn free(c: &Ctx, _: &[String]) -> Result<()> {
    c.tv.connect()?;
    free_ram(&c.tv)
}

fn launch(c: &Ctx, args: &[String]) -> Result<()> {
    let (mut free, mut quiet) = (false, false);
    let mut rest = args;
    while let Some(a) = rest.first().filter(|a| a.starts_with("--")) {
        match a.as_str() {
            "--free" => free = true,
            "--quiet" => quiet = true,
            o => return usage(format!("launch: unknown option '{o}'")),
        }
        rest = &rest[1..];
    }
    let Some(app) = rest.first() else { return usage("launch: missing app") };
    let tv = &c.tv;
    tv.connect()?;
    if free {
        free_ram(tv)?;
    }
    if quiet {
        media::set_mute(tv, true)?;
    }
    let out = start(tv, app)?;
    print!("{out}");
    Ok(())
}

const TV_LAUNCHER: &str = "android.intent.category.LEANBACK_LAUNCHER";

/// The activity that opens `pkg`: its TV-launcher entry, else its ordinary
/// launcher entry. Some TV apps (Prime Video, Channel 4, Vimeo) only have the
/// first.
pub fn launch_activity(tv: &Tv, pkg: &str) -> Result<Option<String>> {
    for category in [TV_LAUNCHER, LAUNCHER] {
        let out = tv.shell(&["cmd", "package", "resolve-activity", "--brief", "-c", category, pkg])?;
        if let Some(c) = super::open::parse_resolved(&out) {
            return Ok(Some(c));
        }
    }
    Ok(None)
}

/// Start an app by alias, package or package/activity; returns what the TV
/// said. Doesn't print, so the ui can use it too.
pub fn start(tv: &Tv, app: &str) -> Result<String> {
    let t = target(app);
    let out = if t == "@home" {
        tv.keyevent(3)?;
        String::new()
    } else if t.contains('/') {
        tv.shell(&["am", "start", "-n", t])?
    } else {
        match launch_activity(tv, t)? {
            Some(component) => tv.shell(&["am", "start", "-n", &component])?,
            None => return failed(format!("launch {app}: {t} has no launcher activity (is it installed?)")),
        }
    };
    // am and monkey report failure on stdout with exit status 0
    if let Some(err) = out.lines().find(|l| l.starts_with("Error") || l.contains("No activities found")) {
        return failed(format!("launch {app}: {}", err.trim()));
    }
    Ok(out)
}

const HOME: &str = "android.intent.category.HOME";

/// The installed home apps (launchers), as `package/activity`.
pub fn home_activities(tv: &Tv) -> Result<Vec<String>> {
    let out = tv.shell(&["cmd", "package", "query-activities", "--brief", "-a", "android.intent.action.MAIN", "-c", HOME])?;
    Ok(super::open::parse_candidates(&out))
}

/// The default home activity, or None when there isn't one (Android then
/// asks which to use whenever Home is pressed).
pub fn default_home(tv: &Tv) -> Result<Option<String>> {
    let out = tv.shell(&["cmd", "package", "resolve-activity", "--brief", "-a", "android.intent.action.MAIN", "-c", HOME])?;
    Ok(super::open::parse_resolved(&out).filter(|c| !c.contains("ResolverActivity")))
}

/// Which of `homes` the user means by `app` (alias, package, or package/activity).
pub fn pick_home<'a>(homes: &'a [String], app: &str) -> Option<&'a String> {
    let t = target(app);
    homes.iter().find(|h| h.as_str() == t).or_else(|| homes.iter().find(|h| h.split('/').next() == Some(pkg_of(app))))
}

/// A home app's name for lists: its alias and package.
fn home_label(component: &str) -> String {
    let pkg = component.split('/').next().unwrap_or(component);
    match by_package(pkg).and_then(|a| a.alias) {
        Some(alias) => format!("{alias:<12} {pkg}"),
        None => format!("{:<12} {pkg}", "-"),
    }
}

/// `home [--list | --set <app>]`.
pub fn home(c: &Ctx, args: &[String]) -> Result<()> {
    let tv = &c.tv;
    match args.first().map(String::as_str) {
        None => {
            tv.key(3)?;
            if default_home(tv)?.is_none() {
                eprintln!("note: no default home app is set, so the TV asks which to use; set one with 'tveee home --set <app>' (see 'tveee home --list')");
            }
            Ok(())
        }
        Some("--list") => {
            tv.connect()?;
            let def = default_home(tv)?;
            for h in home_activities(tv)? {
                let mark = if def.as_deref() == Some(h.as_str()) { "*" } else { " " };
                println!("{mark} {}", home_label(&h));
            }
            if def.is_none() {
                println!("
no default set: the TV asks which to use when Home is pressed ('tveee home --set <app>')");
            }
            Ok(())
        }
        Some("--set") => {
            let Some(app) = args.get(1) else { return usage("usage: tveee home --set <app>  (see 'tveee home --list')") };
            tv.connect()?;
            let homes = home_activities(tv)?;
            let Some(component) = pick_home(&homes, app) else {
                return failed(format!("home: {app} isn't a home app on this TV (see 'tveee home --list')"));
            };
            let out = tv.shell(&["cmd", "package", "set-home-activity", component])?;
            if out.contains("Error") || out.contains("Exception") {
                return failed(format!("home: {}", out.trim()));
            }
            match default_home(tv)? {
                Some(now) if now == *component => {
                    println!("default home app is now {}", home_label(component).split_whitespace().next_back().unwrap_or(component));
                    Ok(())
                }
                other => failed(format!("home: asked for {component}, but the default is {}", other.as_deref().unwrap_or("still unset"))),
            }
        }
        Some(x) => usage(format!("home: unknown option '{x}' (usage: tveee home [--list | --set <app>])")),
    }
}

/// Aliases whose app is installed (`pm list packages` output); `home` always.
#[cfg_attr(not(feature = "ui"), allow(dead_code))]
pub fn installed_aliases(pm_list: &str) -> Vec<&'static App> {
    let installed: Vec<&str> = pm_list.lines().filter_map(|l| l.trim().strip_prefix("package:")).collect();
    all().iter()
        .filter(|a| a.alias.is_some())
        .filter(|a| a.target == "@home" || installed.contains(&a.package()))
        .collect()
}

fn foreground(c: &Ctx, args: &[String]) -> Result<()> {
    let Some(app) = args.first() else { return usage("foreground: missing app") };
    c.tv.connect()?;
    print!("{}", bring_to_front(&c.tv, app)?);
    Ok(())
}

/// Get `pkg` ready to be sent a link: apps whose provider needs it
/// (`fresh_for_links`) are closed first if they're running. Returns whether
/// it closed the app.
pub fn before_link(tv: &Tv, pkg: &str) -> Result<bool> {
    if !crate::provider::for_package(pkg).is_some_and(|p| p.fresh_for_links()) {
        return Ok(false);
    }
    // pidof exits 1 when the app isn't running
    let running = tv.shell(&["pidof", pkg]).map(|o| !o.trim().is_empty()).unwrap_or(false);
    if running {
        tv.shell(&["am", "force-stop", pkg])?;
    }
    Ok(running)
}

/// Send `url` to `pkg` in place when its provider can (an app already in
/// front): Ok(true), and the caller has nothing more to do. Otherwise the app
/// is got ready for a link ([`before_link`]) and the caller sends it.
pub fn link_here_or_prepare(tv: &Tv, pkg: &str, url: &str) -> Result<bool> {
    if crate::provider::for_package(pkg).is_some_and(|p| p.open_here(tv, url).unwrap_or(false)) {
        return Ok(true);
    }
    before_link(tv, pkg)?;
    Ok(false)
}

/// Bring an app's existing task to the front (starting it if it isn't
/// running); returns what the TV said. Doesn't print, so the ui can use it.
pub fn bring_to_front(tv: &Tv, app: &str) -> Result<String> {
    let t = target(app);
    if t == "@home" {
        tv.keyevent(3)?;
        return Ok(String::new());
    }
    let component = if t.contains('/') {
        t.to_string()
    } else {
        match launch_activity(tv, t)? {
            Some(c) => c,
            None => return failed(format!("foreground: no launcher activity for {app}")),
        }
    };
    // REORDER_TO_FRONT: reuse the existing instance instead of recreating it
    let out = tv.shell(&["am", "start", "-f", "0x20000", "-n", &component])?;
    if let Some(err) = out.lines().find(|l| l.starts_with("Error")) {
        return failed(format!("foreground {app}: {}", err.trim()));
    }
    Ok(out)
}

/// The app packages with a running process, from `ps -A -o NAME` (an app's
/// extra processes, `pkg:name`, count as the app).
#[cfg_attr(not(feature = "ui"), allow(dead_code))]
pub fn running(ps: &str) -> std::collections::BTreeSet<String> {
    ps.lines()
        .skip(1)
        .map(str::trim)
        .filter(|n| n.contains('.') && !n.starts_with('[') && !n.starts_with('/'))
        .map(|n| n.split(':').next().unwrap_or(n).to_string())
        .collect()
}

/// The command whose output [`persistent`] reads.
#[cfg_attr(not(feature = "ui"), allow(dead_code))]
pub const PERSISTENT_CMD: &str = "dumpsys activity processes | grep '[*]PERS[*]'";

/// Packages Android keeps running whatever happens ("persistent": live TV,
/// system UI, ...): a force-stop is undone straight away, so they can't be
/// closed. From [`PERSISTENT_CMD`]'s lines, e.g.
/// `*PERS* UID 1000 ProcessRecord{fc3af50 2241:org.droidtv.zapster/1000}`.
#[cfg_attr(not(feature = "ui"), allow(dead_code))]
pub fn persistent(dump: &str) -> std::collections::BTreeSet<String> {
    dump.lines()
        .filter(|l| l.trim_start().starts_with("*PERS*"))
        .filter_map(|l| {
            let rec = l.split("ProcessRecord{").nth(1)?;
            let name = rec.split_whitespace().nth(1)?.split_once(':')?.1;
            let name = name.split('/').next()?;
            Some(name.split(':').next().unwrap_or(name).to_string())
        })
        .collect()
}

/// Aliases (other than `home`) that point at `pkg`.
pub fn aliases_for_package(pkg: &str) -> Vec<&'static str> {
    all().iter()
        .filter(|a| a.target != "@home" && a.package() == pkg)
        .filter_map(|a| a.alias)
        .collect()
}

fn currentapp(c: &Ctx, _: &[String]) -> Result<()> {
    c.tv.connect()?;
    let dump = c.tv.window_dump()?;
    let Some(window) = info::focus_window(&dump) else {
        return failed("currentapp: can't tell what is in the foreground");
    };
    let pkg = window.split('/').next().unwrap_or(&window);
    match aliases_for_package(pkg).as_slice() {
        [] => println!("{window}"),
        names => println!("{} ({window})", names.join("/")),
    }
    Ok(())
}

fn kill(c: &Ctx, args: &[String]) -> Result<()> {
    let Some(app) = args.first() else { return usage("kill: missing app") };
    if target(app) == "@home" {
        return usage("kill: 'home' is not an app");
    }
    c.tv.connect()?;
    c.tv.shell(&["am", "force-stop", pkg_of(app)]).map(drop)
}

/// Whether `quit` may stop `pkg`, given the default launcher's package.
pub fn quit_allowed(pkg: &str, home: &str, force: bool) -> bool {
    force || !(pkg == home || PROTECTED.contains(&pkg))
}

fn quit(c: &Ctx, args: &[String]) -> Result<()> {
    let force = match args.first().map(String::as_str) {
        None => false,
        Some("--force") => true,
        Some(_) => return usage("usage: tveee quit [--force]"),
    };
    let tv = &c.tv;
    tv.connect()?;
    let Some(pkg) = info::focus_package(&tv.window_dump()?) else {
        return failed("quit: can't tell what is in the foreground");
    };
    // the default launcher may not be in PROTECTED
    let home = tv.shell(&[
        "cmd", "package", "resolve-activity", "--brief", "-a", "android.intent.action.MAIN",
        "-c", "android.intent.category.HOME",
    ])?;
    let home = home.lines().last().unwrap_or_default().trim();
    let home = home.split('/').next().unwrap_or_default();
    if !quit_allowed(&pkg, home, force) {
        return failed(format!("quit: {pkg} is the home screen / live TV, not quitting (use --force)"));
    }
    println!("quitting {pkg}");
    tv.shell(&["am", "force-stop", &pkg]).map(drop)
}

fn apps(c: &Ctx, args: &[String]) -> Result<()> {
    let mut all = match args.first().map(String::as_str) {
        None => false,
        Some("--all") => true,
        Some("--others") => return others(c),
        Some(_) => return usage("usage: tveee apps [--all | --others]"),
    };
    let mut installed = Vec::new();
    if !all {
        c.tv.try_connect();
        let out = c.tv.shell(&["cmd", "package", "list", "packages"]).unwrap_or_default();
        installed = out.lines().filter_map(|l| l.strip_prefix("package:")).map(str::to_string).collect();
        if installed.is_empty() {
            eprintln!("tveee apps: can't reach the TV, showing every alias");
            all = true;
        }
    }
    let mut rows: Vec<_> = self::all()
        .iter()
        .filter_map(|a| Some((a.alias?, a.target)))
        // home is always available; others must be installed on the TV
        .filter(|(a, t)| all || *t == "@home" || installed.iter().any(|p| p == pkg_of(a)))
        .collect();
    rows.sort();
    for (a, t) in rows {
        println!("{a:<12} {t}");
    }
    Ok(())
}

/// `apps --others`: installed apps without an alias, with their guessed kind.
fn others(c: &Ctx) -> Result<()> {
    c.tv.connect()?;
    let mut gs: Vec<Guess> = guesses(&c.tv).into_iter().filter(|g| by_package(&g.package).is_none_or(|a| a.alias.is_none())).collect();
    let kind_of = |g: &Guess| by_package(&g.package).map_or(g.kind, |a| a.kind).as_str();
    gs.sort_by(|a, b| (kind_of(a), &a.package).cmp(&(kind_of(b), &b.package)));
    for g in gs {
        let launchable = if g.component.is_some() { "" } else { "  (not in the TV launcher)" };
        // tveee's own table wins over the guess (live TV, the NetTV browser)
        println!("{:<8} {}{launchable}", kind_of(&g), g.package);
    }
    Ok(())
}

/// Menu items for the ui: open a link (search has its own key, /).
pub fn menu() -> Vec<crate::menu::MenuItem> {
    super::open::menu()
}

pub fn commands() -> Vec<Command> {
    let mut cmds = super::open::commands();
    cmds.extend(super::search::commands());
    cmds.extend([
        Command::new(CAT, "launch", "[--free] [--quiet] <app>", "launch an app", launch).long(
            "<app> is an alias (see 'tveee apps'), a package name, or package/activity.\n  'home' goes to the home screen (same as 'tveee home').\n  --free   run 'tveee free' first\n  --quiet  mute the TV first",
        ),
        Command::new(CAT, "foreground", "<app>", "bring an app's existing task to the front", foreground)
            .long("Same app naming as 'launch'. Starts the app if it is not running.")
            .aliases(&["fg"]),
        Command::new(CAT, "quit", "[--force]", "quit the current app", quit).long(
            "Force-stops whatever app is in the foreground. Refuses on the home screen,\nlive TV and system UI unless --force is given.",
        ),
        Command::new(CAT, "currentapp", "", "show the app currently in the foreground", currentapp).long(
            "Prints the foreground window, with the app alias first if it has one,\ne.g. 'kodi (org.xbmc.kodi/.Splash)'. The NetTV webapps (iPlayer, Arte, ...) all run\ninside org.droidtv.nettvbrowser, so they show as that package.",
        ),
        Command::new(CAT, "kill", "<app>", "force-stop one app", kill),
        Command::new(CAT, "free", "", "force-stop idle RAM-hungry apps", free),
        Command::new(CAT, "apps", "[--all | --others]", "list app aliases installed on the TV", apps).long(
            "Queries the TV and hides aliases whose package isn't installed.\n  --all     list every alias without contacting the TV\n  --others  installed apps without an alias, with the kind tveee guesses\n            for them (player, store, home, app); cached for a day",
        ),
    ]);
    cmds
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_aliases_and_packages() {
        assert_eq!(pkg_of("iplayer"), "org.droidtv.nettvapp4340");
        assert_eq!(target("kodi"), "org.xbmc.kodi");
        assert_eq!(pkg_of("com.example/.Main"), "com.example");
        assert_eq!(target("home"), "@home");
    }

    #[test]
    fn finds_aliases_by_package() {
        assert_eq!(aliases_for_package("org.xbmc.kodi"), vec!["kodi"]);
        assert_eq!(aliases_for_package("org.droidtv.nettvapp4340"), vec!["iplayer"]);
        assert!(aliases_for_package("org.droidtv.nettvbrowser").is_empty());
        assert!(aliases_for_package("@home").is_empty());
    }

    #[test]
    fn finds_apps_by_package() {
        assert_eq!(by_package("com.google.android.youtube.tv").unwrap().name, "YouTube");
        assert_eq!(by_package("org.droidtv.zapster").unwrap().kind, Kind::LiveTv);
        assert!(by_package("com.example.unknown").is_none());
    }

    #[test]
    fn app_table_is_consistent() {
        let mut aliases: Vec<_> = all().iter().filter_map(|a| a.alias).collect();
        let n = aliases.len();
        aliases.sort();
        aliases.dedup();
        assert_eq!(aliases.len(), n, "duplicate alias");
        // one entry per package (several home screens share a name, not a package)
        let mut pkgs: Vec<_> = all().iter().filter(|a| a.target != "@home").map(|a| a.package()).collect();
        let n = pkgs.len();
        pkgs.sort();
        pkgs.dedup();
        assert_eq!(pkgs.len(), n, "duplicate package");
        // aliases are what people type: short, lowercase, no spaces
        for a in all().iter().filter_map(|a| a.alias) {
            assert!(a.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()), "{a}");
        }
        for k in [Kind::Video, Kind::Music, Kind::Player, Kind::Store, Kind::Launcher, Kind::LiveTv, Kind::Other] {
            assert_eq!(Kind::parse(k.as_str()), Some(k));
        }
    }

    #[test]
    fn guesses_kinds() {
        let out = "3 activities found:
  Activity #0:
    priority=0
    com.google.android.tvlauncher/.MainActivity
  Activity #1:
    com.spocky.projengmenu/.ui.home.MainActivity
@@
    com.android.vending/com.google.android.finsky.tvmainactivity.TvMainActivity
@@
    com.spocky.projengmenu/.ui.launcherActivities.SetBackgroundActivity
    nextapp.fx/.ui.player.MediaPlayerActivity
    com.google.android.tv.frameworkpackagestubs/.Stubs$MediaStub
@@
    nextapp.fx/.ui.player.MediaPlayerActivity
@@
    nextapp.fx/.ui.Launcher
    com.example.game/.Main
";
        let g = parse_guesses(out);
        let find = |p: &str| g.iter().find(|x| x.package == p).cloned();
        // a home screen that can also show video wallpapers is still home
        assert_eq!(find("com.spocky.projengmenu").unwrap().kind, Kind::Launcher);
        assert_eq!(find("com.android.vending").unwrap().kind, Kind::Store);
        let fx = find("nextapp.fx").unwrap();
        assert_eq!((fx.kind, fx.component.as_deref()), (Kind::Player, Some("nextapp.fx/.ui.Launcher")));
        assert_eq!(find("com.example.game").unwrap().kind, Kind::Other);
        assert!(find("com.google.android.tv.frameworkpackagestubs").is_none());
        // cache round trip
        assert_eq!(read_guesses(&format_guesses(&g)), g);
        // known apps keep their names; others are named by package
        assert_eq!(identify("org.videolan.vlc", &g), Some(("VLC".into(), Kind::Player)));
        assert_eq!(identify("nextapp.fx", &g), Some(("nextapp.fx".into(), Kind::Player)));
        assert_eq!(identify("com.nowhere", &g), None);
    }

    #[test]
    fn reads_running_apps() {
        let ps = "NAME\ninit\n[kthreadd]\n/system/bin/adbd\ncom.spocky.projengmenu\norg.droidtv.nettvbrowser:privileged_process0\ncom.netflix.ninja\n";
        let r = running(ps);
        assert!(r.contains("com.spocky.projengmenu") && r.contains("com.netflix.ninja"));
        assert!(r.contains("org.droidtv.nettvbrowser"));
        assert_eq!(r.len(), 3);
    }

    #[test]
    fn picks_home_apps() {
        let homes = vec![
            "com.google.android.tvlauncher/.MainActivity".to_string(),
            "com.spocky.projengmenu/.ui.home.MainActivity".to_string(),
            "com.android.tv.settings/.system.FallbackHome".to_string(),
        ];
        assert_eq!(pick_home(&homes, "projectivy").map(String::as_str), Some("com.spocky.projengmenu/.ui.home.MainActivity"));
        assert_eq!(pick_home(&homes, "tvlauncher").map(String::as_str), Some("com.google.android.tvlauncher/.MainActivity"));
        assert_eq!(pick_home(&homes, "com.android.tv.settings").map(String::as_str), Some("com.android.tv.settings/.system.FallbackHome"));
        assert_eq!(pick_home(&homes, "com.spocky.projengmenu/.ui.home.MainActivity"), Some(&homes[1]));
        assert_eq!(pick_home(&homes, "kodi"), None);
    }

    #[test]
    fn lists_installed_aliases() {
        let pm = "package:com.google.android.youtube.tv\npackage:org.videolan.vlc\npackage:org.droidtv.zapster\npackage:com.nowhere\n";
        let names: Vec<_> = installed_aliases(pm).iter().filter_map(|a| a.alias).collect();
        // home is always there; the rest only if installed
        // plugin apps (livetv from philips, youtube) come right after home
        assert_eq!(names, vec!["home", "livetv", "youtube", "vlc"]);
    }

    #[test]
    fn reads_persistent_processes() {
        let dump = "  *PERS* UID 1000 ProcessRecord{fc3af50 2241:org.droidtv.zapster/1000}\n\
                    \x20 *PERS* UID 1000 ProcessRecord{572f70d 1722:system/1000}\n\
                    \x20 *PERS* UID 10030 ProcessRecord{437f33e 1989:com.android.systemui/u0a30}\n\
                    \x20 *PERS* UID 10083 ProcessRecord{26080f5 3223:org.droidtv.nettvapps:remote/u0a83}\n\
                    \x20   PERS #45: pers  T/ /PERU trm: 0 2241:org.droidtv.zapster/1000 (pers-top-activity)\n";
        let got: Vec<String> = persistent(dump).into_iter().collect();
        assert_eq!(got, ["com.android.systemui", "org.droidtv.nettvapps", "org.droidtv.zapster", "system"]);
    }

    #[test]
    fn quit_protects_home_and_live_tv() {
        assert!(!quit_allowed("com.spocky.projengmenu", "x", false));
        assert!(!quit_allowed("org.droidtv.zapster", "x", false));
        assert!(!quit_allowed("com.custom.launcher", "com.custom.launcher", false));
        assert!(quit_allowed("org.xbmc.kodi", "com.spocky.projengmenu", false));
        assert!(quit_allowed("org.droidtv.zapster", "x", true));
    }
}

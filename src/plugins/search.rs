//! `search`: hand a query to an app's own search, via Android's search
//! intents (or a search-results link for apps that only take links).

use super::apps;
use super::open::{normalise, parse_candidates, sh_quote};
use crate::error::{failed, usage, Result};
use crate::actions::ActionDescriptor;
use crate::tv::Tv;
use crate::Ctx;

const SEARCH: &str = "android.intent.action.SEARCH";
/// Google's assistant flavour of SEARCH (Kodi has it).
const GMS_SEARCH: &str = "com.google.android.gms.actions.SEARCH_ACTION";
const PLAY: &str = "android.media.action.MEDIA_PLAY_FROM_SEARCH";

/// System packages that answer search intents but aren't apps you'd search in.
const NOISE: &[&str] = &[
    "com.android.cts.ctsshim",
    "com.android.cts.priv.ctsshim",
    "com.google.android.gms",
    "com.google.android.tv.frameworkpackagestubs",
];

/// How an app can be searched.
#[derive(Debug, Clone, PartialEq)]
pub enum Way {
    /// `SEARCH` (or the gms action) to this activity, with the query as `query`.
    Intent { action: &'static str, component: String },
    /// Open this link with `{q}` filled in.
    Link(&'static str),
}

/// One searchable app.
#[derive(Debug, Clone, PartialEq)]
pub struct Searchable {
    pub package: String,
    pub way: Way,
    /// Also takes "play from search" (play the best match straight away).
    pub play: Option<String>,
}

const QUERY_CMD: &str = "\
cmd package query-activities --brief -a android.intent.action.SEARCH; echo @@; \
cmd package query-activities --brief -a com.google.android.gms.actions.SEARCH_ACTION; echo @@; \
cmd package query-activities --brief -a android.media.action.MEDIA_PLAY_FROM_SEARCH";

/// Of several search activities in one app, the one most likely to be its
/// search screen: a name with "Search" in it, then one with "TV", else the first.
fn best(components: &[&String]) -> Option<String> {
    let name = |c: &&&String| c.rsplit(['/', '.']).next().unwrap_or("").to_string();
    components
        .iter()
        .find(|c| name(c).contains("Search"))
        .or_else(|| components.iter().find(|c| name(c).starts_with("TV")))
        .or(components.first())
        .map(|c| (*c).clone())
}

/// The components in `list` that belong to package `p`.
fn mine<'a>(list: &'a [String], p: &str) -> Vec<&'a String> {
    list.iter().filter(|c| c.split('/').next() == Some(p)).collect()
}

/// Searchable apps from [`QUERY_CMD`] output plus the link-searchable apps
/// that are installed (`installed`: `pm list packages` output, or None to skip).
pub fn parse_searchable(out: &str, installed: Option<&str>) -> Vec<Searchable> {
    let parts: Vec<Vec<String>> = out.split("@@").map(parse_candidates).collect();
    let part = |i: usize| parts.get(i).cloned().unwrap_or_default();
    let pkg_of = |c: &String| c.split('/').next().unwrap_or(c).to_string();
    let (search, gms, play) = (part(0), part(1), part(2));
    let mut pkgs: Vec<String> = search.iter().chain(&gms).chain(&play).map(pkg_of).collect();
    pkgs.dedup();
    let mut seen = Vec::new();
    let mut out: Vec<Searchable> = Vec::new();
    for p in pkgs {
        if NOISE.contains(&p.as_str()) || seen.contains(&p) {
            continue;
        }
        seen.push(p.clone());
        let way = match (best(&mine(&search, &p)), best(&mine(&gms, &p))) {
            (Some(component), _) => Way::Intent { action: SEARCH, component },
            (None, Some(component)) => Way::Intent { action: GMS_SEARCH, component },
            // play-from-search only: still searchable, by playing
            (None, None) => match best(&mine(&play, &p)) {
                Some(component) => Way::Intent { action: PLAY, component },
                None => continue,
            },
        };
        out.push(Searchable { package: p.clone(), way, play: best(&mine(&play, &p)) });
    }
    if let Some(pm) = installed {
        let have: Vec<&str> = pm.lines().filter_map(|l| l.trim().strip_prefix("package:")).collect();
        for p in crate::provider::all() {
            let Some(link) = p.search_link() else { continue };
            for pkg in p.hosts() {
                if have.contains(pkg) && !out.iter().any(|s| s.package == *pkg) {
                    out.push(Searchable { package: pkg.to_string(), way: Way::Link(link), play: None });
                }
            }
        }
    }
    out
}

pub fn searchable(tv: &Tv) -> Result<Vec<Searchable>> {
    let out = tv.shell(&[QUERY_CMD])?;
    let pm = tv.shell(&["cmd", "package", "list", "packages"])?;
    Ok(parse_searchable(&out, Some(&pm)))
}

/// `%`-encode a query for a link.
/// Spaces are `%20`, not `+`: the YouTube TV app passes a `+` through as a
/// literal plus (searching for "a+b"), which also left its player stuck
/// behind the search page when a result was opened.
pub fn url_encode(q: &str) -> String {
    q.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The `am start` command line that runs this search.
pub fn search_command(s: &Searchable, query: &str, play: bool) -> Result<String> {
    if play {
        let Some(component) = &s.play else {
            return failed(format!("search: {} can't play from a search; leave out --play", label(&s.package)));
        };
        return Ok(format!("am start -a {PLAY} -n {} --es query {}", sh_quote(component), sh_quote(query)));
    }
    Ok(match &s.way {
        Way::Intent { action, component } => {
            format!("am start -a {action} -n {} --es query {}", sh_quote(component), sh_quote(query))
        }
        Way::Link(template) => {
            let url = normalise(&template.replace("{q}", &url_encode(query)));
            format!("am start -a android.intent.action.VIEW -d {} -p {}", sh_quote(&url), sh_quote(&s.package))
        }
    })
}

/// "YouTube (youtube)", "Live TV (livetv)" or the package.
fn label(pkg: &str) -> String {
    match apps::by_package(pkg) {
        Some(a) => match a.alias {
            Some(alias) => format!("{} ({alias})", apps::list_name(a.name)),
            None => apps::list_name(a.name),
        },
        None => pkg.to_string(),
    }
}

/// Search `pkg` using its search intent or link (`all` from [`searchable`]).
/// Returns status-line feedback.
pub fn search_package(tv: &Tv, all: &[Searchable], pkg: &str, query: &str, play: bool, dry: bool) -> Result<String> {
    let Some(s) = all.iter().find(|s| s.package == pkg) else {
        return failed(format!(
            "search: {} doesn't support search (see 'tveee search --list', or try --keys)",
            label(pkg)
        ));
    };
    let cmd = search_command(s, query, play)?;
    if dry {
        return Ok(format!("would run: {cmd}"));
    }
    let here = match (&s.way, play) {
        (Way::Link(template), false) => {
            super::apps::link_here_or_prepare(tv, pkg, &normalise(&template.replace("{q}", &url_encode(query))))?
        }
        _ => {
            super::apps::before_link(tv, pkg)?;
            false
        }
    };
    if here {
        return Ok(format!("searching {} for \"{query}\"", label(pkg)));
    }
    let out = tv.shell(&[&cmd])?;
    if let Some(err) = out.lines().find(|l| l.starts_with("Error")) {
        return failed(format!("search: {err}"));
    }
    Ok(format!("searching {} for \"{query}\"", label(pkg)))
}

const KEY_SEARCH: u32 = 84;
const KEY_ENTER: u32 = 66;

/// Wait until `pkg` is the foreground app, up to `secs`.
fn wait_for_front(tv: &Tv, pkg: &str, secs: u64) -> Result<bool> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    while std::time::Instant::now() < deadline {
        if super::info::focus_package(&tv.window_dump()?).as_deref() == Some(pkg) {
            return Ok(true);
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    Ok(false)
}

/// The keys fallback, for apps with no search intent: bring the app to the
/// front, press the search key, type the query, press Enter. Best effort:
/// many TV apps ignore the search key (it may open the TV's own search).
pub fn keys_search(tv: &Tv, app: &str, query: &str, dry: bool) -> Result<String> {
    let pkg = apps::pkg_of(app);
    let typed = super::input::escape_text(query);
    if dry {
        return Ok(format!(
            "would bring {pkg} to the front, then: input keyevent {KEY_SEARCH}; input text {typed}; input keyevent {KEY_ENTER}"
        ));
    }
    let in_front = super::info::focus_package(&tv.window_dump()?).as_deref() == Some(pkg);
    if !in_front {
        apps::start(tv, app)?;
        if !wait_for_front(tv, pkg, 20)? {
            return failed(format!("search: {} didn't come to the front", label(pkg)));
        }
        // give a just-started app a moment to draw its first screen
        std::thread::sleep(std::time::Duration::from_secs(3));
    }
    tv.keyevent(KEY_SEARCH)?;
    std::thread::sleep(std::time::Duration::from_millis(1500));
    tv.shell(&["input", "text", &typed])?;
    tv.keyevent(KEY_ENTER)?;
    Ok(format!("typed \"{query}\" into {}'s search (best effort: check the TV)", label(pkg)))
}

/// How `pkg` would be searched: by intent/link, or only by typing keys.
#[cfg_attr(not(feature = "ui"), allow(dead_code))]
pub fn method(all: &[Searchable], pkg: &str) -> &'static str {
    match all.iter().find(|s| s.package == pkg).map(|s| &s.way) {
        Some(Way::Link(_)) => "results link",
        Some(_) => "search",
        None => "keys",
    }
}

fn run(tv: &Tv, app: &str, query: &str, play: bool, keys: bool, dry: bool) -> Result<String> {
    if query.is_empty() {
        return usage(format!("search: what should {app} search for?"));
    }
    if keys {
        return keys_search(tv, app, query, dry);
    }
    search_package(tv, &searchable(tv)?, apps::pkg_of(app), query, play, dry)
}

fn list(tv: &Tv) -> Result<()> {
    let mut all = searchable(tv)?;
    all.sort_by_key(|s| (apps::by_package(&s.package).and_then(|a| a.alias).is_none(), label(&s.package).to_lowercase()));
    for s in all {
        let how = match (&s.way, &s.play) {
            (Way::Link(_), _) => "results link",
            (Way::Intent { action: PLAY, .. }, _) => "play only",
            (_, Some(_)) => "search, play",
            _ => "search",
        };
        println!("{:<32} {how}", label(&s.package));
    }
    Ok(())
}

const USAGE: &str = "[--list] | [--play | --keys] [--dry-run] <app> <query...>";

fn search(c: &Ctx, args: &[String]) -> Result<()> {
    let (mut play, mut keys, mut dry, mut words) = (false, false, false, Vec::new());
    for a in args {
        match a.as_str() {
            "--list" => return list(&c.tv),
            "--play" => play = true,
            "--keys" => keys = true,
            "--dry-run" => dry = true,
            f if f.starts_with("--") => return usage(format!("search: unknown option '{f}'")),
            w => words.push(w),
        }
    }
    match words.split_first() {
        None => list(&c.tv),
        Some((app, query)) => {
            if play && keys {
                return usage("search: --play and --keys don't go together");
            }
            println!("{}", run(&c.tv, app, &query.join(" "), play, keys, dry)?);
            Ok(())
        }
    }
}

pub fn actions() -> Vec<ActionDescriptor> {
    vec![ActionDescriptor::terminal("search.search", "apps", "search inside an app (YouTube, Kodi, live TV, ...)", search).cli("search", USAGE, "search inside an app (YouTube, Kodi, live TV, ...)").long(
        "Hands the query to the app's own search screen, using Android's search\n\
         intent (or a search-results link for YouTube, which only takes links).\n\
         With no arguments, or --list, shows which installed apps support it.\n\
         \n\
         \x20 tveee search youtube cat videos\n\
         \x20 tveee search livetv news          (the TV guide's programme search)\n\
         \x20 tveee search --play vlc jazz       play the best match straight away\n\
         \x20 tveee search --keys netflix dune   for apps without search: open the app, press\n\
         \x20                                     the search key, type, Enter (best effort)\n\
         \x20 --dry-run  show what would be sent\n\
         \n\
         Netflix, Prime and most streaming apps don't accept search from outside, so\n\
         only --keys can reach them, and only if the app answers the search key.",
    )]
}

#[cfg(test)]
mod tests {
    use super::*;

    const OUT: &str = "
    com.android.cts.priv.ctsshim/.InstallPriority
    com.android.vending/com.google.android.finsky.tvmainactivity.TvMainActivity
    org.xbmc.kodi/.XBMCSearchableActivity
    org.droidtv.zapster/.playpvr.activity.RecPlayActivity2
    org.droidtv.zapster/.playtv.activity.SearchActivity
    org.droidtv.zapster/.playtv.activity.EPGSearchActivity
    com.mxtech.videoplayer.pro/.ActivityMediaList
    com.mxtech.videoplayer.pro/.TVActivityMediaList
    org.videolan.vlc/.StartActivity
    com.google.android.tv.frameworkpackagestubs/.Stubs$MediaStub
@@
    org.xbmc.kodi/.XBMCSearchableActivity
    com.example.voice/.Search
@@
    org.videolan.vlc/.StartActivity
";

    #[test]
    fn finds_searchable_apps() {
        let pm = "package:com.google.android.youtube.tv\npackage:org.videolan.vlc\n";
        let all = parse_searchable(OUT, Some(pm));
        let find = |p: &str| all.iter().find(|s| s.package == p).cloned();
        assert!(find("com.google.android.gms").is_none() && find("com.android.cts.priv.ctsshim").is_none());
        // the activity named for search wins in apps with several
        assert_eq!(
            find("org.droidtv.zapster").unwrap().way,
            Way::Intent { action: SEARCH, component: "org.droidtv.zapster/.playtv.activity.SearchActivity".into() }
        );
        assert_eq!(
            find("com.mxtech.videoplayer.pro").unwrap().way,
            Way::Intent { action: SEARCH, component: "com.mxtech.videoplayer.pro/.TVActivityMediaList".into() }
        );
        assert_eq!(find("org.videolan.vlc").unwrap().play.as_deref(), Some("org.videolan.vlc/.StartActivity"));
        assert_eq!(find("com.example.voice").unwrap().way, Way::Intent { action: GMS_SEARCH, component: "com.example.voice/.Search".into() });
        // YouTube by link, only because it's installed
        assert_eq!(find("com.google.android.youtube.tv").unwrap().way, Way::Link("https://www.youtube.com/results?search_query={q}"));
        assert!(parse_searchable(OUT, Some("")).iter().all(|s| s.package != "com.google.android.youtube.tv"));
    }

    #[test]
    fn builds_commands() {
        let all = parse_searchable(OUT, Some("package:com.google.android.youtube.tv\n"));
        let get = |p: &str| all.iter().find(|s| s.package == p).unwrap();
        assert_eq!(
            search_command(get("org.xbmc.kodi"), "star trek", false).unwrap(),
            "am start -a android.intent.action.SEARCH -n 'org.xbmc.kodi/.XBMCSearchableActivity' --es query 'star trek'"
        );
        assert_eq!(
            search_command(get("com.google.android.youtube.tv"), "cats & dogs", false).unwrap(),
            "am start -a android.intent.action.VIEW -d 'https://www.youtube.com/results?search_query=cats%20%26%20dogs' -p 'com.google.android.youtube.tv'"
        );
        assert_eq!(
            search_command(get("org.videolan.vlc"), "it's jazz", true).unwrap(),
            "am start -a android.media.action.MEDIA_PLAY_FROM_SEARCH -n 'org.videolan.vlc/.StartActivity' --es query 'it'\\''s jazz'"
        );
        assert!(search_command(get("org.xbmc.kodi"), "x", true).is_err());
        assert_eq!(url_encode("a b/c?é"), "a%20b%2Fc%3F%C3%A9");
        // a real plus stays a plus, encoded
        assert_eq!(url_encode("c++"), "c%2B%2B");
    }
}

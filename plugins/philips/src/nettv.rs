//! NetTV: Philips' web apps (iPlayer, Arte, ...), each a small wrapper
//! package (`org.droidtv.nettvapp<N>`) that opens a link in the NetTV
//! browser (`org.droidtv.nettvbrowser`, a WebView). For working on them and
//! on wrappers of your own: list the wrappers, open any link in the browser,
//! look into its pages and run JavaScript there (when the firmware lets its
//! WebView be debugged), read its log, and install or take off wrapper APKs.

use super::NETTV_BROWSER;
use tveee_core::actions::ActionDescriptor;
use tveee_core::builtin::open::sh_quote;
use tveee_core::devtools;
use tveee_core::error::{Error, Result, failed, usage};
use tveee_core::provider::{Outcome, Playing};
use tveee_core::tv::Tv;

/// What every NetTV wrapper package's name starts with.
pub const WRAPPER_PREFIX: &str = "org.droidtv.nettvapp";

const VIEW: &str = "android.intent.action.VIEW";

// --- the wrappers ------------------------------------------------------------

/// A NetTV wrapper package on the TV.
#[derive(Debug, Clone, PartialEq)]
pub struct Wrapper {
    /// Its package: "org.droidtv.nettvapp4340".
    pub package: String,
    /// Where its APK is: "/system/app/..." (came with the TV), "/data/app/..." (installed).
    pub path: String,
    /// Its version code, when Android says.
    pub version: Option<String>,
}

impl Wrapper {
    /// Installed by someone (in /data), not part of the TV's software:
    /// those can be uninstalled.
    pub fn user_installed(&self) -> bool {
        self.path.starts_with("/data/")
    }

    /// Its name, if tveee knows the app ("iPlayer").
    pub fn name(&self) -> Option<&'static str> {
        super::apps().into_iter().find(|a| a.package() == self.package).map(|a| a.name)
    }
}

/// The packages in `pm list packages -f [--show-versioncode]` output:
/// `package:/system/app/X/X.apk=org.droidtv.nettvapp4340 versionCode:12`.
/// The path can hold `=`, the package can't: split on the last one.
pub fn parse_packages(out: &str) -> Vec<Wrapper> {
    out.lines()
        .filter_map(|l| {
            let rest = l.trim().strip_prefix("package:")?;
            let (rest, version) = match rest.split_once(" versionCode:") {
                Some((r, v)) => (r, Some(v.trim().to_string())),
                None => (rest, None),
            };
            let (path, package) = rest.rsplit_once('=')?;
            Some(Wrapper { package: package.trim().to_string(), path: path.to_string(), version })
        })
        .collect()
}

/// `pm list packages -f`, with version codes where Android has the option
/// (9 and later); `flags` adds more (`-3`: installed ones only).
fn packages(tv: &Tv, flags: &str, filter: &str) -> Result<Vec<Wrapper>> {
    let with = format!("pm list packages -f {flags} --show-versioncode {filter}");
    let out = match tv.shell(&[&with]) {
        Ok(o) => o,
        Err(_) => tv.shell(&[&format!("pm list packages -f {flags} {filter}")])?,
    };
    let mut found = parse_packages(&out);
    found.sort_by(|a, b| a.package.cmp(&b.package));
    Ok(found)
}

/// Those whose package, name or path has every word of `q` (any case).
pub fn matching<'a>(all: &'a [Wrapper], q: &str) -> Vec<&'a Wrapper> {
    let words: Vec<String> = q.split_whitespace().map(str::to_lowercase).collect();
    all.iter()
        .filter(|w| {
            let text = format!("{} {} {}", w.package, w.name().unwrap_or(""), w.path).to_lowercase();
            words.iter().all(|x| text.contains(x.as_str()))
        })
        .collect()
}

/// A line per wrapper: name, package, version, and installed (or built in).
pub fn lines(found: &[&Wrapper]) -> String {
    let w = found.iter().map(|x| x.name().unwrap_or("?").chars().count()).max().unwrap_or(0);
    found
        .iter()
        .map(|x| {
            let version = x.version.as_deref().map(|v| format!(" v{v}")).unwrap_or_default();
            let whose = if x.user_installed() { "installed" } else { "built in" };
            format!("{:<w$}  {}{version}  ({whose})", x.name().unwrap_or("?"), x.package)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `tveee nettv-apps [words]`.
fn apps_cmd(tv: &Tv, args: &[String]) -> Result<String> {
    let q = args.join(" ");
    let all = packages(tv, "", WRAPPER_PREFIX)?;
    let found = matching(&all, &q);
    if found.is_empty() {
        return Ok(if q.is_empty() { "no NetTV apps on the TV".into() } else { format!("no NetTV apps match \"{q}\"") });
    }
    Ok(lines(&found))
}

// --- opening a link ----------------------------------------------------------

/// `-a VIEW -d '<url>'`, ready for `am start` or a lookup.
pub fn view_args(url: &str) -> String {
    format!("-a {VIEW} -d {}", sh_quote(url))
}

/// The browser activity the wrappers start, from `dumpsys activity
/// activities`: `... cmp=org.droidtv.nettvbrowser/.Main ...`.
pub fn started_component(dump: &str) -> Option<String> {
    let at = dump.find(&format!("cmp={NETTV_BROWSER}/"))? + "cmp=".len();
    let c: String = dump[at..].chars().take_while(|c| !c.is_whitespace() && *c != '}').collect();
    Some(c)
}

/// The browser activity to send a link to: the one that takes VIEW links,
/// else the one the wrappers started last, else its launcher activity.
fn browser_component(tv: &Tv, url: &str) -> Result<String> {
    let resolve = format!("cmd package resolve-activity --brief {} -p {NETTV_BROWSER}", view_args(url));
    if let Some(c) = tv.shell(&[&resolve]).ok().and_then(|o| tveee_core::builtin::open::parse_resolved(&o)) {
        return Ok(c);
    }
    let grep = format!("dumpsys activity activities | grep -m1 -o 'cmp={NETTV_BROWSER}/[^ }}]*'");
    if let Some(c) = tv.shell(&[&grep]).ok().and_then(|o| started_component(&o)) {
        return Ok(c);
    }
    match tveee_core::builtin::apps::launch_activity(tv, NETTV_BROWSER)? {
        Some(c) => Ok(c),
        None => failed(format!("nettv-open: the NetTV browser ({NETTV_BROWSER}) isn't on this TV")),
    }
}

/// The `am start` line that opens `url` in `component`.
pub fn start_line(component: &str, url: &str) -> String {
    format!("am start -n {} {}", sh_quote(component), view_args(url))
}

/// A web link: http(s), with something after the scheme.
pub fn web_link(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    ["http://", "https://"].iter().any(|s| lower.strip_prefix(s).is_some_and(|rest| !rest.is_empty()))
}

/// `tveee nettv-open <url>`.
fn open_cmd(tv: &Tv, args: &[String]) -> Result<String> {
    let [url] = args else { return usage("usage: tveee nettv-open <url>") };
    let url = tveee_core::builtin::open::normalise(url);
    if !web_link(&url) {
        return usage(format!("nettv-open: '{url}' isn't a web link (http:// or https://)"));
    }
    let component = browser_component(tv, &url)?;
    let out = tv.shell(&[&start_line(&component, &url)])?;
    if let Some(err) = out.lines().find(|l| l.starts_with("Error")) {
        return failed(format!("nettv-open: {err}"));
    }
    Ok(format!("opened {url} in NetTV ({component})"))
}

// --- the browser's pages (devtools) -----------------------------------------

/// The NetTV browser's process and its WebView's devtools socket. Not
/// running: Unavailable, said. Running with no socket: the firmware doesn't
/// let its WebView be debugged.
fn socket(tv: &Tv) -> Result<String> {
    let out = tv.shell(&[&format!("pidof {NETTV_BROWSER}; echo @@; grep devtools_remote /proc/net/unix")]).unwrap_or_default();
    let (pids, unix) = out.split_once("@@").unwrap_or((&out, ""));
    if pids.trim().is_empty() {
        return Err(Error::Unavailable("the NetTV browser isn't running (open a NetTV app, or 'tveee nettv-open <url>')".into()));
    }
    match pids.split_whitespace().find_map(|p| devtools::webview_socket(unix, p)) {
        Some(s) => Ok(s),
        None => Err(Error::Unavailable(format!(
            "the NetTV browser (pid {}) has no devtools socket: this TV's firmware doesn't let its WebView be debugged",
            pids.trim()
        ))),
    }
}

/// Run `f` with a devtools connection to the NetTV browser's WebView (only
/// its own socket: never another app's).
fn with_browser<T>(tv: &Tv, f: impl Fn(&devtools::Conn) -> Result<T>) -> Result<T> {
    socket(tv)?;
    devtools::with_conn(tv, NETTV_BROWSER, f)
}

/// A JavaScript string literal of `s` (JSON's, with the two line
/// separators older engines end strings at escaped too).
pub fn js_string(s: &str) -> String {
    serde_json::Value::from(s).to_string().replace('\u{2028}', "\\u2028").replace('\u{2029}', "\\u2029")
}

/// What a page is: its title, address and whether it has loaded.
pub const PAGE_EXPR: &str = "JSON.stringify({title:document.title,url:location.href,ready:document.readyState})";

/// A line for a page from [`PAGE_EXPR`]'s answer (or the url alone).
pub fn page_line(url: &str, answer: Option<&str>) -> String {
    let v = answer.and_then(tveee_core::http::json).unwrap_or_default();
    let text = |k: &str| tveee_core::http::text(&v, k).unwrap_or_default();
    let url = Some(text("url")).filter(|u| !u.is_empty()).unwrap_or_else(|| url.to_string());
    match (text("title"), text("ready")) {
        (t, r) if t.is_empty() && r.is_empty() => format!("{url}  (didn't answer)"),
        (t, r) => format!("{}  [{r}]\n  {url}", if t.is_empty() { "(no title)".into() } else { t }),
    }
}

/// `tveee nettv-inspect`.
fn inspect_cmd(tv: &Tv) -> Result<String> {
    let sock = socket(tv)?;
    let pages = with_browser(tv, |c| c.eval_each(PAGE_EXPR))?;
    if pages.is_empty() {
        return Ok(format!("@{sock}: no pages open"));
    }
    let shown: Vec<String> = pages.iter().map(|(url, r)| page_line(url, r.as_deref().ok())).collect();
    Ok(format!("@{sock}: {} page(s)\n{}", pages.len(), shown.join("\n")))
}

/// JavaScript that runs `src` (with an indirect eval, so in the page's
/// global scope) and answers `{"ok":true,"v":"..."}` or `{"ok":false,"e":"..."}`:
/// strings as they are, anything else as JSON (or String() when it isn't).
pub fn eval_expr(src: &str) -> String {
    format!(
        "(function(){{try{{var v=(0,eval)({});if(v===undefined)v='undefined';else if(typeof v!=='string'){{\
         try{{var j=JSON.stringify(v);v=j===undefined?String(v):j}}catch(_){{v=String(v)}}}}\
         return JSON.stringify({{ok:true,v:v}})}}catch(e){{return JSON.stringify({{ok:false,e:String(e)}})}}}})()",
        js_string(src)
    )
}

/// [`eval_expr`]'s answer: the value, or the page's error.
pub fn eval_answer(raw: &str) -> Result<String> {
    let v = tveee_core::http::json(raw).unwrap_or_default();
    match v.get("ok").and_then(serde_json::Value::as_bool) {
        Some(true) => Ok(tveee_core::http::text(&v, "v").unwrap_or_default()),
        Some(false) => failed(format!("nettv-eval: {}", tveee_core::http::text(&v, "e").unwrap_or_default())),
        None => failed(format!("nettv-eval: an answer it doesn't understand: {raw}")),
    }
}

/// `tveee nettv-eval <js>`.
fn eval_cmd(tv: &Tv, args: &[String]) -> Result<String> {
    let src = args.join(" ");
    if src.trim().is_empty() {
        return usage("usage: tveee nettv-eval <javascript>");
    }
    eval_answer(&with_browser(tv, |c| c.eval(&eval_expr(&src)))?)
}

// --- the log -----------------------------------------------------------------

/// Words that mark a NetTV line in logcat (any case).
const LOG_WORDS: &[&str] = &["nettvbrowser", "droidtv.nettvapp", "chromium", "webview", "zeasn", "console"];

/// The process id of a `logcat -v time` line: `11-28 10:00:00.000 D/tag( 1234): ...`.
pub fn log_pid(line: &str) -> Option<&str> {
    let open = line.find('(')?;
    let close = open + line[open..].find("):")?;
    Some(line[open + 1..close].trim()).filter(|p| p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty())
}

/// The last `max` lines of a `logcat -v time` dump that are NetTV's (from
/// the browser's processes, or naming it, a wrapper, Chromium or WebView)
/// and have every word of `filter` (any case).
pub fn nettv_lines<'a>(log: &'a str, pids: &[&str], filter: &str, max: usize) -> Vec<&'a str> {
    let words: Vec<String> = filter.split_whitespace().map(str::to_lowercase).collect();
    let hits: Vec<&str> = log
        .lines()
        .filter(|l| {
            let lower = l.to_lowercase();
            let ours = log_pid(l).is_some_and(|p| pids.contains(&p)) || LOG_WORDS.iter().any(|w| lower.contains(w));
            ours && words.iter().all(|w| lower.contains(w.as_str()))
        })
        .collect();
    hits[hits.len().saturating_sub(max)..].to_vec()
}

/// `tveee nettv-log [words]`.
fn log_cmd(tv: &Tv, args: &[String]) -> Result<String> {
    let pids = tv.shell(&["pidof", NETTV_BROWSER]).unwrap_or_default();
    let pids: Vec<&str> = pids.split_whitespace().collect();
    let log = tv.adb(&["logcat", "-d", "-v", "time"])?;
    let found = nettv_lines(&log, &pids, &args.join(" "), 200);
    if found.is_empty() {
        return Ok("no NetTV lines in the TV's log".into());
    }
    Ok(found.join("\n"))
}

// --- installing and uninstalling wrappers ------------------------------------

/// The packages whose APK is new or moved between two listings: what an
/// install put in (an update moves it to a new folder too).
pub fn changed(before: &[Wrapper], after: &[Wrapper]) -> Vec<String> {
    after.iter().filter(|a| !before.iter().any(|b| b.package == a.package && b.path == a.path)).map(|a| a.package.clone()).collect()
}

/// `tveee nettv-install <file.apk>`.
fn install_cmd(tv: &Tv, args: &[String]) -> Result<String> {
    let path = args.join(" ");
    let path = path.trim();
    if path.is_empty() {
        return usage("usage: tveee nettv-install <file.apk>");
    }
    let p = std::path::Path::new(path);
    if !p.is_file() {
        return failed(format!("nettv-install: {path}: no such file here"));
    }
    if !p.extension().is_some_and(|e| e.eq_ignore_ascii_case("apk")) {
        return failed(format!("nettv-install: {path} isn't an .apk"));
    }
    let before = packages(tv, "-3", "")?;
    let out = tv.adb(&["install", "-r", path])?;
    if let Some(err) = out.lines().find(|l| l.contains("Failure") || l.starts_with("Error")) {
        return failed(format!("nettv-install: {}", err.trim()));
    }
    let after = packages(tv, "-3", "")?;
    let new = changed(&before, &after);
    let Some(pkg) = new.first() else { return Ok(format!("installed {path} (Android shows no new package: the same APK again?)")) };
    let what = if before.iter().any(|b| b.package == *pkg) { "updated" } else { "installed" };
    let note = if pkg.starts_with(WRAPPER_PREFIX) { String::new() } else { format!("\n(it isn't named {WRAPPER_PREFIX}..., so nettv-apps won't list it)") };
    Ok(format!("{what} {pkg}{note}"))
}

/// Why `pkg` can't be uninstalled here, if it can't: only wrappers
/// (`org.droidtv.nettvapp...`), and only ones someone installed (`installed`:
/// the TV's third-party packages), never the TV's own.
pub fn uninstall_refusal(pkg: &str, installed: &[Wrapper]) -> Option<String> {
    if !pkg.starts_with(WRAPPER_PREFIX) {
        return Some(format!("only NetTV apps ({WRAPPER_PREFIX}...), not {pkg}"));
    }
    if !pkg.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_') {
        return Some(format!("{pkg} isn't a package name"));
    }
    match installed.iter().find(|w| w.package == pkg) {
        Some(w) if w.user_installed() => None,
        Some(w) => Some(format!("{pkg} is part of the TV's software ({}): it can't be uninstalled", w.path)),
        None => Some(format!("{pkg} isn't installed (or came with the TV, and can't be uninstalled)")),
    }
}

/// `tveee nettv-uninstall <package>`.
fn uninstall_cmd(tv: &Tv, args: &[String]) -> Result<String> {
    let [pkg] = args else { return usage("usage: tveee nettv-uninstall <package>") };
    if let Some(why) = uninstall_refusal(pkg, &packages(tv, "-3", WRAPPER_PREFIX)?) {
        return failed(format!("nettv-uninstall: {why}"));
    }
    let out = tv.shell(&["pm", "uninstall", pkg])?;
    if !out.contains("Success") {
        return failed(format!("nettv-uninstall: {}", out.trim()));
    }
    Ok(format!("uninstalled {pkg}"))
}

// --- the provider: links in place, what's playing ----------------------------

/// The NetTV browser's provider: with its WebView debuggable, links opened
/// in the page it's showing, and what its video is doing.
pub struct NetTv;
/// The NetTV browser (the web apps' host).
pub static NETTV: NetTv = NetTv;

/// The page's title and address, and its longest video's `[position,
/// duration, paused]` (null without one).
pub const PLAYING_EXPR: &str = "(function(){var vs=Array.prototype.slice.call(document.querySelectorAll('video')).filter(function(v){return v.readyState>0});\
vs.sort(function(a,b){return (b.duration||0)-(a.duration||0)});var v=vs[0];\
return JSON.stringify({title:document.title,url:location.href,video:v?[v.currentTime,isFinite(v.duration)?v.duration:null,v.paused]:null})})()";

/// [`PLAYING_EXPR`]'s answer as what's playing; None without a video.
pub fn parse_playing(raw: &str) -> Option<Playing> {
    let v = tveee_core::http::json(raw)?;
    let progress = devtools::parse_progress(&v.get("video")?.to_string())?;
    let title = tveee_core::http::text(&v, "title").filter(|t| !t.is_empty());
    Some(Playing {
        progress: Some(progress),
        link: tveee_core::http::text(&v, "url").filter(|u| !u.is_empty()),
        // the media session's title is the programme's; the page's is the app's
        extra: title.map(|t| vec![("page", t)]).unwrap_or_default(),
        media: Some("video"),
        ..Playing::default()
    })
}

impl tveee_core::provider::Provider for NetTv {
    fn id(&self) -> &'static str {
        "nettv"
    }
    fn name(&self) -> &'static str {
        "NetTV apps"
    }
    fn hosts(&self) -> &'static [&'static str] {
        &[NETTV_BROWSER]
    }
    fn player(&self) -> Option<&dyn tveee_core::provider::Player> {
        Some(self)
    }
    // in front and debuggable: the page it's showing goes to the link
    fn open_here(&self, tv: &Tv, link: &str) -> Outcome<()> {
        if !web_link(link) || tveee_core::builtin::info::focus_package(&tv.window_dump().unwrap_or_default()).as_deref() != Some(NETTV_BROWSER) {
            return Outcome::Fallback;
        }
        let go = format!("(function(){{location.href={};return 'sent'}})()", js_string(link));
        match with_browser(tv, |c| c.eval(&go)) {
            Ok(v) if v == "sent" => Outcome::Handled(()),
            _ => Outcome::Fallback,
        }
    }
}

impl tveee_core::provider::Player for NetTv {
    // no video (a menu, the portal): what Android's media session says
    fn playing(&self, tv: &Tv) -> Outcome<Playing> {
        match with_browser(tv, |c| c.eval(PLAYING_EXPR)) {
            Ok(raw) => parse_playing(&raw).map_or(Outcome::Fallback, Outcome::Handled),
            Err(e) => Outcome::from(Err::<Playing, _>(e)),
        }
    }
    fn seek(&self, tv: &Tv, to: devtools::SeekTo) -> Outcome<f64> {
        with_browser(tv, |c| c.seek(to)).into()
    }
}

// --- the commands --------------------------------------------------------------

/// NetTV's commands: wrappers listed, installed and uninstalled; links
/// opened; pages inspected and JavaScript run; the log.
pub fn actions() -> Vec<ActionDescriptor> {
    use ActionDescriptor as A;
    let cat = super::CAT;
    vec![
        A::new("philips.nettv_apps", cat, "philips: list NetTV apps", |cx, a| apps_cmd(cx.tv, a).map(Into::into))
            .cli("nettv-apps", "[words]", "list the NetTV apps (web app wrappers) on the TV")
            .long(
                "Lists the NetTV wrapper packages (org.droidtv.nettvapp...): each opens one\n\
                 web app (iPlayer, Arte, ...) in the NetTV browser. With its name if tveee\n\
                 knows it, its version, and whether it came with the TV or was installed\n\
                 (only those can be uninstalled). Words narrow it down.",
            )
            .words(&["web", "wrapper", "homebrew"])
            .args("[words]")
            .text("NetTV apps (words, or nothing for all):", "iplayer"),
        A::new("philips.nettv_open", cat, "philips: open URL in NetTV", |cx, a| open_cmd(cx.tv, a).map(Into::into))
            .cli("nettv-open", "<url>", "open a web page in the NetTV browser")
            .long(
                "Opens a link in the NetTV browser (org.droidtv.nettvbrowser), the way a\n\
                 NetTV app's wrapper does: for trying a web app of your own on the TV.\n\
                 \n\
                 \x20 tveee nettv-open https://example.com/tv-app/\n\
                 \n\
                 Then 'tveee nettv-inspect' and 'tveee nettv-eval' look into it (if the TV\n\
                 lets its WebView be debugged) and 'tveee nettv-log' reads its log.",
            )
            .words(&["url", "link", "browser", "web"])
            .args("<url>")
            .text("URL to open in NetTV:", "https://..."),
        A::new("philips.nettv_inspect", cat, "philips: inspect NetTV WebView", |cx, _| inspect_cmd(cx.tv).map(Into::into))
            .cli("nettv-inspect", "", "list the NetTV browser's pages (title, address, loaded)")
            .long(
                "Lists the pages open in the NetTV browser, through its WebView's devtools\n\
                 socket (webview_devtools_remote_<pid>): title, address and readyState.\n\
                 Only when the TV's firmware lets the WebView be debugged; if it doesn't,\n\
                 it says so. The same socket works in Chrome's chrome://inspect after\n\
                 'adb forward tcp:9222 localabstract:<socket>'.",
            )
            .words(&["devtools", "debug", "webview", "pages"]),
        A::new("philips.nettv_eval", cat, "philips: run JS in NetTV", |cx, a| eval_cmd(cx.tv, a).map(Into::into))
            .cli("nettv-eval", "<javascript>", "run JavaScript in the NetTV browser's page and print the result")
            .long(
                "Runs JavaScript in the page the NetTV browser is showing (its global\n\
                 scope) and prints the result: strings as they are, anything else as\n\
                 JSON; an exception is the error. Quote it for the shell:\n\
                 \n\
                 \x20 tveee nettv-eval location.href\n\
                 \x20 tveee nettv-eval 'navigator.userAgent'\n\
                 \x20 tveee nettv-eval 'document.querySelectorAll(\"video\").length'\n\
                 \n\
                 Promises aren't waited for. Only when the TV lets its WebView be\n\
                 debugged (see 'tveee nettv-inspect').",
            )
            .words(&["javascript", "devtools", "debug", "console"])
            .args("<javascript>")
            .text("JavaScript to evaluate in NetTV:", "location.href"),
        A::new("philips.nettv_log", cat, "philips: read NetTV logcat", |cx, a| log_cmd(cx.tv, a).map(Into::into))
            .cli("nettv-log", "[words]", "the NetTV browser's recent log lines")
            .long(
                "The last 200 lines of the TV's log (logcat) from the NetTV browser's\n\
                 processes or naming it, a NetTV app, Chromium or WebView (the page's\n\
                 console messages among them). Words keep only the lines with all of them.",
            )
            .words(&["logcat", "console", "debug"])
            .args("[words]")
            .text("NetTV log (words, or nothing for all):", "console"),
        A::new("philips.nettv_install", cat, "philips: install NetTV wrapper APK", |cx, a| install_cmd(cx.tv, a).map(Into::into))
            .cli("nettv-install", "<file.apk>", "install a NetTV app's wrapper APK from this computer")
            .long(
                "Installs an APK from this computer over adb ('adb install -r': installing\n\
                 it again updates it) and says which package it was. For NetTV wrappers of\n\
                 your own; an APK runs with what Android grants it: install ones you trust.",
            )
            .words(&["apk", "sideload", "wrapper", "homebrew"])
            .args("<file.apk>")
            .text("Path to wrapper APK:", "app.apk"),
        A::new("philips.nettv_uninstall", cat, "philips: uninstall NetTV app", |cx, a| uninstall_cmd(cx.tv, a).map(Into::into))
            .cli("nettv-uninstall", "<package>", "uninstall a NetTV app someone installed")
            .long(
                "Uninstalls a NetTV wrapper (org.droidtv.nettvapp...) that was installed,\n\
                 never one that came with the TV. 'tveee nettv-apps' says which are which.",
            )
            .words(&["remove", "wrapper"])
            .args("<package>")
            .text("NetTV package to uninstall:", "org.droidtv.nettvapp...")
            .confirm("Uninstall this NetTV app from the TV?"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const PM: &str = "package:/system/app/NetTvApp4340/NetTvApp4340.apk=org.droidtv.nettvapp4340 versionCode:31
package:/data/app/~~a1==/org.droidtv.nettvapp9001-b2==/base.apk=org.droidtv.nettvapp9001 versionCode:1
package:/system/app/NetTvApp1814/NetTvApp1814.apk=org.droidtv.nettvapp1814
";

    #[test]
    fn reads_package_lists() {
        let w = parse_packages(PM);
        assert_eq!(w.len(), 3);
        assert_eq!(w[1].package, "org.droidtv.nettvapp9001");
        assert_eq!(w[1].path, "/data/app/~~a1==/org.droidtv.nettvapp9001-b2==/base.apk");
        assert_eq!((w[0].version.as_deref(), w[2].version.as_deref()), (Some("31"), None));
        assert!(w[1].user_installed() && !w[0].user_installed());
        assert_eq!(w[0].name(), Some("iPlayer"));
        assert!(parse_packages("Error: something\n").is_empty());
    }

    #[test]
    fn lists_and_matches() {
        let w = parse_packages(PM);
        assert_eq!(matching(&w, "IPLAYER").len(), 1);
        assert_eq!(matching(&w, "data").len(), 1);
        assert_eq!(matching(&w, "").len(), 3);
        let text = lines(&matching(&w, "9001"));
        assert_eq!(text, "?  org.droidtv.nettvapp9001 v1  (installed)");
        assert!(lines(&matching(&w, "4340")).contains("iPlayer  org.droidtv.nettvapp4340 v31  (built in)"));
    }

    #[test]
    fn builds_launches() {
        assert_eq!(
            start_line("org.droidtv.nettvbrowser/.Main", "https://x.example/a?b=1&c='d'"),
            "am start -n 'org.droidtv.nettvbrowser/.Main' -a android.intent.action.VIEW -d 'https://x.example/a?b=1&c='\\''d'\\'''"
        );
        assert!(web_link("HTTPS://x") && web_link("http://x"));
        assert!(!web_link("https://") && !web_link("file:///sdcard/x.html") && !web_link("javascript:alert(1)"));
        let dump = "  Intent { act=android.intent.action.VIEW dat=https://x/ flg=0x10000000 cmp=org.droidtv.nettvbrowser/.NetTvBrowserActivity }";
        assert_eq!(started_component(dump).as_deref(), Some("org.droidtv.nettvbrowser/.NetTvBrowserActivity"));
        assert_eq!(started_component("cmp=org.droidtv.nettvbrowser/.A\n").as_deref(), Some("org.droidtv.nettvbrowser/.A"));
        assert_eq!(started_component("nothing"), None);
    }

    #[test]
    fn guards_uninstalls() {
        let installed = parse_packages(PM);
        assert_eq!(uninstall_refusal("org.droidtv.nettvapp9001", &installed), None);
        assert!(uninstall_refusal("org.droidtv.nettvapp4340", &installed).unwrap().contains("part of the TV"));
        assert!(uninstall_refusal("org.droidtv.nettvapp5555", &installed).unwrap().contains("isn't installed"));
        assert!(uninstall_refusal("org.droidtv.nettvapp1;reboot", &installed).unwrap().contains("isn't a package"));
        assert!(uninstall_refusal("com.example", &installed).unwrap().contains("only NetTV apps"));
    }

    #[test]
    fn finds_installs() {
        let before = parse_packages(PM);
        let mut after = before.clone();
        after[1].path = "/data/app/~~c3==/org.droidtv.nettvapp9001-d4==/base.apk".into();
        after.push(Wrapper { package: "org.example.tv".into(), path: "/data/app/x/base.apk".into(), version: None });
        assert_eq!(changed(&before, &after), ["org.droidtv.nettvapp9001", "org.example.tv"]);
        assert!(changed(&before, &before).is_empty());
    }

    #[test]
    fn filters_the_log() {
        let log = "11-28 10:00:00.000 I/chromium( 4242): [INFO:CONSOLE(1)] \"hello\", source: https://x/ (1)
11-28 10:00:00.100 D/Surface( 4242): something of the browser's
11-28 10:00:00.200 D/Wifi(  777): unrelated
11-28 10:00:00.300 I/ActivityManager(  500): Start proc 4242:org.droidtv.nettvbrowser/u0a12
11-28 10:00:00.400 W/cr_WebView( 900): another app's webview";
        let all = nettv_lines(log, &["4242"], "", 200);
        assert_eq!(all.len(), 4);
        assert!(!all.iter().any(|l| l.contains("Wifi")));
        assert_eq!(nettv_lines(log, &["4242"], "CONSOLE hello", 200).len(), 1);
        assert_eq!(nettv_lines(log, &[], "", 200).len(), 3);
        assert_eq!(nettv_lines(log, &["4242"], "", 2), all[2..]);
        assert_eq!(log_pid("11-28 10:00:00.000 I/chromium( 4242): x"), Some("4242"));
        assert_eq!(log_pid("--------- beginning of main"), None);
    }

    #[test]
    fn javascript_is_quoted() {
        assert_eq!(js_string("a'b\"c\\\n\u{2028}"), "\"a'b\\\"c\\\\\\n\\u2028\"");
        let e = eval_expr("1+1\"); alert(\"x");
        assert!(e.contains(r#"(0,eval)("1+1\"); alert(\"x")"#), "{e}");
        assert_eq!(eval_answer(r#"{"ok":true,"v":"https://x/"}"#).unwrap(), "https://x/");
        assert!(eval_answer(r#"{"ok":false,"e":"ReferenceError: y is not defined"}"#).unwrap_err().to_string().contains("ReferenceError"));
        assert!(eval_answer("garbage").is_err());
    }

    #[test]
    fn reads_pages_and_playing() {
        let line = page_line("https://a/", Some(r#"{"title":"BBC iPlayer","url":"https://b/","ready":"complete"}"#));
        assert_eq!(line, "BBC iPlayer  [complete]\n  https://b/");
        assert_eq!(page_line("https://a/", None), "https://a/  (didn't answer)");
        let p = parse_playing(r#"{"title":"BBC iPlayer","url":"https://b/","video":[12.5,3600,false]}"#).unwrap();
        assert_eq!(p.progress.map(|x| (x.position, x.duration, x.paused)), Some((12.5, Some(3600.0), false)));
        assert_eq!((p.link.as_deref(), p.extra), (Some("https://b/"), vec![("page", "BBC iPlayer".to_string())]));
        assert_eq!(parse_playing(r#"{"title":"Portal","url":"https://b/","video":null}"#), None);
        // live: no duration
        assert_eq!(parse_playing(r#"{"title":"","url":"https://b/","video":[5,null,true]}"#).unwrap().progress.unwrap().duration, None);
    }
}

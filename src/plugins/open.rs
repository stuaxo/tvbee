//! `open [app] <url>`: hand a link to the app that handles it (Android VIEW intent).

use super::apps;
use crate::error::{failed, usage, Result};
use crate::registry::Command;
use crate::tv::Tv;
use crate::Ctx;

const VIEW: &str = "android.intent.action.VIEW";

/// Players to use, in order, when a media file would otherwise bring up a chooser.
const PLAYERS: &[&str] = &["org.videolan.vlc", "com.mxtech.videoplayer.pro", "org.xbmc.kodi"];

/// Any web page resolves here when no app claims the link (MX Player registers
/// as a browser on this TV), so a link that resolves to the same place is
/// "nobody's".
const PLAIN_WEB: &str = "https://example.invalid/";

const CHOOSER: &str = "android/com.android.internal.app.ResolverActivity";

/// MIME type from the file extension, for links straight to media files.
pub fn media_type(url: &str) -> Option<&'static str> {
    let path = url.split(['?', '#']).next()?;
    let ext = path.rsplit_once('.')?.1.to_lowercase();
    if ext.contains('/') {
        return None;
    }
    Some(match ext.as_str() {
        "mp4" | "m4v" => "video/mp4",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        "avi" => "video/x-msvideo",
        "mov" => "video/quicktime",
        "ts" => "video/mp2t",
        "m3u8" => "application/vnd.apple.mpegurl",
        "mpd" => "application/dash+xml",
        "mp3" => "audio/mpeg",
        "m4a" | "aac" => "audio/mp4",
        "flac" => "audio/flac",
        "ogg" | "opus" => "audio/ogg",
        "wav" => "audio/wav",
        _ => return None,
    })
}

/// `youtube.com/watch?v=x` -> `https://youtube.com/watch?v=x`; links with a scheme are left alone.
pub fn normalise(url: &str) -> String {
    if url.contains(':') { url.to_string() } else { format!("https://{url}") }
}

/// Single-quote for the device shell (adb joins arguments into one command line).
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The intent arguments shared by lookups and `am start`.
fn intent_args(url: &str, mime: Option<&str>, pkg: Option<&str>) -> String {
    let mut a = format!("-a {VIEW} -d {}", sh_quote(url));
    if let Some(t) = mime {
        a.push_str(&format!(" -t {t}"));
    }
    if let Some(p) = pkg {
        a.push_str(&format!(" -p {}", sh_quote(p)));
    }
    a
}

/// `pkg/activity` from `resolve-activity --brief` output, None for "No activity found".
pub fn parse_resolved(out: &str) -> Option<String> {
    let last = out.lines().map(str::trim).rfind(|l| !l.is_empty())?;
    (last.contains('/') && !last.contains(' ')).then(|| last.to_string())
}

/// Components listed by `query-activities --brief`.
pub fn parse_candidates(out: &str) -> Vec<String> {
    out.lines()
        .map(str::trim)
        .filter(|l| l.contains('/') && !l.contains(' ') && !l.contains('='))
        .map(str::to_string)
        .collect()
}

fn resolve(tv: &Tv, url: &str, mime: Option<&str>, pkg: Option<&str>) -> Result<Option<String>> {
    let cmd = format!("cmd package resolve-activity --brief {}", intent_args(url, mime, pkg));
    Ok(parse_resolved(&tv.shell(&[&cmd])?))
}

fn candidates(tv: &Tv, url: &str, mime: Option<&str>) -> Result<Vec<String>> {
    let cmd = format!("cmd package query-activities --brief {}", intent_args(url, mime, None));
    Ok(parse_candidates(&tv.shell(&[&cmd])?))
}

/// The app's name ("YouTube"), or its package if tveee doesn't know it.
fn label(component: &str) -> String {
    let pkg = component.split('/').next().unwrap_or(component);
    apps::by_package(pkg).map_or_else(|| pkg.to_string(), |a| a.name.to_string())
}

/// Pick the component to open `url` with, or explain why not.
fn choose(tv: &Tv, url: &str, mime: Option<&str>, app: Option<&str>) -> Result<String> {
    if let Some(app) = app {
        let pkg = apps::pkg_of(app);
        return match resolve(tv, url, mime, Some(pkg))? {
            Some(c) => Ok(c),
            None => failed(format!("open: {app} ({pkg}) doesn't open links like this")),
        };
    }
    let Some(found) = resolve(tv, url, mime, None)? else {
        return failed("open: no app on the TV opens this link");
    };
    if found == CHOOSER {
        // several apps could; Android would ask. Prefer a known player.
        let all = candidates(tv, url, mime)?;
        let pick = PLAYERS
            .iter()
            .find_map(|p| all.iter().find(|c| c.split('/').next() == Some(p)))
            .or(all.first());
        return match pick {
            Some(c) => Ok(c.clone()),
            None => failed("open: Android would ask which app to use; name one: tveee open <app> <url>"),
        };
    }
    if mime.is_none() && url.starts_with("http") && resolve(tv, PLAIN_WEB, None, None)?.as_deref() == Some(&found) {
        return failed(format!(
            "open: no app claims this link; only the catch-all web handler ({}) would.\nName an app to force it: tveee open <app> <url>",
            label(&found)
        ));
    }
    Ok(found)
}

/// Open `url` (optionally "app url") and return what happened, without printing.
pub fn open_link(tv: &Tv, line: &str) -> Result<String> {
    let words: Vec<String> = line.split_whitespace().map(str::to_string).collect();
    let (app, url, _) = parse_args(&words)?;
    let url = normalise(url);
    let mime = media_type(&url);
    let component = choose(tv, &url, mime, app)?;
    if super::apps::link_here_or_prepare(tv, component.split('/').next().unwrap_or(&component), &url)? {
        return Ok(format!("opened in {}", label(&component)));
    }
    let cmd = format!("am start -n {} {}", sh_quote(&component), intent_args(&url, mime, None));
    let out = tv.shell(&[&cmd])?;
    if let Some(err) = out.lines().find(|l| l.starts_with("Error")) {
        return failed(format!("open: {err}"));
    }
    Ok(format!("opened in {}", label(&component)))
}

pub fn actions() -> Vec<crate::actions::ActionDescriptor> {
    vec![crate::actions::ActionDescriptor::new("apps.open_link", "apps", "open a link", |cx, a| open_link(cx.tv, &a.join(" ")).map(Into::into))
        .key('o')
        .text("Link to open:", "e.g. https://youtu.be/...   or   vlc http://host/film.mkv")]
}

pub fn parse_args(args: &[String]) -> Result<(Option<&str>, &str, bool)> {
    let mut dry = false;
    let mut pos = Vec::new();
    for a in args {
        match a.as_str() {
            "--dry-run" => dry = true,
            f if f.starts_with('-') => return usage(format!("open: unknown option '{f}'")),
            p => pos.push(p),
        }
    }
    match pos.as_slice() {
        [url] if apps::is_alias(url) => usage(format!("open: missing url (usage: tveee open {USAGE})")),
        [url] => Ok((None, url, dry)),
        [app, url] => Ok((Some(app), url, dry)),
        _ => usage(format!("usage: tveee open {USAGE}")),
    }
}

const USAGE: &str = "[--dry-run] [app] <url>";

fn open(c: &Ctx, args: &[String]) -> Result<()> {
    let (app, url, dry) = parse_args(args)?;
    if !dry {
        let line = app.map_or_else(|| url.to_string(), |a| format!("{a} {url}"));
        println!("{}", open_link(&c.tv, &line)?);
        return Ok(());
    }
    let url = normalise(url);
    let mime = media_type(&url);
    let component = choose(&c.tv, &url, mime, app)?;
    println!("would open in {} ({component})", label(&component));
    Ok(())
}

pub fn commands() -> Vec<Command> {
    vec![Command::new("apps", "open", USAGE, "open a link in the app that handles it", open).long(
        "Sends the TV a 'view this link' request. Without an app, Android picks the app\n\
         that claims the link (YouTube and Netflix links open in their apps). Links\n\
         straight to media files (.mp4, .mkv, .m3u8, ...) go to VLC, then MX Player,\n\
         then Kodi. Links no app claims are refused rather than opened in the\n\
         catch-all web handler.\n\
         \n\
         <app> is an alias (see 'tveee apps') or a package, e.g.\n\
         \x20 tveee open https://youtu.be/gwLRWgOWhBs\n\
         \x20 tveee open vlc http://example.com/film.mkv\n\
         \x20 tveee open \"$(tveee status | sed -n 's/^url: *//p')\"\n\
         \x20 --dry-run  say which app would open it, without opening",
    )]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_types() {
        assert_eq!(media_type("http://x/film.MKV?token=1"), Some("video/x-matroska"));
        assert_eq!(media_type("http://x/live.m3u8"), Some("application/vnd.apple.mpegurl"));
        assert_eq!(media_type("https://www.youtube.com/watch?v=a.b"), None);
        assert_eq!(media_type("https://example.com/"), None);
        assert_eq!(media_type("https://example.com"), None);
    }

    #[test]
    fn normalises_and_quotes() {
        assert_eq!(normalise("youtu.be/x"), "https://youtu.be/x");
        assert_eq!(normalise("vnd.youtube:x"), "vnd.youtube:x");
        assert_eq!(sh_quote("a&b='c'"), "'a&b='\\''c'\\'''");
    }

    #[test]
    fn parses_lookups() {
        assert_eq!(
            parse_resolved("priority=0 preferredOrder=0\norg.videolan.vlc/.StartActivity\n").as_deref(),
            Some("org.videolan.vlc/.StartActivity")
        );
        assert_eq!(parse_resolved("No activity found\n"), None);
        let q = "5 activities found:\n  Activity #0:\n    priority=0 preferredOrder=0 match=0x608000\n    org.droidtv.contentexplorer/.player.ExternalMediaPlaybackActivity\n  Activity #1:\n    org.videolan.vlc/.StartActivity\n";
        assert_eq!(
            parse_candidates(q),
            vec!["org.droidtv.contentexplorer/.player.ExternalMediaPlaybackActivity", "org.videolan.vlc/.StartActivity"]
        );
    }

    #[test]
    fn args() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let x = a(&["https://youtu.be/x"]);
        assert_eq!(parse_args(&x).unwrap(), (None, "https://youtu.be/x", false));
        let x = a(&["vlc", "http://x/a.mp4", "--dry-run"]);
        assert_eq!(parse_args(&x).unwrap(), (Some("vlc"), "http://x/a.mp4", true));
        assert!(parse_args(&a(&["vlc"])).is_err());
        assert!(parse_args(&a(&[])).is_err());
        assert!(parse_args(&a(&["-x", "http://y"])).is_err());
    }
}

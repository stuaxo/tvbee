//! YouTube for Android TV: its app entry (links for what's playing, search),
//! and commands that look inside the app.
//!
//! The YouTube TV app is a web app run by Google's Cobalt engine, which opens
//! a devtools socket. tveee reads the page through it ([`crate::devtools`]):
//! the route (`tv#/watch?v=...`), the focused element, the video. Enabled when
//! the app is installed.

use crate::devtools;
use crate::more::{Chapter, Details, Go, Link, Place};
use crate::error::{failed, Result};
use crate::provider::{Outcome, Playing};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use crate::registry::Command;
use crate::tv::Tv;

pub const PACKAGE: &str = "com.google.android.youtube.tv";

pub fn detect(tv: &Tv) -> Result<bool> {
    let out = tv.shell(&["cmd", "package", "list", "packages", PACKAGE])?;
    Ok(out.lines().any(|l| l.trim() == format!("package:{PACKAGE}")))
}

/// `https://www.youtube.com/tv#/watch?v=ID&...` -> `https://www.youtube.com/watch?v=ID`
pub fn watch_link(page: &str) -> Option<String> {
    let rest = &page[page.find("watch?v=")? + "watch?v=".len()..];
    let id: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
    (id.len() == 11).then(|| format!("https://www.youtube.com/watch?v={id}"))
}

/// Places in the app; each is a youtube.com link the app opens at the
/// matching screen (checked on a TV).
pub const PLACES: &[Place] = &[
    Place { id: "home", label: "Home", link: "https://www.youtube.com/" },
    Place { id: "subscriptions", label: "Subscriptions", link: "https://www.youtube.com/feed/subscriptions" },
    Place { id: "history", label: "History", link: "https://www.youtube.com/feed/history" },
    Place { id: "library", label: "Library (watch later, playlists)", link: "https://www.youtube.com/feed/library" },
    Place { id: "watchlater", label: "Watch later", link: "https://www.youtube.com/playlist?list=WL" },
];

pub fn apps() -> Vec<crate::plugins::apps::App> {
    use crate::plugins::apps::{app, Kind};
    vec![app("youtube", "YouTube", PACKAGE, Kind::Video)]
}

/// The YouTube provider: what's playing (via the app's devtools page), its
/// places, details via yt-dlp, and search by results link.
pub struct YouTube;
pub static YOUTUBE: YouTube = YouTube;

impl crate::provider::Provider for YouTube {
    fn id(&self) -> &'static str {
        "youtube"
    }
    fn name(&self) -> &'static str {
        "YouTube"
    }
    fn hosts(&self) -> &'static [&'static str] {
        &[PACKAGE]
    }
    fn player(&self) -> Option<&dyn crate::provider::Player> {
        Some(self)
    }
    // the TV app takes links, not the search intent
    fn search_link(&self) -> Option<&'static str> {
        Some("https://www.youtube.com/results?search_query={q}")
    }
    // each link into the running app stacks another drawing surface; a
    // stale one can cover the video (you hear it but don't see it)
    fn fresh_for_links(&self) -> bool {
        true
    }
    // already in front: hand the page the link as Android would, through
    // devtools, rather than closing the app for it (the screen would drop
    // to home, then reopen). The app ignores changes to location.hash.
    fn open_here(&self, tv: &Tv, link: &str) -> Outcome<()> {
        // anything amiss: the usual way
        if crate::plugins::info::focus_package(&tv.window_dump().unwrap_or_default()).as_deref() != Some(PACKAGE) {
            return Outcome::Fallback;
        }
        match devtools::with_conn(tv, PACKAGE, |c| c.eval(&deep_link_expr(link))) {
            Ok(v) if v == "sent" => Outcome::Handled(()),
            _ => Outcome::Fallback,
        }
    }
    fn places(&self) -> &'static [Place] {
        PLACES
    }
    // the video and how far in: restarting comes back to it
    fn save_session(&self, tv: &Tv) -> Outcome<crate::provider::Session> {
        session(tv).map_or(Outcome::Fallback, Outcome::Handled)
    }
    fn has_sessions(&self) -> bool {
        true
    }
    fn details(&self) -> Option<&dyn crate::provider::Details> {
        Some(self)
    }
}

impl crate::provider::Details for YouTube {
    fn details(&self, link: &str) -> Result<Details> {
        details(link)
    }
}

/// JavaScript that hands the page `link` the way the app's Android side does:
/// a `deeplink` event on Cobalt's `h5vcc.runtime`. "sent" if it could.
fn deep_link_expr(link: &str) -> String {
    // a JavaScript string: escape what could end it
    let mut quoted = String::from("'");
    for ch in link.chars() {
        match ch {
            '\\' | '\'' => quoted.extend(['\\', ch]),
            c if (c as u32) < 0x20 || c == '\u{2028}' || c == '\u{2029}' => quoted.push_str(&format!("\\u{:04x}", c as u32)),
            c => quoted.push(c),
        }
    }
    quoted.push('\'');
    format!(
        "(function(){{var r=window.h5vcc&&h5vcc.runtime;if(!r||!r.dispatchEvent)return 'no';\
         var e=new Event('deeplink');Object.defineProperty(e,'url',{{value:{quoted}}});\
         r.dispatchEvent(e);return 'sent'}})()"
    )
}

// --- the player: what's playing, seeking, and a picture of it ----------------

impl crate::provider::Player for YouTube {
    fn playing(&self, tv: &Tv) -> Outcome<Playing> {
        let pages = match devtools::with_conn(tv, PACKAGE, |c| c.pages()) {
            Ok(p) => p,
            Err(e) => return Outcome::from(Err::<Playing, _>(e)),
        };
        let link = pages.iter().find_map(|p| watch_link(&p.url));
        let progress = pages.iter().find_map(|p| p.progress);
        if link.is_none() && progress.is_none() {
            return Outcome::Fallback;
        }
        let art = link.as_deref().and_then(video_id).map(|id| format!("youtube:{id}"));
        // the title is the media session's (it has the channel too)
        Outcome::Handled(Playing { progress, art, art_moves: true, link, ..Playing::default() })
    }
    fn seek(&self, tv: &Tv, to: devtools::SeekTo) -> Outcome<f64> {
        devtools::with_conn(tv, PACKAGE, |c| c.seek(to)).into()
    }
    fn stop(&self) {
        stop_fetching();
    }
    // a frame of the video at `at`: from a small copy of it, or its
    // storyboard until that's here
    fn picture(&self, _tv: &Tv, art: &str, at: Option<f64>) -> Outcome<Vec<u8>> {
        let Some(id) = art.strip_prefix("youtube:") else { return Outcome::Fallback };
        let at = at.unwrap_or(0.0);
        if let Some(copy) = copy_of(id) {
            if let Some(jpeg) = frame_of(&copy, at) {
                return Outcome::Handled(jpeg);
            }
        }
        storyboard_frame(id, at).map_or(Outcome::Fallback, Outcome::Handled)
    }
}

fn program(name: &str) -> Option<PathBuf> {
    crate::tv::search_path(name, std::env::var_os("PATH").as_deref())
}

/// Where small copies of videos and storyboard sheets are kept.
fn previews_dir() -> Option<PathBuf> {
    Some(crate::plugin::cache_path("x")?.parent()?.join("previews"))
}

/// How many copies of videos to keep.
const KEEP: usize = 4;

/// The smallest video-only download yt-dlp can find (a direct one, not a
/// stream of pieces): 144p, a couple of MB for a few minutes.
const SMALLEST: &[&str] = &["-f", "bv[protocol^=http]/bv/wv", "-S", "+size,+br,+res"];

/// Copies being fetched (None: tried, failed) or here.
fn copies() -> &'static Mutex<HashMap<String, Option<PathBuf>>> {
    static COPIES: OnceLock<Mutex<HashMap<String, Option<PathBuf>>>> = OnceLock::new();
    COPIES.get_or_init(Default::default)
}

/// The download running now: (video, yt-dlp), so it can be stopped.
static FETCHING: Mutex<Option<(String, std::process::Child)>> = Mutex::new(None);

/// Stop the download running now (if any), and forget it was asked for, so
/// it starts again if that video is wanted later.
pub fn stop_fetching() {
    let stopped = FETCHING.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
    if let Some((id, mut child)) = stopped {
        let _ = child.kill();
        let _ = child.wait();
        crate::tlog!("youtube: stopped fetching {id}");
        let mut all = copies().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if all.get(&id).is_some_and(Option::is_none) {
            all.remove(&id);
        }
    }
}

/// The small copy of video `id`, if it's here; the first ask starts
/// fetching it (in the background), stopping any other video's download.
fn copy_of(id: &str) -> Option<PathBuf> {
    let fetching_other = FETCHING.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref().is_some_and(|(f, _)| f != id);
    if fetching_other {
        stop_fetching();
    }
    let mut all = copies().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    match all.get(id) {
        Some(Some(p)) if p.exists() => return Some(p.clone()),
        Some(_) => return None,
        None => {}
    }
    // fetching: None until done (or for good, if it fails)
    all.insert(id.to_string(), None);
    let id = id.to_string();
    std::thread::spawn(move || {
        if let Ok(p) = local_copy(&id) {
            copies().lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(id, Some(p));
        }
    });
    None
}

/// A copy of video `id`, at the lowest quality, made with yt-dlp (or the one
/// made before). Slow-ish the first time.
pub fn local_copy(id: &str) -> Result<PathBuf> {
    let Some(dir) = previews_dir() else { return failed("preview: no cache folder") };
    std::fs::create_dir_all(&dir)?;
    let mine = |p: &PathBuf| {
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        name.starts_with(&format!("{id}.")) && !name.ends_with(".part") && !name.ends_with(".ytdl")
    };
    let copies_here = || -> Vec<PathBuf> {
        std::fs::read_dir(&dir).map(|d| d.flatten().map(|e| e.path()).filter(|p| !p.to_string_lossy().contains("-sb")).collect()).unwrap_or_default()
    };
    if let Some(p) = copies_here().into_iter().find(mine) {
        return Ok(p);
    }
    // room for this one: the oldest go
    let mut old: Vec<(std::time::SystemTime, PathBuf)> =
        copies_here().into_iter().filter_map(|p| Some((std::fs::metadata(&p).ok()?.modified().ok()?, p))).collect();
    old.sort();
    for (_, p) in old.iter().rev().skip(KEEP - 1) {
        let _ = std::fs::remove_file(p);
    }
    let Some(ytdlp) = program("yt-dlp") else { return failed("preview: yt-dlp isn't installed here") };
    let child = std::process::Command::new(ytdlp)
        .args(["-q", "--no-playlist", "--no-progress"])
        .args(SMALLEST)
        .arg("-o")
        .arg(dir.join(format!("{id}.%(ext)s")))
        .arg(format!("https://www.youtube.com/watch?v={id}"))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    crate::tlog!("youtube: fetching a small copy of {id}");
    *FETCHING.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some((id.to_string(), child));
    // wait for it, letting go of it between looks so it can be stopped
    let done = loop {
        std::thread::sleep(std::time::Duration::from_millis(250));
        let mut f = FETCHING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        match f.as_mut() {
            Some((fid, child)) if fid == id => match child.try_wait() {
                Ok(Some(status)) => {
                    *f = None;
                    break status.success();
                }
                Ok(None) => {}
                Err(_) => {
                    *f = None;
                    break false;
                }
            },
            // stopped (or another one started): not this one, then
            _ => return failed("preview: the download was stopped"),
        }
    };
    match copies_here().into_iter().find(mine) {
        Some(p) => Ok(p),
        None => failed(format!("preview: yt-dlp couldn't fetch the video{}", if done { "" } else { " (it failed)" })),
    }
}

/// `ffmpeg` over `input` with filter `vf`, one frame out as JPEG.
fn ffmpeg_jpeg(seek: Option<f64>, input: &std::path::Path, vf: &str) -> Option<Vec<u8>> {
    let mut cmd = std::process::Command::new(program("ffmpeg")?);
    cmd.args(["-loglevel", "error"]);
    if let Some(s) = seek {
        cmd.args(["-ss", &format!("{:.2}", s.max(0.0))]);
    }
    let out = cmd
        .arg("-i")
        .arg(input)
        .args(["-frames:v", "1", "-vf", vf, "-f", "image2pipe", "-vcodec", "mjpeg", "pipe:1"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    (!out.stdout.is_empty()).then_some(out.stdout)
}

/// The frame at `secs` into a video file, as JPEG.
pub fn frame_of(file: &std::path::Path, secs: f64) -> Option<Vec<u8>> {
    ffmpeg_jpeg(Some(secs), file, "scale=320:-2")
}

/// A video's storyboard: sheets of `cols` x `rows` frames, each `width` x
/// `height`, `fps` frames a second of video.
#[derive(Debug, Clone, PartialEq)]
pub struct Storyboard {
    pub cols: u32,
    pub rows: u32,
    pub fps: f64,
    pub width: u32,
    pub height: u32,
    pub sheets: Vec<String>,
}

impl Storyboard {
    /// Which sheet, and where on it (x, y), the frame at `secs` is.
    pub fn frame_at(&self, secs: f64) -> Option<(usize, u32, u32)> {
        let per_sheet = (self.cols * self.rows) as usize;
        if per_sheet == 0 || self.sheets.is_empty() {
            return None;
        }
        let i = (secs.max(0.0) * self.fps).floor() as usize;
        // past the end: the last frame there is
        let i = i.min(self.sheets.len() * per_sheet - 1);
        let (sheet, cell) = (i / per_sheet, (i % per_sheet) as u32);
        Some((sheet, (cell % self.cols) * self.width, (cell / self.cols) * self.height))
    }
}

/// The strings in a JSON list of strings (`["a", "b\\"c"]`).
pub fn json_strings(list: &str) -> Vec<String> {
    serde_json::from_str(list).unwrap_or_default()
}

/// The storyboard from yt-dlp's two lines (see [`storyboard`]).
pub fn parse_storyboard(out: &str) -> Option<Storyboard> {
    let mut lines = out.lines().filter(|l| !l.trim().is_empty());
    let nums: Vec<&str> = lines.next()?.split_whitespace().collect();
    let [cols, rows, fps, width, height] = nums.as_slice() else { return None };
    let sheets = json_strings(lines.next()?);
    Some(Storyboard {
        cols: cols.parse().ok()?,
        rows: rows.parse().ok()?,
        fps: fps.parse().ok()?,
        width: width.parse().ok()?,
        height: height.parse().ok()?,
        sheets,
    })
    .filter(|s| !s.sheets.is_empty() && s.fps > 0.0)
}

/// Video `id`'s storyboard, from yt-dlp (asked once a run per video).
pub fn storyboard(id: &str) -> Option<Storyboard> {
    static KNOWN: OnceLock<Mutex<HashMap<String, Option<Storyboard>>>> = OnceLock::new();
    let known = KNOWN.get_or_init(Default::default);
    if let Some(s) = known.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(id) {
        return s.clone();
    }
    let out = std::process::Command::new(program("yt-dlp")?)
        .args(["--skip-download", "-f", "sb0", "-O", "%(columns)s %(rows)s %(fps)s %(width)s %(height)s", "-O", "%(fragments.:.url)j"])
        .arg(format!("https://www.youtube.com/watch?v={id}"))
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let board = parse_storyboard(&String::from_utf8_lossy(&out.stdout));
    known.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(id.to_string(), board.clone());
    board
}

/// Of the storyboard sheets here (file name, last changed), those to delete:
/// all but those of the [`KEEP`] videos used last.
pub fn sheets_to_remove(sheets: &[(String, std::time::SystemTime)]) -> Vec<String> {
    let video = |name: &str| name.rfind("-sb").map(|i| name[..i].to_string());
    let mut last_used: HashMap<String, std::time::SystemTime> = HashMap::new();
    for (name, at) in sheets {
        if let Some(v) = video(name) {
            let e = last_used.entry(v).or_insert(*at);
            *e = (*e).max(*at);
        }
    }
    let mut videos: Vec<(std::time::SystemTime, String)> = last_used.into_iter().map(|(v, at)| (at, v)).collect();
    videos.sort_by(|a, b| b.cmp(a));
    let keep: Vec<String> = videos.into_iter().take(KEEP).map(|(_, v)| v).collect();
    sheets.iter().filter(|(n, _)| video(n).is_some_and(|v| !keep.contains(&v))).map(|(n, _)| n.clone()).collect()
}

/// Delete old storyboard sheets (see [`sheets_to_remove`]).
fn prune_sheets(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let sheets: Vec<(String, std::time::SystemTime)> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            (name.contains("-sb") && name.ends_with(".jpg")).then_some(())?;
            Some((name, e.metadata().ok()?.modified().ok()?))
        })
        .collect();
    for name in sheets_to_remove(&sheets) {
        let _ = std::fs::remove_file(dir.join(&name));
        crate::tlog!("youtube: removed old storyboard sheet {name}");
    }
}

/// The storyboard frame of video `id` at `secs`, as JPEG.
pub fn storyboard_frame(id: &str, secs: f64) -> Option<Vec<u8>> {
    let board = storyboard(id)?;
    let (n, x, y) = board.frame_at(secs)?;
    // the sheet, fetched once
    let dir = previews_dir()?;
    std::fs::create_dir_all(&dir).ok()?;
    let sheet = dir.join(format!("{id}-sb{n}.jpg"));
    if !sheet.exists() {
        let ok = std::process::Command::new(program("curl")?)
            .args(["-sSfL", "--max-time", "15", "-o"])
            .arg(&sheet)
            .arg(board.sheets.get(n)?)
            .status()
            .ok()?
            .success();
        if !ok {
            return None;
        }
        prune_sheets(&dir);
    }
    ffmpeg_jpeg(None, &sheet, &format!("crop={}:{}:{x}:{y}", board.width, board.height))
}

pub fn providers() -> Vec<&'static dyn crate::provider::Provider> {
    vec![&YOUTUBE]
}

/// The video id in a watch link.
pub fn video_id(link: &str) -> Option<String> {
    let rest = &link[link.find("v=")? + 2..];
    let id: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
    (id.len() == 11).then_some(id)
}

/// yt-dlp output templates: one JSON value per line, in this order.
const FIELDS: &[&str] = &[
    "title", "channel", "uploader_id", "channel_url", "upload_date", "view_count", "like_count", "duration", "chapters",
    "description",
];

/// Turn yt-dlp's lines (see [`FIELDS`]) into details.
pub fn parse_details(out: &str) -> Details {
    let lines: Vec<&str> = out.lines().collect();
    let get = |k: &str| FIELDS.iter().position(|f| *f == k).and_then(|i| lines.get(i)).copied().unwrap_or("NA");
    let text = |k: &str| serde_json::from_str::<String>(get(k)).ok().filter(|s| !s.is_empty());
    let num = |k: &str| get(k).trim().parse::<f64>().ok();
    let mut d = Details::default();
    let handle = text("uploader_id");
    if let Some(ch) = text("channel") {
        d.fields.push(("channel", handle.as_ref().map_or(ch.clone(), |h| format!("{ch} ({h})"))));
    }
    if let Some(date) = text("upload_date").filter(|s| s.len() == 8) {
        d.fields.push(("uploaded", format!("{}-{}-{}", &date[..4], &date[4..6], &date[6..])));
    }
    if let Some(v) = num("view_count") {
        d.fields.push(("views", crate::more::count(v)));
    }
    if let Some(v) = num("like_count") {
        d.fields.push(("likes", crate::more::count(v)));
    }
    if let Some(v) = num("duration") {
        d.fields.push(("length", crate::nowplaying::clock(v)));
    }
    let chapters = crate::http::json(get("chapters")).unwrap_or_default();
    d.chapters = chapters
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|o| Some(Chapter { start: crate::http::number(o, "start_time")?, title: crate::http::text(o, "title")? }))
        .collect();
    d.description = text("description");
    // where to go from here: the channel, the chapters, links in the description
    let channel = handle.map(|h| format!("https://www.youtube.com/{h}")).or_else(|| text("channel_url"));
    if let Some(url) = channel {
        d.links.push(Link { label: format!("channel: {}", text("channel").unwrap_or_default()), go: Go::Link(url) });
    }
    for (i, c) in d.chapters.iter().enumerate() {
        d.links.push(Link { label: format!("chapter {}: {} ({})", i + 1, c.title, crate::nowplaying::clock(c.start)), go: Go::Seek(c.start) });
    }
    d.links.extend(crate::more::description_links(d.description.as_deref().unwrap_or("")));
    d
}

fn cache_file(id: &str) -> Option<std::path::PathBuf> {
    let base = crate::plugin::cache_path("x")?;
    Some(base.parent()?.join("youtube").join(format!("{id}.txt")))
}

/// Details for a watch link, from yt-dlp (cached a day per video).
pub fn details(link: &str) -> Result<Details> {
    let Some(id) = video_id(link) else { return failed("youtube: that isn't a video link") };
    let cache = cache_file(&id);
    let fresh = |p: &std::path::Path| -> Option<String> {
        let age = std::fs::metadata(p).ok()?.modified().ok()?.elapsed().ok()?;
        (age < std::time::Duration::from_secs(24 * 3600)).then(|| std::fs::read_to_string(p).ok()).flatten()
    };
    if let Some(text) = cache.as_deref().and_then(fresh) {
        return Ok(parse_details(&text));
    }
    let Some(ytdlp) = crate::tv::search_path("yt-dlp", std::env::var_os("PATH").as_deref()) else {
        return failed("youtube: details need yt-dlp (https://github.com/yt-dlp/yt-dlp), not found on PATH");
    };
    let mut cmd = std::process::Command::new(ytdlp);
    cmd.args(["--skip-download", "--no-warnings"]);
    for f in FIELDS {
        cmd.args(["--print", &format!("%({f})j")]);
    }
    let out = cmd.arg(format!("https://www.youtube.com/watch?v={id}")).output()?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return failed(format!("youtube: yt-dlp failed: {}", err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim()));
    }
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    if let Some(p) = cache {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(p, &text);
    }
    Ok(parse_details(&text))
}

/// Where YouTube is: the video and how far in (or home), from its page.
fn session(tv: &Tv) -> Option<crate::provider::Session> {
    let raw = devtools::with_conn(tv, PACKAGE, |c| c.eval("JSON.stringify({h:location.hash,t:(document.querySelector('video')||{}).currentTime||0})")).ok()?;
    let v = crate::http::json(&raw)?;
    let hash = crate::http::text(&v, "h")?;
    Some(match video_id(&hash) {
        Some(id) => {
            let t = crate::http::number(&v, "t").unwrap_or(0.0).floor() as u64;
            crate::provider::Session {
                resume: None,
                link: format!("https://www.youtube.com/watch?v={id}&t={t}s"),
                said: format!("at {} in the same video", crate::nowplaying::clock(t as f64)),
            }
        }
        None => crate::provider::Session { link: "https://www.youtube.com/".into(), said: String::new(), resume: None },
    })
}

/// No commands of its own: `tveee app youtube` covers it (see `apps`).
pub fn commands() -> Vec<Command> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_sheets_go() {
        let t = |secs| std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs);
        // six videos (ids may contain '-'), one with two sheets; KEEP is 4
        let sheets: Vec<(String, std::time::SystemTime)> = vec![
            ("aaa-sb0.jpg".into(), t(1)),
            ("aaa-sb1.jpg".into(), t(60)),
            ("b-b-sb0.jpg".into(), t(2)),
            ("ccc-sb0.jpg".into(), t(30)),
            ("ddd-sb0.jpg".into(), t(40)),
            ("eee-sb0.jpg".into(), t(50)),
            ("fff-sb0.jpg".into(), t(3)),
        ];
        let mut gone = sheets_to_remove(&sheets);
        gone.sort();
        // aaa was used last (its second sheet), so both its sheets stay
        assert_eq!(gone, vec!["b-b-sb0.jpg".to_string(), "fff-sb0.jpg".to_string()]);
        assert!(sheets_to_remove(&sheets[..3]).is_empty());
    }

    #[test]
    fn storyboard_frames() {
        let board = parse_storyboard("WARNING: ignored\n5 5 0.1 120 90\n[\"https://a/M0.jpg?x=1\", \"https://a/M1.jpg\"]\n");
        // the warning line is the first non-empty: not numbers
        assert_eq!(board, None);
        let b = parse_storyboard("5 5 0.1 120 90\n[\"https://a/M0.jpg?x=1\", \"https://a/M1.jpg\"]\n").unwrap();
        assert_eq!((b.cols, b.width, b.sheets.len()), (5, 120, 2));
        // 0.1 fps: frame 0 at 0s, frame 7 at 70s (row 1, column 2), frame 25 on sheet 1
        assert_eq!(b.frame_at(0.0), Some((0, 0, 0)));
        assert_eq!(b.frame_at(75.0), Some((0, 240, 90)));
        assert_eq!(b.frame_at(250.0), Some((1, 0, 0)));
        // past the end: the last frame
        assert_eq!(b.frame_at(99999.0), Some((1, 480, 360)));
    }

    #[test]
    fn reads_json_strings() {
        assert_eq!(json_strings(r#"["a", "b\"c", "d\\e"]"#), ["a", "b\"c", "d\\e"]);
        assert!(json_strings("[]").is_empty());
    }


    #[test]
    fn deep_links_are_quoted() {
        let e = deep_link_expr("https://www.youtube.com/results?search_query=red%20panda");
        assert!(e.contains("{value:'https://www.youtube.com/results?search_query=red%20panda'}"), "{e}");
        // nothing in a link can end the JavaScript string
        let e = deep_link_expr("a'b\\c\nd");
        assert!(e.contains(r"{value:'a\'b\\c\u000ad'}"), "{e}");
    }

    #[test]
    fn reads_ytdlp_output() {
        let out = r#""Everyone\u2019s Letting AI Hack Their TVs\u2026 So I Tried It"
"RelfWolf"
"@RelfWolf"
"https://www.youtube.com/channel/UCWU67Tq18eRho_DN6z2FrZQ"
"20261115"
1051296
13384
574
[{"start_time": 0, "title": "Ads on a TV I paid for", "end_time": 54}, {"start_time": 54.0, "title": "Could anybody do this?", "end_time": 69}]
"Intro\n\u2022 Claude Code: https://code.claude.com/docs/en/setup\nbye"
"#;
        let d = parse_details(out);
        assert_eq!(
            d.fields,
            vec![
                ("channel", "RelfWolf (@RelfWolf)".to_string()),
                ("uploaded", "2026-11-15".into()),
                ("views", "1.05M".into()),
                ("likes", "13.4K".into()),
                ("length", "9:34".into()),
            ]
        );
        assert_eq!(d.chapters.len(), 2);
        assert_eq!(d.chapters[1], Chapter { start: 54.0, title: "Could anybody do this?".into() });
        assert_eq!(d.links[0], Link { label: "channel: RelfWolf".into(), go: Go::Link("https://www.youtube.com/@RelfWolf".into()) });
        assert_eq!(d.links[2].go, Go::Seek(54.0));
        assert_eq!(d.links[3].label, "Claude Code");
        // no chapters / missing values (NA)
        let d = parse_details("\"t\"\nNA\nNA\nNA\nNA\nNA\nNA\nNA\nNA\nNA\n");
        assert!(d.fields.is_empty() && d.chapters.is_empty() && d.links.is_empty());
        assert_eq!(video_id("https://www.youtube.com/watch?v=aN-65z6bvKk"), Some("aN-65z6bvKk".into()));
    }

    #[test]
    fn watch_links() {
        assert_eq!(
            watch_link("https://www.youtube.com/tv#/watch?v=nh5iCP5Ta_4&resume").as_deref(),
            Some("https://www.youtube.com/watch?v=nh5iCP5Ta_4")
        );
        assert_eq!(watch_link("https://www.youtube.com/tv#/"), None);
        assert_eq!(watch_link("https://www.youtube.com/tv#/watch?v=short"), None);
    }
}

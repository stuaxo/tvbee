//! Browsing Kodi's library and sources from tveee, without changing what
//! Kodi shows: folders are read with `Files.GetDirectory` (it lists any path
//! Kodi knows: `musicdb://`, `videodb://`, shares, add-ons), and Kodi only
//! hears about what's chosen to play.
//!
//! A place to browse is a link `media|name|path` (the name may be empty: a
//! typed location); something to play is `kodi-play:playlist|folder|file`.

use super::browse::{bare, path_crumbs, section_name, unescape};
use super::link::Link;
use super::{Result, Tv};
use serde_json::Value;
use tveee_core::more::Go;
use tveee_core::provider::{ContextList, ListCrumb, ListItem, ListPlace};

/// Kodi's top's key (as a list, and the libraries' way up).
const ROOT_KEY: &str = "kodi";

/// Which of Kodi's libraries.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Media {
    Music,
    Video,
}

impl Media {
    /// As `Files.GetDirectory`'s media, and links, call it.
    pub fn name(self) -> &'static str {
        match self {
            Media::Music => "music",
            Media::Video => "video",
        }
    }
    /// Its window's id (the lists' keys start with it, as on the screen).
    pub fn window(self) -> u64 {
        match self {
            Media::Music => 10502,
            Media::Video => 10025,
        }
    }
    /// Its window, as `GUI.ActivateWindow` names it.
    pub fn window_name(self) -> &'static str {
        match self {
            Media::Music => "music",
            Media::Video => "videos",
        }
    }
    /// Its library's top.
    pub fn db(self) -> &'static str {
        match self {
            Media::Music => "musicdb://",
            Media::Video => "videodb://",
        }
    }
    /// Its playlist (Kodi's: 0 music, 1 video).
    pub fn playlist(self) -> u32 {
        match self {
            Media::Music => 0,
            Media::Video => 1,
        }
    }
    fn top(self) -> &'static str {
        match self {
            Media::Music => "Music",
            Media::Video => "Videos",
        }
    }
    pub fn of_window(id: u64) -> Option<Media> {
        match id {
            10502 => Some(Media::Music),
            10025 => Some(Media::Video),
            _ => None,
        }
    }
    pub fn parse(s: &str) -> Option<Media> {
        match s {
            "music" => Some(Media::Music),
            "video" => Some(Media::Video),
            _ => None,
        }
    }
}

/// The link to browse `path` (named `name`, if known).
pub fn link(m: Media, name: &str, path: &str) -> String {
    Link::browse(m, name, path).to_string()
}

/// Whether `at` is a place to browse (Kodi's top, or a folder).
pub fn is_link(at: &str) -> bool {
    matches!(Link::parse(at), Some(Link::Root | Link::Browse { .. }))
}

/// Kodi's top: its categories, each its library's top.
pub fn root() -> ContextList {
    let category = |m: Media, what: &str| ListItem {
        section: None,
        id: m.db().to_string(),
        label: m.top().to_string(),
        meta: Some(what.to_string()),
        folder: true,
        go: Go::Link(link(m, m.top(), "")),
    };
    ContextList {
        title: "Kodi".into(),
        kind: "folder",
        key: ROOT_KEY.into(),
        items: vec![category(Media::Music, "library and shares"), category(Media::Video, "library and shares")],
        playing: None,
        at: None,
        up: None,
        reopen: Some(Link::Root.go()),
        crumbs: Vec::new(),
        crumbs_whole: true,
        location: None,
        open_location: Some("music||{}".into()),
        browsed: true,
    }
}

/// A [`link`]'s media, name and path; a bare path (typed) is music's,
/// unless it's in the video library.
pub fn parse(at: &str) -> (Media, String, String) {
    match Link::parse(at) {
        Some(Link::Browse { media, name, path }) => (media, name, path),
        _ => {
            let m = if at.starts_with("videodb://") { Media::Video } else { Media::Music };
            (m, String::new(), at.trim().to_string())
        }
    }
}

/// The folder `path` is in: "" is the library's top (none above it).
pub fn parent(path: &str) -> Option<String> {
    let b = bare(path);
    if b.is_empty() {
        return None;
    }
    let p = b.trim_end_matches('/');
    let (lead, rest) = match p.split_once("://") {
        Some((s, r)) => (format!("{s}://"), r),
        None => (String::new(), p),
    };
    if rest.is_empty() {
        return Some(String::new());
    }
    Some(match rest.rfind('/') {
        Some(i) => format!("{lead}{}/", &rest[..i]),
        None if lead.is_empty() => "/".into(),
        None => String::new(),
    })
}

/// A name for `path`, when nobody said: its last folder's (a share's), or
/// its library section's; else the path.
fn name_of(m: Media, path: &str) -> String {
    let b = bare(path);
    if b.is_empty() || b == m.db() {
        return m.top().into();
    }
    let (scheme, rest) = b.split_once("://").unwrap_or(("", b));
    let parts: Vec<&str> = rest.split('/').filter(|p| !p.is_empty()).collect();
    match (scheme, parts.as_slice()) {
        ("musicdb" | "videodb", [one]) => section_name(one),
        ("musicdb" | "videodb", _) => b.to_string(),
        (_, [.., last]) => unescape(last),
        _ => b.to_string(),
    }
}

/// How long, as Kodi shows it: 4:24, 1:02:03.
fn duration(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

/// The items in `Files.GetDirectory`'s (or `Files.GetSources`') answer
/// `v` for folder `dir`, in part `section`: folders open here, files play
/// from their folder.
pub fn items_of(m: Media, dir: &str, v: &Value, key: &str, section: Option<&str>) -> Vec<ListItem> {
    let text = |f: &Value, k: &str| f.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string();
    let Some(files) = v.get(key).and_then(Value::as_array) else { return Vec::new() };
    files
        .iter()
        .filter_map(|f| {
            let file = text(f, "file");
            if file.is_empty() {
                return None;
            }
            let label = text(f, "label");
            // sources are folders, and say nothing of it
            let folder = key == "sources" || text(f, "filetype") == "directory";
            let track = f.get("track").and_then(Value::as_u64).unwrap_or(0);
            let shown = if !folder && track > 0 { format!("{track:02}. {label}") } else { label.clone() };
            let artists: Vec<&str> = f.get("artist").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
            let meta = match f.get("duration").and_then(Value::as_u64) {
                Some(d) if !folder && d > 0 => Some(duration(d)),
                _ if folder && !artists.is_empty() => Some(artists.join(", ")),
                _ => None,
            };
            let go =
                if folder { Link::browse(m, &label, &file).go() } else { Link::Play { playlist: m.playlist(), dir: dir.to_string(), file: file.clone() }.go() };
            Some(ListItem { section: section.map(str::to_string), id: file, label: shown, meta, folder, go })
        })
        .collect()
}

/// The list at `at` (a [`link`], or a path typed), read from Kodi without
/// changing its screen.
pub fn list_at(tv: &Tv, at: &str) -> Result<ContextList> {
    if Link::parse(at) == Some(Link::Root) {
        return Ok(root());
    }
    let (m, name, path) = parse(at);
    let path = if bare(&path) == m.db() { String::new() } else { path };
    let dir = if path.is_empty() { m.db().to_string() } else { path.clone() };
    let r = super::rpc::ask(tv, &super::rpc::GetDirectory { directory: &dir, media: m.name() })?;
    let mut items;
    if path.is_empty() {
        // the top: the library, then the shares and folders added to Kodi
        items = items_of(m, &dir, &r, "files", Some("Library"));
        if let Ok(s) = super::rpc::ask(tv, &super::rpc::GetSources { media: m.name() }) {
            items.extend(items_of(m, &dir, &s, "sources", Some("Files")));
        }
    } else {
        items = items_of(m, &dir, &r, "files", None);
    }
    let mut l = shape(m, &name, &path, items);
    l.playing = super::browse::playing_file(tv).and_then(|f| l.find_playing(&f));
    Ok(l)
}

/// The list for folder `path` (named `name`) of `m` and its `items`: its
/// key (as the screen's), the way up, the breadcrumbs its path names.
pub fn shape(m: Media, name: &str, path: &str, items: Vec<ListItem>) -> ContextList {
    let title = if name.is_empty() { name_of(m, path) } else { name.to_string() };
    let key_for = |p: &str| format!("{}|{}", m.window(), bare(p));
    // a library's top: Kodi's top above it (its categories)
    let up = match parent(path) {
        Some(p) => Some((key_for(&p), Go::Link(link(m, "", &p)))),
        None => Some((ROOT_KEY.to_string(), Link::Root.go())),
    };
    let (mut crumbs, crumbs_whole) = path_crumbs(m.window(), path);
    for c in &mut crumbs {
        let p = c.key.split_once('|').map_or("", |(_, p)| p).to_string();
        c.go = Some(Go::Link(link(m, &c.title, &p)));
    }
    // every way starts at Kodi's top: a click from another category
    crumbs.insert(0, ListCrumb { title: "Kodi".into(), key: ROOT_KEY.into(), go: Some(Link::Root.go()) });
    ContextList {
        title: title.clone(),
        kind: "folder",
        key: key_for(path),
        items,
        playing: None,
        at: None,
        up,
        reopen: Some(Go::Link(link(m, &title, path))),
        crumbs,
        crumbs_whole,
        location: Some(if path.is_empty() { m.db().to_string() } else { path.to_string() }),
        open_location: Some(format!("{}||{{}}", m.name())),
        browsed: true,
    }
}

/// Where to start browsing: the folder Kodi shows, if it's showing its
/// music or videos (on the item it has highlighted); else music's top.
pub fn start(tv: &Tv) -> Result<ContextList> {
    if let Some(p) = super::browse::place(tv)?
        && let Some(m) = Media::of_window(p.window)
        && let Ok(mut l) = list_at(tv, &link(m, &p.name, &p.path))
    {
        let place = ListPlace { key: l.key.clone(), focus_label: p.focus.0.clone(), focus_id: p.focus.1.clone(), playing_id: String::new() };
        l.at = l.find(&place);
        return Ok(l);
    }
    list_at(tv, &link(Media::Music, "", ""))
}

/// Play a [`Link::Play`]: its whole folder queued in Kodi's playlist,
/// started at the file (so next and previous go along the folder); just
/// the file if that can't be done (or `link` is a file's own path).
pub fn play(tv: &Tv, link: &str) -> Result<String> {
    let (pl, dir, file) = match Link::parse(link) {
        Some(Link::Play { playlist, dir, file }) => (playlist, dir, file),
        _ => (0, String::new(), link.to_string()),
    };
    let media = if pl == 1 { "video" } else { "music" };
    let queued = || -> Result<Option<usize>> {
        if dir.is_empty() {
            return Ok(None);
        }
        use super::rpc::{PlaylistAddDirectory, PlaylistClear, PlaylistFiles, ask};
        ask(tv, &PlaylistClear(pl))?;
        ask(tv, &PlaylistAddDirectory { playlist: pl, directory: &dir, media })?;
        let files = ask(tv, &PlaylistFiles(pl))?;
        Ok(files.iter().position(|f| *f == file))
    };
    match queued() {
        Ok(Some(n)) => super::rpc::ask(tv, &super::rpc::Open::Playlist { playlist: pl, position: n })?,
        _ => super::rpc::ask(tv, &super::rpc::Open::File(&file))?,
    };
    let _ = tveee_core::builtin::apps::bring_to_front(tv, super::PACKAGE);
    Ok("playing it in Kodi".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_and_parents() {
        assert_eq!(parse("music|A Polished Solid|musicdb://albums/251/"), (Media::Music, "A Polished Solid".into(), "musicdb://albums/251/".into()));
        assert_eq!(parse("video||smb://nas/films/"), (Media::Video, String::new(), "smb://nas/films/".into()));
        // typed, bare
        assert_eq!(parse("videodb://movies/titles/").0, Media::Video);
        assert!(is_link("music||") && !is_link("smb://nas/") && !is_link("kodi-play:0|a|b"));
        assert_eq!(parent("musicdb://albums/251/-2/?albumid=251&discid=1").as_deref(), Some("musicdb://albums/251/"));
        assert_eq!(parent("musicdb://albums/").as_deref(), Some(""));
        assert_eq!(parent("smb://nas/music/").as_deref(), Some("smb://nas/"));
        assert_eq!(parent("smb://nas/").as_deref(), Some(""));
        assert_eq!(parent("/storage/music/").as_deref(), Some("/storage/"));
        assert_eq!(parent("/storage/").as_deref(), Some("/"));
        assert_eq!(parent("/").as_deref(), Some(""));
        assert_eq!(parent(""), None);
        assert_eq!(name_of(Media::Music, "musicdb://albums/"), "Albums");
        assert_eq!(name_of(Media::Music, "smb://nas/music/Aphex%20Twin/"), "Aphex Twin");
        assert_eq!(name_of(Media::Music, ""), "Music");
    }

    #[test]
    fn a_folder_from_kodis_answer() {
        // as Kodi answered for an album (Files.GetDirectory)
        let v: Value = serde_json::from_str(r#"{"files":[
            {"artist":["Luke Vibert"],"duration":519,"file":"smb://werner/media/Music/A Polished Solid/01 A Polished Solid.mp3","filetype":"file","label":"A Polished Solid","track":1,"type":"song"},
            {"file":"musicdb://albums/251/1/","filetype":"directory","label":"Disc 1","type":"unknown"}]}"#)
        .unwrap();
        let items = items_of(Media::Music, "musicdb://albums/251/", &v, "files", None);
        assert_eq!(items[0].label, "01. A Polished Solid");
        assert_eq!(items[0].meta.as_deref(), Some("8:39"));
        assert!(!items[0].folder);
        assert_eq!(items[0].go, Go::Link("kodi-play:0|musicdb://albums/251/|smb://werner/media/Music/A Polished Solid/01 A Polished Solid.mp3".into()));
        assert!(items[1].folder);
        assert_eq!(items[1].go, Go::Link("music|Disc 1|musicdb://albums/251/1/".into()));
        let l = shape(Media::Music, "A Polished Solid", "musicdb://albums/251/", items);
        assert!(l.browsed);
        assert_eq!(l.key, "10502|musicdb://albums/251/");
        assert_eq!(l.up, Some(("10502|musicdb://albums/".into(), Go::Link("music||musicdb://albums/".into()))));
        assert_eq!(l.crumbs.iter().map(|c| c.title.as_str()).collect::<Vec<_>>(), ["Kodi", "Music", "Albums"]);
        assert_eq!(l.crumbs[2].go, Some(Go::Link("music|Albums|musicdb://albums/".into())));
        assert_eq!(l.crumbs[0].go, Some(Link::Root.go()));
        assert_eq!(l.open_location.as_deref(), Some("music||{}"));
        // the top: the library, then the sources
        let s: Value = serde_json::from_str(r#"{"sources":[{"file":"smb://192.168.1.80/media/","label":"media"}]}"#).unwrap();
        let src = items_of(Media::Music, "musicdb://", &s, "sources", Some("Files"));
        assert!(src[0].folder && src[0].section.as_deref() == Some("Files"));
        let top = shape(Media::Music, "", "", src);
        assert_eq!((top.title.as_str(), top.location.as_deref()), ("Music", Some("musicdb://")));
        // up from a library's top: Kodi's, its categories
        assert_eq!(top.up, Some(("kodi".into(), Link::Root.go())));
        assert_eq!(top.crumbs.iter().map(|c| c.title.as_str()).collect::<Vec<_>>(), ["Kodi"]);
        let r = root();
        assert_eq!(r.items.iter().map(|i| i.label.as_str()).collect::<Vec<_>>(), ["Music", "Videos"]);
        assert_eq!(r.items[1].go, Go::Link("video|Videos|".into()));
        assert!(r.up.is_none() && r.browsed && is_link(&Link::Root.to_string()));
    }
}

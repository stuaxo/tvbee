//! Where Kodi's screen is (its window, the folder shown, what's
//! highlighted, what's playing), read through its API without changing it:
//! where browsing starts ([`super::library`]), and the ▶ mark. Also what
//! a folder's path can say of where it is (the breadcrumbs).

use super::{Result, Tv, call};
use serde_json::Value;
use tveee_core::more::Go;
use tveee_core::provider::{ListCrumb, ListPlace};

/// Where Kodi's screen is: its window, the folder shown and how many rows.
#[derive(Debug, Clone, PartialEq)]
pub struct Place {
    pub window: u64,
    pub path: String,
    pub name: String,
    pub rows: usize,
    /// The highlighted row's label and path (to find it among the rows,
    /// whatever Kodi numbers from).
    pub focus: (String, String),
}

/// Kodi's windows that list things, by id, and the name to open a folder in
/// one (`GUI.ActivateWindow`).
pub fn window_name(id: u64) -> Option<&'static str> {
    Some(match id {
        10025 => "videos",
        10502 => "music",
        10002 => "pictures",
        10001 => "programs",
        10040 => "addonbrowser",
        _ => return None,
    })
}

fn labels(tv: &Tv, names: &[String]) -> Result<Value> {
    call(tv, "XBMC.GetInfoLabels", &serde_json::json!({ "labels": names }).to_string())
}

fn text(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string()
}

/// Where Kodi's screen is, from its labels and window; None when it isn't
/// showing a list (the home screen, the player, settings).
pub fn place_of(window: u64, l: &Value) -> Option<Place> {
    let path = text(l, "Container.FolderPath");
    let rows: usize = text(l, "Container.NumItems").parse().unwrap_or(0);
    // a section's top (Music: Genres, Artists, Albums...) has no folder path
    let window_title = window_name(window).filter(|_| rows > 0)?;
    let name = Some(text(l, "Container.FolderName")).filter(|n| !n.is_empty()).unwrap_or_else(|| {
        let mut c = window_title.chars();
        c.next().map_or_else(String::new, |f| f.to_uppercase().chain(c).collect())
    });
    let focus = (text(l, "ListItem.Label"), text(l, "ListItem.FileNameAndPath"));
    Some(Place { window, path, name, rows, focus })
}

/// Where Kodi's screen is now (one small call and a window read).
pub fn place(tv: &Tv) -> Result<Option<Place>> {
    let w = call(tv, "GUI.GetProperties", r#"{"properties":["currentwindow"]}"#)?;
    let window = w.pointer("/currentwindow/id").and_then(Value::as_u64).unwrap_or(0);
    let names: Vec<String> = ["Container.FolderPath", "Container.FolderName", "Container.NumItems", "ListItem.Label", "ListItem.FileNameAndPath"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    Ok(place_of(window, &labels(tv, &names)?))
}

/// A path without what Kodi adds after `?` (`?albumid=224&discid=1`):
/// the same folder, however Kodi got there.
pub fn bare(path: &str) -> &str {
    path.split('?').next().unwrap_or(path)
}

/// `%20` and friends in a path, back to what they stand for.
pub fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        // (no let chains here: this crate builds with Rust 1.85)
        let escaped = if b[i] == b'%' && i + 2 < b.len() { u8::from_str_radix(&s[i + 1..i + 3], 16).ok() } else { None };
        if let Some(v) = escaped {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The library's sections, as their paths name them.
pub fn section_name(part: &str) -> String {
    match part {
        "top100" => "Top 100".into(),
        "recentlyaddedalbums" => "Recently added albums".into(),
        "recentlyplayedalbums" => "Recently played albums".into(),
        "singles" => "Singles".into(),
        "tvshows" => "TV shows".into(),
        "musicvideos" => "Music videos".into(),
        "inprogresstvshows" => "In progress TV shows".into(),
        other => {
            let mut c = other.chars();
            c.next().map_or_else(String::new, |f| f.to_uppercase().chain(c).collect())
        }
    }
}

/// The lists a folder is in, named from its path (outermost first, not
/// itself), and whether they go all the way to it: a share's or a folder's
/// path names every step (`smb://nas/music/Artist/Album/`); the library's
/// only its section (`musicdb://albums/251/` -> Albums, then a number);
/// an add-on's nothing. The window's own top comes first.
pub fn path_crumbs(window: u64, path: &str) -> (Vec<ListCrumb>, bool) {
    let Some(w) = window_name(window) else { return (Vec::new(), false) };
    let top_title = {
        let mut c = w.chars();
        c.next().map_or_else(String::new, |f| f.to_uppercase().chain(c).collect::<String>())
    };
    let crumb = |title: String, p: &str| ListCrumb {
        title,
        key: format!("{window}|{}", bare(p)),
        go: Some(Go::Link(if p.is_empty() { format!("kodi-window:{w}") } else { format!("kodi-window:{w}|{p}") })),
    };
    let mut out = vec![crumb(top_title, "")];
    let path = bare(path);
    if path.is_empty() {
        // the window's top itself
        return (Vec::new(), true);
    }
    let (scheme, rest) = path.split_once("://").map_or(("", path), |(s, r)| (s, r));
    match scheme {
        "musicdb" | "videodb" => {
            let parts: Vec<&str> = rest.split('/').filter(|p| !p.is_empty()).collect();
            if let Some(first) = parts.first() {
                if parts.len() > 1 {
                    out.push(crumb(section_name(first), &format!("{scheme}://{first}/")));
                }
            }
            (out, parts.len() <= 1)
        }
        "plugin" | "addons" => (out, false),
        // a share or a folder: every step has a name
        _ => {
            let parts: Vec<&str> = rest.split('/').filter(|p| !p.is_empty()).collect();
            let lead = if scheme.is_empty() { "/".to_string() } else { format!("{scheme}://") };
            let mut so_far = lead.clone();
            for part in parts.iter().take(parts.len().saturating_sub(1)) {
                so_far.push_str(part);
                so_far.push('/');
                out.push(crumb(unescape(part), &so_far));
            }
            (out, true)
        }
    }
}

/// What a list is: its window and folder (another folder, another list;
/// what Kodi adds after `?` aside).
pub fn key_of(p: &Place) -> String {
    format!("{}|{}", p.window, bare(&p.path))
}

/// Where Kodi's list is, for the ui to follow.
pub fn list_place(p: &Place) -> ListPlace {
    ListPlace { key: key_of(p), focus_label: p.focus.0.clone(), focus_id: p.focus.1.clone(), playing_id: String::new() }
}

/// The file Kodi's player is playing, if it's playing one.
pub fn playing_file(tv: &Tv) -> Option<String> {
    let (id, _) = super::player_id(tv).ok()??;
    super::rpc::ask(tv, &super::rpc::PlayingFile(id)).ok()?
}

/// Where Kodi's list is, and what's playing in it (by its file).
pub fn place_now(tv: &Tv) -> Result<Option<ListPlace>> {
    let Some(p) = place(tv)? else { return Ok(None) };
    let mut l = list_place(&p);
    l.playing_id = playing_file(tv).unwrap_or_default();
    Ok(Some(l))
}

#[cfg(test)]
mod tests {

    #[test]
    fn crumbs_from_the_path() {
        let titles = |(c, whole): (Vec<ListCrumb>, bool)| (c.iter().map(|x| x.title.clone()).collect::<Vec<_>>(), whole);
        // a share: every folder, named, the whole way
        let (c, whole) = path_crumbs(10502, "smb://nas/music/Aphex%20Twin/Selected%20Ambient%20Works/");
        assert_eq!(titles((c.clone(), whole)), (vec!["Music".to_string(), "nas".into(), "music".into(), "Aphex Twin".into()], true));
        assert_eq!(c[3].go, Some(Go::Link("kodi-window:music|smb://nas/music/Aphex%20Twin/".into())));
        assert_eq!(c[0].go, Some(Go::Link("kodi-window:music".into())));
        // the library: its section, then numbers (not the whole way)
        assert_eq!(titles(path_crumbs(10502, "musicdb://albums/251/-2/?albumid=251&discid=1")), (vec!["Music".to_string(), "Albums".into()], false));
        assert_eq!(titles(path_crumbs(10502, "musicdb://albums/")), (vec!["Music".to_string()], true));
        // the window's top: nothing above it
        assert_eq!(titles(path_crumbs(10502, "")), (vec![], true));
        // an add-on: only the window
        assert_eq!(titles(path_crumbs(10025, "plugin://plugin.video.x/?a=b")), (vec!["Videos".to_string()], false));
        assert_eq!(unescape("Aphex%20Twin"), "Aphex Twin");
    }

    use super::*;
    use serde_json::json;

    fn place_labels() -> Value {
        json!({
            "Container.FolderPath": "musicdb://artists/",
            "Container.FolderName": "Artists",
            "Container.NumItems": "3",
            "ListItem.Label": "Radiohead",
            "ListItem.FileNameAndPath": "",
        })
    }

    #[test]
    fn where_it_is() {
        let p = place_of(10502, &place_labels()).expect("a list");
        assert_eq!((p.path.as_str(), p.name.as_str(), p.rows), ("musicdb://artists/", "Artists", 3));
        // the home screen, the player, settings: not lists
        assert!(place_of(10000, &place_labels()).is_none());
        assert!(place_of(10502, &json!({ "Container.FolderPath": "", "Container.NumItems": "0" })).is_none());
        // the top of Music (Genres, Artists, Albums...): a list, no path, named after it
        let top = place_of(10502, &json!({ "Container.FolderPath": "", "Container.FolderName": "", "Container.NumItems": "15", "ListItem.Label": "Genres" }))
            .expect("a list");
        assert_eq!((top.name.as_str(), top.path.as_str(), top.rows), ("Music", "", 15));
    }
}

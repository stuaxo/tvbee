//! The list Kodi is showing (Music > Artists, a folder of videos, an
//! add-on's listing), read through its API to be chosen from elsewhere:
//! `tveee list`, later the ui's list pane.
//!
//! Read as Kodi shows it (its rows' labels, in its order and sort), not by
//! listing the folder again: for an add-on's listing that would run the
//! add-on once more. Reading never changes what's on the screen; choosing
//! does (a folder opens in the same window, a file plays).

use super::{call, Result, Tv};
use serde_json::Value;
use tveee_core::more::Go;
use tveee_core::provider::{ContextList, ListItem};

/// Most rows read (a library of thousands: the first ones).
const MAX: usize = 500;
/// Rows asked for in one call.
const BATCH: usize = 100;

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

fn booleans(tv: &Tv, names: &[String]) -> Result<Value> {
    call(tv, "XBMC.GetInfoBooleans", &serde_json::json!({ "booleans": names }).to_string())
}

fn text(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string()
}

/// Where Kodi's screen is, from its labels and window; None when it isn't
/// showing a list (the home screen, the player, settings).
pub fn place_of(window: u64, l: &Value) -> Option<Place> {
    let path = text(l, "Container.FolderPath");
    let rows: usize = text(l, "Container.NumItems").parse().unwrap_or(0);
    if path.is_empty() || rows == 0 || window_name(window).is_none() {
        return None;
    }
    let name = text(l, "Container.FolderName");
    let focus = (text(l, "ListItem.Label"), text(l, "ListItem.FileNameAndPath"));
    Some(Place { window, path, name, rows, focus })
}

/// Where Kodi's screen is now (one small call and a window read).
pub fn place(tv: &Tv) -> Result<Option<Place>> {
    let w = call(tv, "GUI.GetProperties", r#"{"properties":["currentwindow"]}"#)?;
    let window = w.pointer("/currentwindow/id").and_then(Value::as_u64).unwrap_or(0);
    let names: Vec<String> = ["Container.FolderPath", "Container.FolderName", "Container.NumItems", "ListItem.Label", "ListItem.FileNameAndPath"].iter().map(|s| s.to_string()).collect();
    Ok(place_of(window, &labels(tv, &names)?))
}

/// One row as Kodi shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub label: String,
    pub label2: String,
    pub folder: bool,
    /// A folder's path, or a file's.
    pub path: String,
}

fn row_names(i: usize) -> [String; 4] {
    let p = format!("Container.ListItemAbsolute({i})");
    [format!("{p}.Label"), format!("{p}.Label2"), format!("{p}.FolderPath"), format!("{p}.FileNameAndPath")]
}

/// Rows `range` from Kodi's labels and booleans for them; empty rows (past
/// the end) and the parent row ("..": Back does that) left out.
pub fn rows_of(range: std::ops::Range<usize>, l: &Value, b: &Value) -> Vec<Row> {
    range
        .filter_map(|i| {
            let [label, label2, folder_path, file] = row_names(i);
            let label = text(l, &label);
            if label.is_empty() || label == ".." {
                return None;
            }
            let folder = b.get(format!("Container.ListItemAbsolute({i}).IsFolder")).and_then(Value::as_bool).unwrap_or(false);
            let path = if folder { text(l, &folder_path) } else { text(l, &file) };
            Some(Row { label, label2: text(l, &label2), folder, path })
        })
        .collect()
}

/// The rows Kodi shows (up to [`MAX`]), in its order.
pub fn rows(tv: &Tv, p: &Place) -> Result<Vec<Row>> {
    // a row more than it counts: the parent row may come first
    let end = (p.rows + 1).min(MAX);
    let mut out = Vec::new();
    let mut start = 0;
    while start <= end {
        let range = start..(start + BATCH).min(end + 1);
        let names: Vec<String> = range.clone().flat_map(row_names).collect();
        let bools: Vec<String> = range.clone().map(|i| format!("Container.ListItemAbsolute({i}).IsFolder")).collect();
        out.extend(rows_of(range.clone(), &labels(tv, &names)?, &booleans(tv, &bools)?));
        start = range.end;
    }
    Ok(out)
}

/// What choosing `row` does: a folder opens in the same window, anything
/// else plays (both as links Kodi's provider opens: [`super::window_params`]
/// for `kodi-window:`, `Player.Open` for a file).
pub fn go_for(row: &Row, window: u64) -> Go {
    match (row.folder, window_name(window)) {
        (true, Some(w)) => Go::Link(format!("kodi-window:{w}|{}", row.path)),
        _ => Go::Link(row.path.clone()),
    }
}

/// The list from where Kodi is and its rows.
pub fn list_of(p: &Place, rows: &[Row]) -> ContextList {
    let items = rows
        .iter()
        .map(|r| ListItem { label: r.label.clone(), meta: Some(r.label2.clone()).filter(|m| !m.is_empty()), folder: r.folder, go: go_for(r, p.window) })
        .collect();
    // the highlighted row: by its path, else its label
    let (label, path) = &p.focus;
    let at = rows.iter().position(|r| !path.is_empty() && r.path == *path).or_else(|| rows.iter().position(|r| r.label == *label));
    let title = if p.name.is_empty() { p.path.clone() } else { p.name.clone() };
    ContextList { title, kind: "folder", key: format!("{}|{}", p.window, p.path), items, at }
}

/// The list Kodi is showing, if it's showing one.
pub fn list(tv: &Tv) -> Result<Option<ContextList>> {
    let Some(p) = place(tv)? else { return Ok(None) };
    Ok(Some(list_of(&p, &rows(tv, &p)?)))
}

#[cfg(test)]
mod tests {
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
    }

    #[test]
    fn rows_and_choosing() {
        let l = json!({
            "Container.ListItemAbsolute(0).Label": "..",
            "Container.ListItemAbsolute(1).Label": "Portishead",
            "Container.ListItemAbsolute(1).FolderPath": "musicdb://artists/7/",
            "Container.ListItemAbsolute(2).Label": "Radiohead",
            "Container.ListItemAbsolute(2).Label2": "9 albums",
            "Container.ListItemAbsolute(2).FolderPath": "musicdb://artists/9/",
            "Container.ListItemAbsolute(3).Label": "Exit Music.flac",
            "Container.ListItemAbsolute(3).FileNameAndPath": "smb://nas/music/exit.flac",
        });
        let b = json!({
            "Container.ListItemAbsolute(1).IsFolder": true,
            "Container.ListItemAbsolute(2).IsFolder": true,
            "Container.ListItemAbsolute(3).IsFolder": false,
        });
        let rows = rows_of(0..5, &l, &b);
        // the parent row and the empty ones past the end are left out
        assert_eq!(rows.iter().map(|r| r.label.as_str()).collect::<Vec<_>>(), vec!["Portishead", "Radiohead", "Exit Music.flac"]);
        let list = list_of(&place_of(10502, &place_labels()).unwrap(), &rows);
        assert_eq!((list.title.as_str(), list.kind, list.at), ("Artists", "folder", Some(1)));
        assert_eq!(list.key, "10502|musicdb://artists/");
        assert_eq!(list.items[1].meta.as_deref(), Some("9 albums"));
        // a folder opens in the same window; a file plays
        assert_eq!(list.items[0].go, Go::Link("kodi-window:music|musicdb://artists/7/".into()));
        assert_eq!(list.items[2].go, Go::Link("smb://nas/music/exit.flac".into()));
        assert!(list.items[0].folder && !list.items[2].folder);
    }
}

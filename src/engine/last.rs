//! What the ui last saw on the TV, kept between runs so the first frame
//! isn't blank while the first snapshot is read (a second or two).

use super::*;

/// What's kept: the what line, the title and the volume, and when.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LastSeen {
    pub what: String,
    pub title: Option<String>,
    pub volume: Option<(u32, u32)>,
    /// Unix seconds.
    pub at: i64,
}

/// Older than this, it's not worth showing.
const MAX_AGE: i64 = 12 * 3600;

fn path(ip: &str) -> Option<std::path::PathBuf> {
    crate::plugin::cache_path(ip).map(|p| p.with_extension("ui-last"))
}

pub fn format(l: &LastSeen) -> String {
    let mut out = format!("at={}\nwhat={}\n", l.at, l.what.replace('\n', " "));
    if let Some(t) = &l.title {
        out += &format!("title={}\n", t.replace('\n', " "));
    }
    if let Some((v, max)) = l.volume {
        out += &format!("volume={v}/{max}\n");
    }
    out
}

pub fn parse(text: &str) -> Option<LastSeen> {
    let mut l = LastSeen::default();
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        match k {
            "at" => l.at = v.parse().ok()?,
            "what" => l.what = v.to_string(),
            "title" => l.title = Some(v.to_string()),
            "volume" => l.volume = v.split_once('/').and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?))),
            _ => {}
        }
    }
    (!l.what.is_empty() && l.at > 0).then_some(l)
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// What was last seen, if it's recent.
pub fn load(ip: &str) -> Option<LastSeen> {
    let l = parse(&std::fs::read_to_string(path(ip)?).ok()?)?;
    (now() - l.at < MAX_AGE).then_some(l)
}

/// Keep what a snapshot shows (not while in standby: there's nothing to see).
pub(crate) fn of(s: &Snapshot) -> Option<LastSeen> {
    let a = s.activity.as_ref().filter(|_| !s.standby)?;
    Some(LastSeen { what: a.what.clone(), title: a.title.clone(), volume: s.volume, at: now() })
}

pub fn save(ip: &str, l: &LastSeen) {
    let Some(p) = path(ip) else { return };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(p, format(l));
}

/// A snapshot to draw until the first real one comes.
pub(crate) fn seed(l: &LastSeen) -> Snapshot {
    let mut a = Activity::just(l.what.clone());
    a.title = l.title.clone();
    Snapshot { activity: Some(a), ..Snapshot::default() }
}

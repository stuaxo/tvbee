//! What Kodi's JSON-RPC answers hold, as structs (serde): what a method's
//! result is, said once. Fields Kodi leaves out are their defaults, and a
//! `null` reads as empty, so an odd answer doesn't fail.

use serde::{Deserialize, Deserializer};

/// A string, `null` as empty.
fn text<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(Option::<String>::deserialize(d)?.unwrap_or_default())
}

/// A file, folder or source in a list (`Files.GetDirectory`,
/// `Files.GetSources`, `Playlist.GetItems`).
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct Entry {
    /// Its path.
    #[serde(deserialize_with = "text")]
    pub file: String,
    /// Its name, as Kodi shows it.
    #[serde(deserialize_with = "text")]
    pub label: String,
    /// "directory" or "file" (sources don't say).
    #[serde(deserialize_with = "text")]
    pub filetype: String,
    /// The library's title, when it's a song or video.
    #[serde(deserialize_with = "text")]
    pub title: String,
    /// Its track number (0: none).
    pub track: u64,
    /// Its length in seconds (0: not known).
    pub duration: u64,
    /// Its artists.
    pub artist: Vec<String>,
}

/// `Files.GetDirectory`'s result.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Directory {
    /// What's in the folder.
    pub files: Vec<Entry>,
}

/// `Files.GetSources`' result.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Sources {
    /// The shares and folders added to Kodi.
    pub sources: Vec<Entry>,
}

/// `Playlist.GetItems`' result.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Items {
    /// The playlist's items, in order.
    pub items: Vec<Entry>,
}

/// An audio stream or subtitle of the video playing.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct Stream {
    /// Kodi's index for it.
    pub index: Option<u64>,
    /// Its name ("Commentary"), often empty.
    #[serde(deserialize_with = "text")]
    pub name: String,
    /// Its language ("eng").
    #[serde(deserialize_with = "text")]
    pub language: String,
    /// Its codec ("ac3"; audio only).
    #[serde(deserialize_with = "text")]
    pub codec: String,
    /// Its channels (audio only).
    pub channels: Option<u64>,
}

/// `Player.GetProperties`' result, for the tracks.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Tracks {
    /// The audio streams.
    pub audiostreams: Vec<Stream>,
    /// The audio stream playing.
    pub currentaudiostream: Option<Stream>,
    /// The subtitles.
    pub subtitles: Vec<Stream>,
    /// The subtitle chosen (shown only if `subtitleenabled`).
    pub currentsubtitle: Option<Stream>,
    /// Whether subtitles are showing.
    pub subtitleenabled: bool,
}

/// Read a result as `T` (an answer that isn't one: its defaults).
pub fn read<T: for<'de> Deserialize<'de> + Default>(v: &serde_json::Value) -> T {
    T::deserialize(v).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn odd_answers_still_read() {
        // missing fields, nulls, and a field Kodi added
        let d: Directory = read(&serde_json::json!({ "files": [{ "file": "a", "label": null, "new": 1 }, {}] }));
        assert_eq!((d.files[0].file.as_str(), d.files[0].label.as_str(), d.files[0].track), ("a", "", 0));
        assert_eq!(d.files.len(), 2);
        // not an object at all: empty
        let d: Directory = read(&serde_json::json!("pong"));
        assert!(d.files.is_empty());
    }
}

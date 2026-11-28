//! Kodi's links, as tveee passes them around (`Go::Link`'s text): one type,
//! written one way ([`std::fmt::Display`]) and read one way ([`Link::parse`]),
//! instead of strings built and split in a dozen places (the plan's part 8).
//!
//! | Link | Written |
//! |---|---|
//! | Kodi's top (its categories) | `kodi\|` |
//! | A folder to browse in tveee | `music\|Name\|musicdb://albums/1/` |
//! | A file to play from its folder | `kodi-play:0\|musicdb://albums/1/\|smb://…/01.mp3` |
//! | A window on Kodi's screen | `kodi-window:music` or `kodi-window:music\|smb://…/` |
//! | The queue playing | `kodi-queue:0` |
//! | An item of it, to play from | `kodi-goto:3` |

use super::library::Media;
use std::fmt;
use tveee_core::more::Go;

/// One of Kodi's links (see the table above).
#[derive(Debug, Clone, PartialEq)]
pub enum Link {
    /// Above the libraries: Kodi's categories.
    Root,
    /// A folder to browse in tveee.
    Browse {
        /// Which library.
        media: Media,
        /// Its name, if known (empty: a typed location).
        name: String,
        /// Its path, as Kodi knows it.
        path: String,
    },
    /// A file to play from its folder (the folder queued, see
    /// [`super::library::play`]).
    Play {
        /// The playlist to queue it in (0 music, 1 video).
        playlist: u32,
        /// Its folder (empty: just the file).
        dir: String,
        /// The file.
        file: String,
    },
    /// A window on Kodi's screen.
    Window {
        /// The window, as `GUI.ActivateWindow` names it.
        window: String,
        /// The folder to open it at (None: its own start).
        path: Option<String>,
    },
    /// The queue playing.
    Queue {
        /// Kodi's playlist.
        playlist: u32,
    },
    /// An item of the queue playing: play from there.
    GoTo {
        /// Its position, from 0.
        position: usize,
    },
}

const ROOT: &str = "kodi|";
const PLAY: &str = "kodi-play:";
const WINDOW: &str = "kodi-window:";
const QUEUE: &str = "kodi-queue:";
const GOTO: &str = "kodi-goto:";

impl fmt::Display for Link {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Link::Root => f.write_str(ROOT),
            // a name can't hold the separator
            Link::Browse { media, name, path } => write!(f, "{}|{}|{path}", media.name(), name.replace('|', "/")),
            Link::Play { playlist, dir, file } => write!(f, "{PLAY}{playlist}|{dir}|{file}"),
            Link::Window { window, path: None } => write!(f, "{WINDOW}{window}"),
            Link::Window { window, path: Some(p) } => write!(f, "{WINDOW}{window}|{p}"),
            Link::Queue { playlist } => write!(f, "{QUEUE}{playlist}"),
            Link::GoTo { position } => write!(f, "{GOTO}{position}"),
        }
    }
}

impl Link {
    /// A link's text read back (None: not one of Kodi's; a media file's
    /// own path, say).
    pub fn parse(s: &str) -> Option<Link> {
        if s == ROOT {
            return Some(Link::Root);
        }
        if let Some(rest) = s.strip_prefix(PLAY) {
            let mut parts = rest.splitn(3, '|');
            return match (parts.next().and_then(|p| p.parse().ok()), parts.next(), parts.next()) {
                (Some(playlist), Some(dir), Some(file)) => Some(Link::Play { playlist, dir: dir.to_string(), file: file.to_string() }),
                _ => None,
            };
        }
        if let Some(n) = s.strip_prefix(QUEUE) {
            return n.parse().ok().map(|playlist| Link::Queue { playlist });
        }
        if let Some(n) = s.strip_prefix(GOTO) {
            return n.parse().ok().map(|position| Link::GoTo { position });
        }
        if let Some(rest) = s.strip_prefix(WINDOW) {
            return Some(match rest.split_once('|') {
                Some((w, p)) => Link::Window { window: w.to_string(), path: Some(p.to_string()) },
                None => Link::Window { window: rest.to_string(), path: None },
            });
        }
        let mut parts = s.splitn(3, '|');
        let media = parts.next().and_then(Media::parse)?;
        match (parts.next(), parts.next()) {
            (Some(name), Some(path)) => Some(Link::Browse { media, name: name.to_string(), path: path.trim().to_string() }),
            // media|path (no name)
            (Some(path), None) => Some(Link::Browse { media, name: String::new(), path: path.trim().to_string() }),
            _ => None,
        }
    }

    /// As a `Go` for a list item, a crumb, the way up.
    pub fn go(&self) -> Go {
        Go::Link(self.to_string())
    }

    /// A folder to browse.
    pub fn browse(media: Media, name: &str, path: &str) -> Link {
        Link::Browse { media, name: name.to_string(), path: path.to_string() }
    }

    /// A window on Kodi's screen.
    pub fn window(window: &str, path: Option<&str>) -> Link {
        Link::Window { window: window.to_string(), path: path.filter(|p| !p.is_empty()).map(str::to_string) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn written_and_read_back() {
        let links = [
            Link::Root,
            Link::browse(Media::Music, "A Polished Solid", "musicdb://albums/251/"),
            Link::browse(Media::Video, "", "smb://nas/films/"),
            Link::Play { playlist: 0, dir: "musicdb://albums/251/".into(), file: "smb://nas/a|b.mp3".into() },
            Link::Play { playlist: 1, dir: String::new(), file: "x.mkv".into() },
            Link::window("music", None),
            Link::window("videos", Some("videodb://movies/titles/")),
            Link::Queue { playlist: 0 },
            Link::GoTo { position: 3 },
        ];
        for l in links {
            assert_eq!(Link::parse(&l.to_string()), Some(l.clone()), "{l}");
        }
    }

    #[test]
    fn as_written_before() {
        // the text is what it was (saved recents, the command line's list)
        assert_eq!(Link::Root.to_string(), "kodi|");
        assert_eq!(Link::browse(Media::Music, "Disc 1", "musicdb://albums/251/1/").to_string(), "music|Disc 1|musicdb://albums/251/1/");
        assert_eq!(Link::window("music", Some("smb://nas/")).to_string(), "kodi-window:music|smb://nas/");
        assert_eq!(Link::parse("video||smb://nas/films/"), Some(Link::browse(Media::Video, "", "smb://nas/films/")));
        // a name with the separator in it
        assert_eq!(Link::browse(Media::Music, "AC|DC", "p").to_string(), "music|AC/DC|p");
        // not Kodi's
        assert_eq!(Link::parse("smb://nas/a.mp3"), None);
        assert_eq!(Link::parse("kodi-play:x|a|b"), None);
        assert_eq!(Link::parse("https://youtube.com/"), None);
    }
}

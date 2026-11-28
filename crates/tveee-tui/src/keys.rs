//! Every key of the ui, in one place, by where it works.
//!
//! The main screen's keys (player and remote mode) are dispatched from
//! [`PLAYER`] and [`REMOTE`] themselves (keymap.rs), so they can't
//! say one thing and do another. The other screens' keys are handled
//! where they are (the guide's in guide.rs, the list's in ctxlist.rs, ...);
//! their [`Section`]s here say what those handlers do, and change with
//! them. Everything that shows keys is made from this: the help (`?`),
//! `tveee ui --keys`, and `tveee ui --help`'s pointer to them.

use super::*;

/// A main-screen key in player mode: the keys, as shown, what it does to
/// the ui, and in words.
pub struct PlayerKey {
    pub codes: &'static [KeyCode],
    /// As shown: at most [`LABEL`] characters.
    pub label: &'static str,
    pub input: Input,
    pub what: &'static str,
}

const fn pk(codes: &'static [KeyCode], label: &'static str, input: Input, what: &'static str) -> PlayerKey {
    PlayerKey { codes, label, input, what }
}

const fn run(id: &'static str) -> Input {
    Input::Do(Action::Run { id, args: Vec::new() })
}

/// The main screen's keys in player mode (besides ^C, ^P and tab, which
/// work in remote mode too, and the digits, [`DIGITS`]).
pub const PLAYER: &[PlayerKey] = &[
    pk(&[KeyCode::Char(' '), KeyCode::Char('p')], "space p", Input::Do(Action::PlayPause), "play / pause"),
    pk(&[KeyCode::Left], "←", Input::Do(Action::Seek(-KEY_STEP)), "seek back 10s (exact in YouTube, Kodi; ff/rw keys elsewhere)"),
    pk(&[KeyCode::Right], "→", Input::Do(Action::Seek(KEY_STEP)), "seek forward 10s"),
    pk(&[KeyCode::Char('[')], "[", Input::Do(Action::Seek(-60.0)), "seek back 60s"),
    pk(&[KeyCode::Char(']')], "]", Input::Do(Action::Seek(60.0)), "seek forward 60s"),
    pk(&[KeyCode::Up, KeyCode::Char('+'), KeyCode::Char('=')], "↑ + =", Input::Do(Action::Volume(1)), "volume up"),
    pk(&[KeyCode::Down, KeyCode::Char('-')], "↓ -", Input::Do(Action::Volume(-1)), "volume down"),
    pk(&[KeyCode::Char('m')], "m", Input::Do(Action::Mute), "mute / unmute"),
    pk(&[KeyCode::Backspace], "⌫", Input::Do(Action::Key(KEY_BACK)), "back (the TV's Back key)"),
    pk(&[KeyCode::Char('h')], "h", Input::Do(Action::Key(KEY_HOME)), "home (the TV's Home key)"),
    pk(&[KeyCode::Char('w')], "w", run("power.wake"), "wake the TV (from standby)"),
    pk(&[KeyCode::Char('S')], "S", run("power.sleep"), "sleep (standby)"),
    pk(&[KeyCode::Char('F')], "F", run("apps.free"), "free memory (force-stop idle apps)"),
    pk(&[KeyCode::Char('R')], "R", Input::RestartFront, "restart the app in front, coming back where it was (YouTube, Kodi)"),
    pk(&[KeyCode::Char('r')], "r", Input::Do(Action::Refresh), "refresh"),
    pk(&[KeyCode::Char('.')], ".", Input::OpenMenu, "the menu: power, inputs, mirror, ... (each with a letter of its own)"),
    pk(&[KeyCode::Char(':')], ":", Input::OpenPalette, "commands: everything tveee can do, found by typing a few letters (^P too)"),
    pk(&[KeyCode::Char('?')], "?", Input::ToggleHelp, "all keys (this)"),
    pk(&[KeyCode::Char('a')], "a", Input::OpenApps, "launch an app (the launcher)"),
    pk(&[KeyCode::Char('g')], "g", Input::OpenGuide, "the live TV guide"),
    pk(&[KeyCode::Char('t')], "t", Input::OpenList, "the app's list: Kodi's library, YouTube's chapters and links"),
    pk(&[KeyCode::Char('/')], "/", Input::OpenSearch, "search the app in front"),
    pk(&[KeyCode::Char(',')], ",", Input::OpenOptions, "options"),
    pk(&[KeyCode::Char('z')], "z", Input::ToggleVisualiser, "music visualiser on/off (Kodi, while it plays music)"),
    pk(&[KeyCode::Char('v')], "v", Input::TogglePreview, "preview on/off: a picture of what's on"),
    pk(&[KeyCode::Char('i')], "i", Input::ToggleInfo, "system info on/off"),
    pk(&[KeyCode::Char('I')], "I", Input::PauseInfo, "pause the system panel's figures (to look at them)"),
    pk(&[KeyCode::Char('l')], "l", Input::ToggleAmbilight, "Ambilight panel on/off (Philips TVs with one)"),
    pk(&[KeyCode::Char('L')], "L", Input::AmbilightDialog, "change the Ambilight (⏎ on a light: a colour picker)"),
    pk(&[KeyCode::Char('P')], "P", Input::Pair, "pair with the TV's API (asked for by the Ambilight panel)"),
    pk(&[KeyCode::Char('u')], "u", Input::ToggleUrl, "the link whole / shortened"),
    pk(&[KeyCode::Char('y')], "y", Input::CopyUrl, "copy the link (clicking it does too; ^click opens it in most terminals)"),
    pk(&[KeyCode::Char('q'), KeyCode::Esc], "q esc", Input::Quit, "quit"),
];

/// The digits in player mode: they jump through what's playing.
pub const DIGITS: Bind = Bind { keys: "0-9", what: "jump to 0%, 10%, ... 90%" };

/// The main screen's letters (and other characters) in player mode, each
/// with the action it runs if it runs one ("power.wake"), and what it does:
/// for checking them against the menu's letters.
pub fn player_chars() -> Vec<(char, Option<&'static str>, &'static str)> {
    let mut out: Vec<_> = PLAYER
        .iter()
        .flat_map(|k| {
            let id = match &k.input {
                Input::Do(Action::Run { id, .. }) => Some(*id),
                _ => None,
            };
            k.codes.iter().filter_map(move |c| if let KeyCode::Char(ch) = c { Some((*ch, id, k.what)) } else { None })
        })
        .collect();
    out.extend(('0'..='9').map(|d| (d, None, DIGITS.what)));
    out
}

/// A key in remote mode and the TV key it sends.
pub struct RemoteKey {
    pub code: KeyCode,
    pub label: &'static str,
    pub key: u32,
    pub what: &'static str,
}

const fn rk(code: KeyCode, label: &'static str, key: u32, what: &'static str) -> RemoteKey {
    RemoteKey { code, label, key, what }
}

/// Remote mode: the keyboard is the TV's (besides tab, ^C and ^P). What
/// else is typed goes into the TV as text ([`TYPING`]).
pub const REMOTE: &[RemoteKey] = &[
    rk(KeyCode::Up, "↑", 19, "up (the d-pad)"),
    rk(KeyCode::Down, "↓", 20, "down"),
    rk(KeyCode::Left, "←", 21, "left"),
    rk(KeyCode::Right, "→", 22, "right"),
    rk(KeyCode::Enter, "⏎", KEY_OK, "OK"),
    rk(KeyCode::Esc, "esc", KEY_BACK, "back"),
    rk(KeyCode::Backspace, "⌫", KEY_DEL, "delete backwards in a text box"),
    rk(KeyCode::Delete, "del", KEY_FORWARD_DEL, "delete forwards"),
    rk(KeyCode::Home, "home", KEY_HOME, "the TV's home screen"),
    rk(KeyCode::PageUp, "pgup", KEY_PAGE_UP, "page up"),
    rk(KeyCode::PageDown, "pgdn", KEY_PAGE_DOWN, "page down"),
];

/// Remote mode's other keys: typed into the TV.
pub const TYPING: Bind = Bind { keys: "typing", what: "typed into the TV (a search box, say); pasting too" };

/// A key and what it does, as shown.
pub struct Bind {
    /// At most [`LABEL`] characters.
    pub keys: &'static str,
    pub what: &'static str,
}

const fn b(keys: &'static str, what: &'static str) -> Bind {
    Bind { keys, what }
}

/// The longest a key's label can be (the help's first column).
pub const LABEL: usize = 7;

/// Where some keys work: its name, how you get there, and the keys.
pub struct Section {
    pub name: &'static str,
    pub how: &'static str,
    pub keys: &'static [Bind],
}

/// Keys that work wherever you are.
pub const EVERYWHERE: Section = Section {
    name: "everywhere",
    how: "",
    keys: &[
        b("^P", "commands (over whatever is open; back to it after)"),
        b(":", "commands, where ':' isn't being typed (a filter, a search, a box)"),
        b("F1", "the guide (alt+1 too), from anywhere you aren't typing"),
        b("F2", "the apps (alt+2 too)"),
        b("F3", "remote mode (alt+3 too)"),
        b("tab", "player mode / remote mode (on the main screen)"),
        b("^C", "quit (the guide and the list: close them)"),
    ],
};

/// The other screens, each opened from the main screen (or another).
/// What their handlers do: change them with the handlers.
pub const SCREENS: &[Section] = &[
    Section {
        name: "the menu",
        how: ".",
        keys: &[
            b("↑ ↓", "choose (pgup pgdn home end too)"),
            b("⏎", "do it"),
            b("letter", "the item with that letter (shown beside it; not the main screen's)"),
            b("esc . q", "close (⌫ too)"),
        ],
    },
    Section {
        name: "commands",
        how: ": or ^P",
        keys: &[
            b("typing", "find (titles, ids, command names, words they're about); a link: open it"),
            b("↑ ↓", "choose (pgup pgdn home end too)"),
            b("⏎", "do it (or ask for what it needs, or ask first)"),
            b("⌫ ^U", "delete a letter, clear"),
            b("esc", "clear what's typed, then close (back to what it was over)"),
        ],
    },
    Section {
        name: "a box asking for text",
        how: "an action that needs it",
        keys: &[
            b("typing", "the text (⌫ deletes, ^U clears)"),
            b("⏎", "do it with that"),
            b("← →", "in the volume box: volume now; in the seek box: seek now"),
            b("esc", "cancel"),
        ],
    },
    Section {
        name: "choices",
        how: "an action with a list: switch input, subtitles, ...",
        keys: &[b("↑ ↓", "choose (pgup pgdn home end too)"), b("⏎", "do it"), b("esc", "close")],
    },
    Section {
        name: "are you sure",
        how: "reboot, power off, uninstall, ...",
        keys: &[b("y", "yes"), b("← → tab", "between yes and no"), b("⏎", "the one chosen"), b("other", "no")],
    },
    Section { name: "help", how: "?", keys: &[b("↑ ↓", "scroll (pgup pgdn home end too)"), b("other", "close")] },
    Section {
        name: "the apps",
        how: "a or F2",
        keys: &[
            b("↑ ↓", "choose (pgup pgdn home end too)"),
            b("⏎", "start it (running: to the front)"),
            b("letter", "the next app starting with it"),
            b("space", "its actions: front, restart, close, search"),
            b("/", "search in it"),
            b("#", "package names shown or not (remembered)"),
            b("tab", "the other tab (known apps, others)"),
            b("esc ⌫", "close"),
        ],
    },
    Section {
        name: "an app's actions", how: "space in the apps", keys: &[b("↑ ↓ tab", "choose"), b("⏎ space", "do it"), b("other", "back to the apps")]
    },
    Section {
        name: "search",
        how: "/",
        keys: &[
            b("typing", "what to search for (⌫ deletes, ^U clears)"),
            b("tab ↓", "the next app to search in (shift+tab ↑ the one before)"),
            b("⏎", "search"),
            b("esc", "cancel"),
        ],
    },
    Section {
        name: "the guide",
        how: "g or F1",
        keys: &[
            b("typing", "filter the channels (⌫ deletes, ^U clears)"),
            b("↑ ↓", "channels (pgup pgdn home end too)"),
            b("← →", "earlier, later"),
            b("⏎", "watch it (clicking the channel too)"),
            b("*", "a favourite or not (clicking the star too)"),
            b("tab", "the next tab: all, favourites, radio"),
            b("S-tab", "into the programme's details"),
            b("^P", "close the guide, open the commands"),
            b("esc", "clear the filter, then close (^C closes)"),
        ],
    },
    Section {
        name: "a programme's details",
        how: "shift+tab in the guide",
        keys: &[b("↑ ↓", "scroll (pgup pgdn home end too)"), b("esc tab", "back to the channels (the details stay; S-tab too)"), b("⌫ h", "close the details")],
    },
    Section {
        name: "the app's list",
        how: "t",
        keys: &[
            b("typing", "filter it (^U clears)"),
            b("↑ ↓", "choose (pgup pgdn home end too)"),
            b("⏎", "open the folder, play the item"),
            b("⌫ ←", "up a level (with nothing typed; else ⌫ deletes)"),
            b("alt+↑", "up a level"),
            b("tab", "the breadcrumbs"),
            b("^L", "type a location (smb://..., /storage/..., musicdb://...)"),
            b("^S", "open the folders on the TV too (Kodi's library, browsed here)"),
            b("esc", "clear the filter, then close (^C closes)"),
        ],
    },
    Section {
        name: "the breadcrumbs",
        how: "tab in the list",
        keys: &[b("← →", "along them (home end: first, last)"), b("⏎", "go there"), b("tab ↓", "back to the list (esc too)")],
    },
    Section {
        name: "a location",
        how: "^L in the list",
        keys: &[b("typing", "where (⌫ deletes, ^U clears)"), b("⏎", "open it"), b("esc", "back to the list")],
    },
    Section {
        name: "the log",
        how: "commands: ui: show the log",
        keys: &[
            b("↑ ↓", "a line (pgup pgdn too; shift: select)"),
            b("home", "the oldest"),
            b("end", "follow the newest"),
            b("v", "start or end a selection"),
            b("y ⏎", "copy the line or the selection"),
            b("s", "save the log to a file"),
            b("esc", "let go of the selection, then close (q closes)"),
        ],
    },
    Section {
        name: "saving the log",
        how: "s in the log",
        keys: &[b("typing", "the file's name (⌫ deletes, ^U clears)"), b("⏎", "save (again to write over a file)"), b("esc", "back to the log")],
    },
    Section {
        name: "options",
        how: ",",
        keys: &[
            b("↑ ↓", "choose (↑ from the first: the tabs)"),
            b("← →", "off / on (on the tabs: the other tab)"),
            b("⏎ space", "flip it"),
            b("a", "a plugin: back to checking the TV"),
            b("tab", "the other tab (options, plugins)"),
            b("esc , q", "close (⌫ too; esc on the tabs: back to the rows)"),
        ],
    },
    Section {
        name: "the Ambilight",
        how: "L (Philips TVs with one)",
        keys: &[
            b("↑ ↓", "power, style, the lights, ..."),
            b("← →", "change it now (on the lights: which one)"),
            b("⏎ space", "choose (on a light: the colour picker)"),
            b("esc L q", "close"),
        ],
    },
    Section {
        name: "a light's colour",
        how: "⏎ on a light in the Ambilight",
        keys: &[
            b("arrows", "around the colours"),
            b("typing", "a colour's name or #hex (⌫ deletes, ^U clears)"),
            b("⏎", "set it (what's typed, else the one picked); stays open"),
            b("tab", "the next light (shift+tab the one before)"),
            b("esc", "back to the Ambilight"),
        ],
    },
    Section { name: "pairing", how: "P", keys: &[b("0-9", "the PIN the TV shows (⌫ deletes)"), b("⏎", "pair"), b("esc", "cancel")] },
    Section {
        name: "mouse",
        how: "",
        keys: &[
            b("click", "the buttons, the progress bar (jump), the volume bar (set it)"),
            b("wheel", "volume (in lists: scroll)"),
            b("S-drag", "select text as usual (to copy the link)"),
        ],
    },
];

/// A heading and its rows, as the help shows them.
fn section_lines(out: &mut Vec<String>, name: &str, how: &str, rows: impl IntoIterator<Item = (&'static str, &'static str)>) {
    if !out.is_empty() {
        out.push(String::new());
    }
    out.push(if how.is_empty() { format!(" {name}") } else { format!(" {name}  ({how})") });
    for (keys, what) in rows {
        out.push(format!("   {keys:<LABEL$}  {what}"));
    }
}

/// Every key, by where it works: the main screen's first.
pub fn lines() -> Vec<String> {
    let mut out = Vec::new();
    section_lines(&mut out, EVERYWHERE.name, EVERYWHERE.how, EVERYWHERE.keys.iter().map(|k| (k.keys, k.what)));
    let player = PLAYER.iter().map(|k| (k.label, k.what)).chain(std::iter::once((DIGITS.keys, DIGITS.what)));
    section_lines(&mut out, "player mode", "the main screen", player);
    let remote = REMOTE.iter().map(|k| (k.label, k.what)).chain(std::iter::once((TYPING.keys, TYPING.what)));
    section_lines(&mut out, "remote mode", "tab or F3: the keyboard is the TV's", remote);
    for s in SCREENS {
        section_lines(&mut out, s.name, s.how, s.keys.iter().map(|k| (k.keys, k.what)));
    }
    out
}

/// [`lines`] as one text (made once).
pub fn help() -> &'static str {
    static TEXT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    TEXT.get_or_init(|| lines().join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels() -> Vec<(&'static str, &'static str)> {
        let mut v: Vec<(&str, &str)> = EVERYWHERE.keys.iter().map(|k| ("everywhere", k.keys)).collect();
        v.extend(PLAYER.iter().map(|k| ("player", k.label)));
        v.extend(REMOTE.iter().map(|k| ("remote", k.label)));
        for s in SCREENS {
            v.extend(s.keys.iter().map(|k| (s.name, k.keys)));
        }
        v
    }

    #[test]
    fn labels_fit_the_column() {
        for (screen, label) in labels() {
            assert!(label.chars().count() <= LABEL, "{screen}: '{label}' is longer than {LABEL}");
        }
    }

    #[test]
    fn no_key_twice_on_a_screen() {
        let mut seen = std::collections::HashSet::new();
        for k in PLAYER {
            for c in k.codes {
                assert!(seen.insert(*c), "player mode: {c:?} is bound twice");
            }
        }
        // the keys every mode keeps for itself
        for c in [KeyCode::Tab, KeyCode::Char('0'), KeyCode::Char('9')] {
            assert!(!seen.contains(&c), "player mode: {c:?} is taken already");
        }
        let mut seen = std::collections::HashSet::new();
        for k in REMOTE {
            assert!(seen.insert(k.code), "remote mode: {:?} is bound twice", k.code);
        }
        // as shown, within each screen
        for (screen, keys) in std::iter::once((EVERYWHERE.name, EVERYWHERE.keys)).chain(SCREENS.iter().map(|s| (s.name, s.keys))) {
            let mut seen = std::collections::HashSet::new();
            for k in keys {
                for word in k.keys.split(' ').filter(|w| !matches!(*w, "typing" | "letter" | "other")) {
                    assert!(seen.insert(word), "{screen}: '{word}' is listed twice");
                }
            }
        }
    }

    #[test]
    fn dispatched_as_listed() {
        // every player key does what its row says
        for k in PLAYER {
            for c in k.codes {
                assert_eq!(input_for(*c, KeyModifiers::NONE, Mode::Player), k.input, "{c:?}");
            }
        }
        for k in REMOTE {
            assert_eq!(remote_input(k.code, KeyModifiers::NONE), Input::Do(Action::Key(k.key)), "{:?}", k.code);
        }
        // the digits jump
        assert_eq!(input_for(KeyCode::Char('5'), KeyModifiers::NONE, Mode::Player), Input::Do(Action::Jump(0.5)));
    }

    #[test]
    fn the_help_has_every_screen() {
        let h = help();
        for name in std::iter::once("player mode").chain(std::iter::once("remote mode")).chain(SCREENS.iter().map(|s| s.name)) {
            assert!(h.lines().any(|l| l.starts_with(&format!(" {name}"))), "no heading for {name}");
        }
        assert!(h.contains("   space p  play / pause"));
    }
}

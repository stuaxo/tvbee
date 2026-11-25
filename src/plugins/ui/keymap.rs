//! Keys: what a key press means to the UI, and the TV keycodes it sends.

use super::*;

pub const KEY_BACK: u32 = 4;
pub const KEY_HOME: u32 = 3;
pub(super) const DPAD: [(KeyCode, u32, &str); 4] =
    [(KeyCode::Up, 19, "up"), (KeyCode::Down, 20, "down"), (KeyCode::Left, 21, "left"), (KeyCode::Right, 22, "right")];
pub const KEY_OK: u32 = 23;
/// Delete (backspace), forward delete, page up / down.
pub const KEY_DEL: u32 = 67;
pub const KEY_FORWARD_DEL: u32 = 112;
pub const KEY_PAGE_UP: u32 = 92;
pub const KEY_PAGE_DOWN: u32 = 93;

/// What the arrow keys do: seek/volume, or move around the TV's menus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Player,
    Remote,
}

/// What a key press means to the UI.
#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    Quit,
    ToggleMode,
    ToggleHelp,
    ToggleInfo,
    /// The Ambilight panel (TVs that have one).
    ToggleAmbilight,
    /// Pair with the TV's API (from the Ambilight panel).
    Pair,
    /// The dialog for changing the Ambilight.
    AmbilightDialog,
    /// The preview panel: a picture of what's on.
    TogglePreview,
    OpenMenu,
    OpenApps,
    OpenSearch,
    OpenGuide,
    OpenOptions,
    Do(Action),
    Nothing,
}

pub fn key_name(code: u32) -> &'static str {
    match code {
        KEY_BACK => "back",
        KEY_HOME => "home",
        KEY_OK => "ok",
        KEY_DEL => "delete",
        KEY_FORWARD_DEL => "forward delete",
        KEY_PAGE_UP => "page up",
        KEY_PAGE_DOWN => "page down",
        c => DPAD.iter().find(|(_, k, _)| *k == c).map_or("key", |(_, _, n)| n),
    }
}

/// Map a key to what it means in `mode`.
pub fn input_for(code: KeyCode, mods: KeyModifiers, mode: Mode) -> Input {
    use Action::*;
    let act = |a| Input::Do(a);
    match (code, mode) {
        (KeyCode::Char('c'), _) if mods.contains(KeyModifiers::CONTROL) => Input::Quit,
        (KeyCode::Tab, _) => Input::ToggleMode,
        // remote mode: the keyboard is the TV's (Tab leaves, Ctrl-C quits)
        (_, Mode::Remote) => remote_input(code, mods),
        (KeyCode::Char('q') | KeyCode::Esc, _) => Input::Quit,
        (KeyCode::Char('?'), _) => Input::ToggleHelp,
        (KeyCode::Char('.'), _) => Input::OpenMenu,
        (KeyCode::Char('i'), _) => Input::ToggleInfo,
        (KeyCode::Char('l'), _) => Input::ToggleAmbilight,
        (KeyCode::Char('P'), _) => Input::Pair,
        (KeyCode::Char('L'), _) => Input::AmbilightDialog,
        (KeyCode::Char('v'), _) => Input::TogglePreview,
        (KeyCode::Char('a'), _) => Input::OpenApps,
        (KeyCode::Char('g'), _) => Input::OpenGuide,
        (KeyCode::Char(','), _) => Input::OpenOptions,
        (KeyCode::Char('/'), _) => Input::OpenSearch,
        (KeyCode::Char(' ') | KeyCode::Char('p'), _) => act(PlayPause),
        (KeyCode::Backspace, _) => act(Key(KEY_BACK)),
        (KeyCode::Char('h'), _) => act(Key(KEY_HOME)),
        (KeyCode::Char('['), _) => act(Seek(-60.0)),
        (KeyCode::Char(']'), _) => act(Seek(60.0)),
        (KeyCode::Char(d @ '0'..='9'), _) => act(Jump(f64::from(d.to_digit(10).unwrap_or(0)) / 10.0)),
        (KeyCode::Char('+') | KeyCode::Char('='), _) => act(Volume(1)),
        (KeyCode::Char('-'), _) => act(Volume(-1)),
        (KeyCode::Char('m'), _) => act(Mute),
        (KeyCode::Char('r'), _) => act(Refresh),
        // player mode: arrows seek and change volume
        (KeyCode::Left, _) => act(Seek(-10.0)),
        (KeyCode::Right, _) => act(Seek(10.0)),
        (KeyCode::Up, _) => act(Volume(1)),
        (KeyCode::Down, _) => act(Volume(-1)),
        _ => Input::Nothing,
    }
}

/// A key in remote mode: arrows are the d-pad, Enter is OK, Esc is back,
/// and what you type is typed into the TV.
pub(super) fn remote_input(code: KeyCode, mods: KeyModifiers) -> Input {
    let key = |k| Input::Do(Action::Key(k));
    match code {
        KeyCode::Enter => key(KEY_OK),
        KeyCode::Esc => key(KEY_BACK),
        KeyCode::Backspace => key(KEY_DEL),
        KeyCode::Delete => key(KEY_FORWARD_DEL),
        KeyCode::Home => key(KEY_HOME),
        KeyCode::PageUp => key(KEY_PAGE_UP),
        KeyCode::PageDown => key(KEY_PAGE_DOWN),
        // Android's `input text` takes plain ASCII
        KeyCode::Char(c) if !mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) && c.is_ascii() && !c.is_ascii_control() => {
            Input::Do(Action::Text(c.to_string()))
        }
        c => DPAD.iter().find(|(k, _, _)| *k == c).map_or(Input::Nothing, |(_, code, _)| key(*code)),
    }
}

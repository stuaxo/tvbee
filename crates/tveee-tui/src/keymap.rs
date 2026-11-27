//! Keys: what a key press means to the UI, and the TV keycodes it sends.

use super::*;

pub(super) const DPAD: [(KeyCode, u32, &str); 4] =
    [(KeyCode::Up, 19, "up"), (KeyCode::Down, 20, "down"), (KeyCode::Left, 21, "left"), (KeyCode::Right, 22, "right")];

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
    /// The command palette: every action, found by typing.
    OpenPalette,
    /// The log: what tveee did lately.
    ShowLogs,
    /// Restart the app in front, coming back where it was if it can.
    RestartFront,
    /// What's on's link: whole, or shortened to fit.
    ToggleUrl,
    /// Copy what's on's link to the clipboard.
    CopyUrl,
    /// Open what's on's link in this computer's browser.
    OpenUrl,
    OpenApps,
    OpenSearch,
    OpenGuide,
    /// The music visualiser (Kodi, while music plays): show or hide it.
    ToggleVisualiser,
    OpenOptions,
    Do(Action),
    Nothing,
}



/// The views: the guide, the launcher, remote mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Guide,
    Apps,
    Remote,
}

/// A key that switches view from anywhere it isn't typing: F1-F3, or
/// Alt+1-3 for terminals that keep the F keys. (Digits alone keep jumping
/// through the video, and filter lists.)
pub fn view_key(code: KeyCode, mods: KeyModifiers) -> Option<View> {
    match (code, mods.contains(KeyModifiers::ALT)) {
        (KeyCode::F(1), _) | (KeyCode::Char('1'), true) => Some(View::Guide),
        (KeyCode::F(2), _) | (KeyCode::Char('2'), true) => Some(View::Apps),
        (KeyCode::F(3), _) | (KeyCode::Char('3'), true) => Some(View::Remote),
        _ => None,
    }
}

/// Map a key to what it means in `mode`.
pub fn input_for(code: KeyCode, mods: KeyModifiers, mode: Mode) -> Input {
    use Action::*;
    let act = |a| Input::Do(a);
    match (code, mode) {
        (KeyCode::Char('c'), _) if mods.contains(KeyModifiers::CONTROL) => Input::Quit,
        // the palette from either mode (Ctrl-P types nothing into the TV)
        (KeyCode::Char('p'), _) if mods.contains(KeyModifiers::CONTROL) => Input::OpenPalette,
        (KeyCode::Tab, _) => Input::ToggleMode,
        // remote mode: the keyboard is the TV's (Tab leaves, Ctrl-C quits)
        (_, Mode::Remote) => remote_input(code, mods),
        (KeyCode::Char('q') | KeyCode::Esc, _) => Input::Quit,
        (KeyCode::Char('?'), _) => Input::ToggleHelp,
        (KeyCode::Char('.'), _) => Input::OpenMenu,
        (KeyCode::Char(':'), _) => Input::OpenPalette,
        (KeyCode::Char('F'), _) => act(Run { id: "apps.free", args: Vec::new() }),
        (KeyCode::Char('S'), _) => act(Run { id: "power.sleep", args: Vec::new() }),
        (KeyCode::Char('R'), _) => Input::RestartFront,
        (KeyCode::Char('u'), _) => Input::ToggleUrl,
        (KeyCode::Char('y'), _) => Input::CopyUrl,
        (KeyCode::Char('i'), _) => Input::ToggleInfo,
        (KeyCode::Char('l'), _) => Input::ToggleAmbilight,
        (KeyCode::Char('P'), _) => Input::Pair,
        (KeyCode::Char('L'), _) => Input::AmbilightDialog,
        (KeyCode::Char('v'), _) => Input::TogglePreview,
        (KeyCode::Char('a'), _) => Input::OpenApps,
        (KeyCode::Char('g'), _) => Input::OpenGuide,
        (KeyCode::Char('z'), _) => Input::ToggleVisualiser,
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

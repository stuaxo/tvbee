//! Keys: what a key press means to the UI, and the TV keycodes it sends.

use super::*;

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
    /// Pause (or resume) the system panel's figures.
    PauseInfo,
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
    /// The list the app in front shows (Kodi's folder).
    OpenList,
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
///
/// Its keys are listed in keys.rs, EVERYWHERE (F1-F3): they're handled here, not
/// from that table, so change the rows with this.
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
        // the digits jump through what's playing
        (KeyCode::Char(d @ '0'..='9'), _) => act(Jump(f64::from(d.to_digit(10).unwrap_or(0)) / 10.0)),
        // the rest: as the key table has them
        _ => super::keys::PLAYER.iter().find(|k| k.codes.contains(&code)).map_or(Input::Nothing, |k| k.input.clone()),
    }
}

/// A key in remote mode: arrows are the d-pad, Enter is OK, Esc is back,
/// and what you type is typed into the TV.
pub(super) fn remote_input(code: KeyCode, mods: KeyModifiers) -> Input {
    if let Some(k) = super::keys::REMOTE.iter().find(|k| k.code == code) {
        return Input::Do(Action::Key(k.key));
    }
    match code {
        // Android's `input text` takes plain ASCII
        KeyCode::Char(c) if !mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) && c.is_ascii() && !c.is_ascii_control() => {
            Input::Do(Action::Text(c.to_string()))
        }
        _ => Input::Nothing,
    }
}

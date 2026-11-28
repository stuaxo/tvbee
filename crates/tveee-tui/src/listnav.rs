//! Moving a highlight through a list, the same way in every list (the
//! palette, the menu, the apps, options, the guide, ...): ↑/↓ one (round
//! from the end to the start), PgUp/PgDn a page (stopping at the ends),
//! Home/End the first and last.
//!
//! The keys on the ui's widgets, the same everywhere:
//!
//! * ↑/↓ (PgUp/PgDn, Home/End) move between rows.
//! * ←/→ change the row's value (a choice of two, a tick: ← off, → on; a
//!   light's mode), so trying values is one key each. They don't move
//!   elsewhere; on a row without a value they do nothing (in a list of
//!   places, ← goes up a level, as in a file manager).
//! * Tab / Shift+Tab move between tabs (or a pane's parts). ←/→ switch tabs
//!   only on the tab bar itself (↑ from the first row; ↓ back).
//! * Space or ⏎ toggles or chooses; Esc goes back one step (out of the tab
//!   bar, a picker, then the pop-up).
//! * Changing something leaves the widget open, to try another value.

use ratatui::crossterm::event::KeyCode;

/// How far PgUp/PgDn move.
pub const PAGE: usize = 10;

/// Whether `code` moves the highlight.
pub fn is_move(code: KeyCode) -> bool {
    matches!(code, KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown | KeyCode::Home | KeyCode::End)
}

/// Where a movement key takes the highlight from `i` in a list of `n`;
/// None for a key that isn't one (or an empty list).
pub fn step(i: usize, n: usize, code: KeyCode) -> Option<usize> {
    if n == 0 {
        return None;
    }
    let i = i.min(n - 1);
    Some(match code {
        KeyCode::Up => i.checked_sub(1).unwrap_or(n - 1),
        KeyCode::Down => (i + 1) % n,
        KeyCode::PageUp => i.saturating_sub(PAGE),
        KeyCode::PageDown => (i + PAGE).min(n - 1),
        KeyCode::Home => 0,
        KeyCode::End => n - 1,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moves() {
        assert_eq!(step(0, 5, KeyCode::Up), Some(4));
        assert_eq!(step(4, 5, KeyCode::Down), Some(0));
        assert_eq!(step(3, 50, KeyCode::PageUp), Some(0));
        assert_eq!(step(3, 50, KeyCode::PageDown), Some(13));
        assert_eq!(step(45, 50, KeyCode::PageDown), Some(49));
        assert_eq!(step(20, 50, KeyCode::Home), Some(0));
        assert_eq!(step(20, 50, KeyCode::End), Some(49));
        // past the end (the list shrank): from its last
        assert_eq!(step(9, 3, KeyCode::Up), Some(1));
        assert_eq!(step(0, 0, KeyCode::Down), None);
        assert_eq!(step(0, 5, KeyCode::Enter), None);
    }
}

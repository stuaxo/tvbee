//! The preview panel: a lane that makes pictures of what's on, and drawing them.

use super::*;

/// Whether to ask the terminal how it draws pictures: `TVEEE_PICTURES=detect`
/// (or `halfblocks` never), else when it's one known to draw them.
pub fn picture_terminal(env: impl Fn(&str) -> Option<String>) -> bool {
    match env("TVEEE_PICTURES").as_deref() {
        Some("detect") => return true,
        Some("halfblocks") => return false,
        _ => {}
    }
    let term = env("TERM").unwrap_or_default();
    let program = env("TERM_PROGRAM").unwrap_or_default();
    env("KITTY_WINDOW_ID").is_some()
        || env("WEZTERM_EXECUTABLE").is_some()
        || env("KONSOLE_VERSION").is_some()
        || ["kitty", "foot", "mlterm", "wezterm"].iter().any(|t| term.contains(t))
        || ["iTerm.app", "WezTerm", "ghostty", "rio"].contains(&program.as_str())
}

impl App {
    /// Rows the system panel wants, borders included.
    /// The preview panel: the picture, or what it's waiting for.
    pub(super) fn draw_preview(&mut self, f: &mut Frame, area: ratatui::layout::Rect) {
        let source = if self.preview_source.is_empty() { String::new() } else { format!(" ({})", self.preview_source) };
        let block = Block::bordered().title(format!(" preview{source} - v hides ")).border_style(Style::new().fg(Color::DarkGray));
        let inner = block.inner(area);
        f.render_widget(block, area);
        let note_h = u16::from(!self.preview_note.is_empty());
        let [pic, note] = Layout::vertical([Constraint::Min(1), Constraint::Length(note_h)]).areas(inner);
        match self.preview.as_mut() {
            Some(proto) => f.render_stateful_widget(ratatui_image::StatefulImage::new(), pic, proto),
            None => f.render_widget(Paragraph::new("getting a picture...").italic(), pic),
        }
        if note_h > 0 {
            f.render_widget(Paragraph::new(self.preview_note.clone()).fg(Color::DarkGray), note);
        }
    }
}

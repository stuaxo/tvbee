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

/// The preview panel: open or not, its picture (ready to draw), where it
/// came from, and what it's doing.
#[derive(Default)]
pub struct PreviewPane {
    pub open: bool,
    pub image: Option<ratatui_image::protocol::StatefulProtocol>,
    pub source: &'static str,
    pub note: String,
}

impl App {
    /// Rows the system panel wants, borders included.
    /// What's playing's audio and subtitle tracks, in a line ("Audio: AAC •
    /// Subtitles: English"); None when it doesn't say.
    pub(super) fn track_line(&self) -> Option<String> {
        let a = self.st.snap.as_ref()?.activity.as_ref()?;
        let parts: Vec<String> = ["audio", "subtitles"]
            .iter()
            .filter_map(|k| a.fields.iter().find(|f| f.label == *k).map(|f| format!("{}{}: {}", k[..1].to_uppercase(), &k[1..], f.value)))
            .collect();
        (!parts.is_empty()).then(|| parts.join(" • "))
    }

    /// The preview panel: the picture, or what it's waiting for.
    pub(super) fn draw_preview(&mut self, f: &mut Frame, area: ratatui::layout::Rect) {
        let source = if self.preview.source.is_empty() { String::new() } else { format!(" ({})", self.preview.source) };
        let block = Block::bordered().title(format!(" preview{source} - v hides ")).border_style(Style::new().fg(Color::DarkGray));
        let inner = block.inner(area);
        f.render_widget(block, area);
        // remote mode (no now-playing card): what's playing's tracks under it
        let tracks = if self.mode == Mode::Remote { self.track_line() } else { None };
        let tracks_h = u16::from(tracks.is_some());
        let note_h = u16::from(!self.preview.note.is_empty());
        let [pic, track, note] = Layout::vertical([Constraint::Min(1), Constraint::Length(tracks_h), Constraint::Length(note_h)]).areas(inner);
        if let Some(t) = tracks {
            f.render_widget(Paragraph::new(t), track);
        }
        match self.preview.image.as_mut() {
            Some(proto) => f.render_stateful_widget(ratatui_image::StatefulImage::new(), pic, proto),
            None => f.render_widget(Paragraph::new("getting a picture...").italic(), pic),
        }
        if note_h > 0 {
            f.render_widget(Paragraph::new(self.preview.note.clone()).fg(Color::DarkGray), note);
        }
    }
}

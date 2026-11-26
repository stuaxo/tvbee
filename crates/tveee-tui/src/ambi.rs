//! The panel (`tveee_core::panel`: the Ambilight, on TVs with one) and its dialog.

use super::*;
use tveee_core::panel::{Row, Run, View};

/// A row whose options each ask for something (the lights: a colour):
/// moving along it picks nothing.
fn asks(r: &Row) -> bool {
    !r.options.is_empty() && r.options.iter().all(|o| matches!(o.run, Run::Ask { .. }))
}

impl App {
    /// The panel `l` shows: the first this TV has.
    pub(super) fn panel_id(&self) -> Option<&'static str> {
        self.st.panels.first().copied()
    }

    /// Its heading ("ambilight").
    pub(super) fn panel_title(&self) -> &'static str {
        self.panel_id().and_then(tveee_core::panel::by_id).map_or("panel", |p| p.title)
    }

    /// What it shows, as of the last refresh (None until read).
    pub(super) fn panel_view(&self) -> Option<&View> {
        let id = self.panel_id()?;
        self.st.snap.as_ref()?.panels.iter().find(|(p, _)| *p == id).map(|(_, v)| v)
    }

    /// The dialog's rows, from the last reading (none until there is one).
    pub(super) fn ambi_rows(&self) -> Vec<Row> {
        match self.panel_view() {
            Some(View::Shown(s)) => s.rows.clone(),
            _ => Vec::new(),
        }
    }

    /// Pick option `col` of row `row` in the dialog: run it, or ask for
    /// what it needs (a light: its colour).
    pub(super) fn ambi_pick(&mut self, row: usize, col: usize, actions: &Handle) {
        let rows = self.ambi_rows();
        let Some(choice) = rows.get(row).and_then(|r| r.options.get(col)) else { return };
        match &choice.run {
            Run::Ask { .. } => {
                self.prompt.clear();
                self.overlay = Overlay::AmbiColour { row, col };
            }
            Run::Act { id, args } => {
                self.overlay = Overlay::Ambilight { row, col };
                self.note = format!("{}: {}...", self.panel_title(), choice.label);
                let _ = actions.dispatch(id, args.clone());
            }
        }
    }

    /// Keys in the dialog: ↑/↓ rows, ←/→ change at once, ⏎ picks.
    pub(super) fn ambi_key(&mut self, row: usize, col: usize, code: KeyCode, actions: &Handle) {
        let rows = self.ambi_rows();
        if matches!(code, KeyCode::Esc | KeyCode::Char('L') | KeyCode::Char('q')) || rows.is_empty() {
            if !rows.is_empty() || code == KeyCode::Esc {
                self.overlay = Overlay::None;
            }
            return;
        }
        let row = row.min(rows.len() - 1);
        let n = rows[row].options.len().max(1);
        let to_row = |r: usize| Overlay::Ambilight { row: r, col: rows[r].current.unwrap_or(0) };
        match code {
            KeyCode::Up => self.overlay = to_row(row.saturating_sub(1)),
            KeyCode::Down => self.overlay = to_row((row + 1).min(rows.len() - 1)),
            KeyCode::Left | KeyCode::Right => {
                let c = if code == KeyCode::Left { col.min(n - 1).saturating_sub(1) } else { (col + 1).min(n - 1) };
                if asks(&rows[row]) {
                    self.overlay = Overlay::Ambilight { row, col: c };
                } else if c != col.min(n - 1) || rows[row].current != Some(c) {
                    self.ambi_pick(row, c, actions);
                }
            }
            KeyCode::Enter | KeyCode::Char(' ') => self.ambi_pick(row, col.min(n - 1), actions),
            _ => {}
        }
    }

    /// Keys while typing what an option asked for (a light's colour): ⏎ runs
    /// it, Esc goes back to the dialog.
    pub(super) fn ambi_ask_key(&mut self, row: usize, col: usize, code: KeyCode, mods: KeyModifiers, actions: &Handle) {
        match code {
            KeyCode::Esc => self.overlay = Overlay::Ambilight { row, col },
            KeyCode::Enter if !self.prompt.trim().is_empty() => {
                let rows = self.ambi_rows();
                if let Some(Run::Ask { id, args, .. }) = rows.get(row).and_then(|r| r.options.get(col)).map(|o| &o.run) {
                    let mut args = args.clone();
                    args.push(self.prompt.trim().to_string());
                    let _ = actions.dispatch(id, args);
                    self.note = format!("{}: setting it...", self.panel_title());
                }
                self.overlay = Overlay::Ambilight { row, col };
            }
            KeyCode::Backspace => {
                self.prompt.pop();
            }
            KeyCode::Char(c) if !mods.contains(KeyModifiers::CONTROL) => self.prompt.push(c),
            _ => {}
        }
    }

    /// The panel's lines: what it shows, or what's needed.
    pub(super) fn ambilight_lines(&self) -> Vec<(Line<'static>, Option<Hit>)> {
        let dim = Style::new().fg(Color::DarkGray);
        match self.panel_view() {
            None => vec![(Line::from(format!("reading the {}...", self.panel_title())).italic(), None)],
            Some(View::Missing(why)) => vec![(Line::from(why.clone()), None)],
            Some(View::NeedsPairing { why, .. }) => vec![
                (Line::from(why.clone()), None),
                (
                    Line::from(vec![Span::styled(" P ", Style::new().fg(Color::Black).bg(Color::Yellow)), Span::raw(" pair now: the TV shows a PIN to type in")]),
                    Some(Hit::Input(Input::Pair)),
                ),
            ],
            Some(View::Failed(e)) => vec![(Line::from(format!("! {e}")).fg(Color::Red), None)],
            Some(View::Shown(s)) => {
                let open = Some(Hit::Input(Input::AmbilightDialog));
                let mut out: Vec<(Line<'static>, Option<Hit>)> = s
                    .info
                    .iter()
                    .map(|(k, v)| (Line::from(vec![Span::styled(format!("{k:<9}"), dim), Span::raw(v.clone())]), open.clone()))
                    .collect();
                // a block of each colour, a row per swatch row (a side's lights)
                for (label, colours) in &s.swatches {
                    let mut spans = vec![Span::styled(format!("{label:<9}"), dim)];
                    for &(r, g, b) in colours {
                        spans.push(Span::styled("      ", Style::new().bg(Color::Rgb(r, g, b))));
                        spans.push(Span::raw(" "));
                    }
                    out.push((Line::from(spans), open.clone()));
                }
                out
            }
        }
    }

    pub(super) fn ambilight_height(&self) -> u16 {
        u16::try_from(self.ambilight_lines().len()).unwrap_or(1) + 2
    }

    /// The panel: what it shows now (the Ambilight: power, mode, style, Hue,
    /// and each light's colour).
    pub(super) fn draw_ambilight(&mut self, f: &mut Frame, area: ratatui::layout::Rect) {
        let title = format!(" {} - L (or click) changes it, l hides ", self.panel_title());
        let block = Block::bordered().title(title).border_style(Style::new().fg(Color::DarkGray));
        let inner = block.inner(area);
        f.render_widget(block, area);
        for (i, (line, hit)) in self.ambilight_lines().into_iter().enumerate() {
            let y = inner.y + u16::try_from(i).unwrap_or(u16::MAX);
            if y >= inner.y + inner.height {
                break;
            }
            let r = ratatui::layout::Rect { x: inner.x, y, width: inner.width, height: 1 };
            f.render_widget(Paragraph::new(line), r);
            if let Some(h) = hit {
                self.hits.push((r, h));
            }
        }
    }

    pub(super) fn draw_ambi_dialog(&mut self, f: &mut Frame, row: usize, col: usize) {
        let rows = self.ambi_rows();
        // rows come and go (outside control has no settings row): the
        // lights row is last, so a row past the end is it
        let row = row.min(rows.len().saturating_sub(1));
        let mut text: Vec<Line> = Vec::new();
        // (line, x offset in it, width, row, option)
        let mut spots: Vec<(usize, usize, usize, usize, usize)> = Vec::new();
        if rows.is_empty() {
            text.push(Line::from(match self.panel_view() {
                Some(View::NeedsPairing { .. }) => "tveee isn't paired with the TV yet: P pairs".to_string(),
                Some(View::Failed(_)) => format!("couldn't read the {} (see the panel)", self.panel_title()),
                _ => format!("reading the {}...", self.panel_title()),
            }));
        }
        for (ri, r) in rows.iter().enumerate() {
            let mut spans = vec![Span::styled(format!("{:<9}", r.name), Style::new().fg(Color::DarkGray))];
            let mut x = 9;
            for (ci, choice) in r.options.iter().enumerate() {
                let mut st = Style::new();
                if r.current == Some(ci) {
                    st = st.fg(Color::Green).bold();
                }
                // a light shows its colour
                if let Some((rr, g, b)) = choice.colour {
                    let dark = u32::from(rr) * 3 + u32::from(g) * 6 + u32::from(b) < 1280;
                    st = st.bg(Color::Rgb(rr, g, b)).fg(if dark { Color::White } else { Color::Black });
                }
                if ri == row && ci == col.min(r.options.len().saturating_sub(1)) {
                    st = st.reversed();
                }
                let shown = format!(" {} ", choice.label);
                spots.push((text.len(), x, shown.chars().count(), ri, ci));
                x += shown.chars().count() + 1;
                spans.push(Span::styled(shown, st));
                spans.push(Span::raw(" "));
            }
            text.push(Line::from(spans));
        }
        text.push(Line::default());
        text.push(Line::from("↑/↓ row  ←/→ change (at once)  ⏎ on a light: its colour  esc closes").fg(Color::DarkGray));
        text.push(Line::from("green: what the TV is doing now").fg(Color::DarkGray));
        let r = popup(f, &format!(" {} ", self.panel_title()), text, Color::Yellow);
        self.hits.push((r, Hit::Popup));
        for (line, x, w, ri, ci) in spots {
            let y = r.y + 1 + u16::try_from(line).unwrap_or(0);
            let x = r.x + 1 + u16::try_from(x).unwrap_or(0);
            self.hits.push((ratatui::layout::Rect { x, y, width: u16::try_from(w).unwrap_or(1), height: 1 }, Hit::AmbiOption(ri, ci)));
        }
    }

    /// Asking for what option `col` of row `row` needs (a light's colour).
    pub(super) fn draw_ambi_colour(&mut self, f: &mut Frame, row: usize, col: usize) {
        let rows = self.ambi_rows();
        let (prompt, hint) = match rows.get(row).and_then(|r| r.options.get(col)).map(|o| &o.run) {
            Some(Run::Ask { prompt, hint, .. }) => (prompt.clone(), *hint),
            _ => ("Colour:".to_string(), ""),
        };
        let text = vec![
            Line::from(prompt),
            Line::from(vec![
                Span::styled(format!("{}█", self.prompt), Style::new().fg(Color::White).bg(Color::DarkGray)),
                Span::styled(" ".repeat(24usize.saturating_sub(self.prompt.chars().count() + 1)), Style::new().bg(Color::DarkGray)),
            ]),
            Line::from(hint).fg(Color::DarkGray),
            Line::default(),
            Line::from("⏎ set   esc back").fg(Color::DarkGray),
        ];
        let r = popup(f, &format!(" {} colour ", self.panel_title()), text, Color::Yellow);
        self.hits.push((r, Hit::Popup));
    }
}

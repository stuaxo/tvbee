//! The Ambilight panel and dialog.

use super::*;

impl App {
    /// The Ambilight dialog's rows, from the last reading (none until there is one).
    pub(super) fn ambi_rows(&self) -> Vec<crate::plugins::ambilight::Row> {
        match self.st.snap.as_ref().and_then(|s| s.ambilight.as_ref()) {
            Some(crate::plugins::ambilight::Panel::Shown { state, lights }) => crate::plugins::ambilight::rows(state, lights),
            _ => Vec::new(),
        }
    }

    pub(super) fn ambi_lights(&self) -> Vec<crate::plugins::ambilight::Light> {
        match self.st.snap.as_ref().and_then(|s| s.ambilight.as_ref()) {
            Some(crate::plugins::ambilight::Panel::Shown { lights, .. }) => lights.clone(),
            _ => Vec::new(),
        }
    }

    /// Pick option `col` of row `row` in the Ambilight dialog: make its
    /// change, or (a light) ask for a colour.
    pub(super) fn ambi_pick(&mut self, row: usize, col: usize, actions: &Handle) {
        let rows = self.ambi_rows();
        let Some(r) = rows.get(row) else { return };
        if r.name == "lights" {
            self.prompt.clear();
            self.overlay = Overlay::AmbiColour { col };
            return;
        }
        self.overlay = Overlay::Ambilight { row, col };
        if let Some((label, Some(change))) = r.options.get(col) {
            self.note = format!("ambilight: {label}...");
            let _ = actions.send(Action::AmbilightChange(change.clone()));
        }
    }

    /// Keys in the Ambilight dialog: ↑/↓ rows, ←/→ change at once, ⏎ picks.
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
                if rows[row].name == "lights" {
                    self.overlay = Overlay::Ambilight { row, col: c };
                } else if c != col.min(n - 1) || rows[row].current != Some(c) {
                    self.ambi_pick(row, c, actions);
                }
            }
            KeyCode::Enter | KeyCode::Char(' ') => self.ambi_pick(row, col.min(n - 1), actions),
            _ => {}
        }
    }

    /// The Ambilight panel's lines: state, or what's needed.
    pub(super) fn ambilight_lines(&self) -> Vec<(Line<'static>, Option<Hit>)> {
        use crate::plugins::ambilight::Panel;
        let dim = Style::new().fg(Color::DarkGray);
        match self.st.snap.as_ref().and_then(|s| s.ambilight.as_ref()) {
            None => vec![(Line::from("reading the Ambilight...").italic(), None)],
            Some(Panel::Missing) => vec![(Line::from("no Ambilight here that tveee can control"), None)],
            Some(Panel::NeedsPairing) => vec![
                (Line::from("tveee isn't paired with the TV yet, which Ambilight needs."), None),
                (
                    Line::from(vec![Span::styled(" P ", Style::new().fg(Color::Black).bg(Color::Yellow)), Span::raw(" pair now: the TV shows a PIN to type in")]),
                    Some(Hit::Input(Input::Pair)),
                ),
            ],
            Some(Panel::Failed(e)) => vec![(Line::from(format!("! {e}")).fg(Color::Red), None)],
            Some(Panel::Shown { state, lights }) => {
                let open = Some(Hit::Input(Input::AmbilightDialog));
                let mut out: Vec<(Line<'static>, Option<Hit>)> = state
                    .lines()
                    .iter()
                    .filter(|l| !l.starts_with("lights:"))
                    .map(|l| {
                        let (k, v) = l.split_once(':').unwrap_or((l, ""));
                        (Line::from(vec![Span::styled(format!("{k:<9}"), dim), Span::raw(v.trim().to_string())]), open.clone())
                    })
                    .collect();
                // a block of each light's colour, a row per side
                let mut sides: Vec<&str> = Vec::new();
                for l in lights {
                    if !sides.contains(&l.side.as_str()) {
                        sides.push(&l.side);
                    }
                }
                for side in sides {
                    let mut spans = vec![Span::styled(format!("{side:<9}"), dim)];
                    for l in lights.iter().filter(|l| l.side == side) {
                        let (r, g, b) = l.rgb;
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

    /// The Ambilight panel: power, mode, style, Hue, and each light's colour now.
    pub(super) fn draw_ambilight(&mut self, f: &mut Frame, area: ratatui::layout::Rect) {
        let block = Block::bordered().title(" ambilight - L (or click) changes it, l hides ").border_style(Style::new().fg(Color::DarkGray));
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
        let lights = self.ambi_lights();
        let mut text: Vec<Line> = Vec::new();
        // (line, x offset in it, width, row, option)
        let mut spots: Vec<(usize, usize, usize, usize, usize)> = Vec::new();
        if rows.is_empty() {
            text.push(Line::from(match self.st.snap.as_ref().and_then(|s| s.ambilight.as_ref()) {
                Some(crate::plugins::ambilight::Panel::NeedsPairing) => "tveee isn't paired with the TV yet: P pairs",
                Some(crate::plugins::ambilight::Panel::Failed(_)) => "couldn't read the Ambilight (see the panel)",
                _ => "reading the Ambilight...",
            }));
        }
        for (ri, r) in rows.iter().enumerate() {
            let mut spans = vec![Span::styled(format!("{:<9}", r.name), Style::new().fg(Color::DarkGray))];
            let mut x = 9;
            for (ci, (label, _)) in r.options.iter().enumerate() {
                let mut st = Style::new();
                if r.current == Some(ci) {
                    st = st.fg(Color::Green).bold();
                }
                // a light shows its colour
                if r.name == "lights" && ci > 0 {
                    if let Some(l) = lights.get(ci - 1) {
                        let (rr, g, b) = l.rgb;
                        let dark = u32::from(rr) * 3 + u32::from(g) * 6 + u32::from(b) < 1280;
                        st = st.bg(Color::Rgb(rr, g, b)).fg(if dark { Color::White } else { Color::Black });
                    }
                }
                if ri == row && ci == col.min(r.options.len().saturating_sub(1)) {
                    st = st.reversed();
                }
                let shown = format!(" {label} ");
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
        let r = popup(f, " ambilight ", text, Color::Yellow);
        self.hits.push((r, Hit::Popup));
        for (line, x, w, ri, ci) in spots {
            let y = r.y + 1 + u16::try_from(line).unwrap_or(0);
            let x = r.x + 1 + u16::try_from(x).unwrap_or(0);
            self.hits.push((ratatui::layout::Rect { x, y, width: u16::try_from(w).unwrap_or(1), height: 1 }, Hit::AmbiOption(ri, ci)));
        }
    }

    pub(super) fn draw_ambi_colour(&mut self, f: &mut Frame, col: usize) {
        let which = match col.checked_sub(1).and_then(|i| self.ambi_lights().get(i).cloned()) {
            Some(l) => format!("{} {}", l.side, l.n),
            None => "every light".into(),
        };
        let text = vec![
            Line::from(format!("Colour for {which}:")),
            Line::from(vec![
                Span::styled(format!("{}█", self.prompt), Style::new().fg(Color::White).bg(Color::DarkGray)),
                Span::styled(" ".repeat(24usize.saturating_sub(self.prompt.chars().count() + 1)), Style::new().bg(Color::DarkGray)),
            ]),
            Line::from("red, warm, #ff8800 or 255,136,0 (the TV hands the lights over)").fg(Color::DarkGray),
            Line::default(),
            Line::from("⏎ set   esc back").fg(Color::DarkGray),
        ];
        let r = popup(f, " ambilight colour ", text, Color::Yellow);
        self.hits.push((r, Hit::Popup));
    }
}

//! The panel (`tveee_core::panel`: the Ambilight, on TVs with one) and its dialog.

use super::*;
use tveee_core::panel::{Row, Run, View};

/// An option as shown: a word (` on `), or a light's colour block with a
/// space either side (` ▆▆ `); the one with the cursor has thin bars in
/// those spaces, against it (`▕on▏`, `▕▆▆▏`: the eighth blocks sit at a
/// cell's right and left edge), so nothing moves. Green: in effect now.
/// With its width.
fn option_spans(choice: &tveee_core::panel::Choice, here: bool, current: bool) -> (Vec<Span<'static>>, u16) {
    let (open, close) = if here { ("▕", "▏") } else { (" ", " ") };
    let mark = Style::new().fg(Color::Yellow).bold();
    match choice.colour {
        Some((r, g, b)) => {
            // white bars frame a light best; yellow round one that's near white
            let light = u32::from(r) + u32::from(g) + u32::from(b) > 600;
            let bar = Style::new().fg(if light { Color::Yellow } else { Color::White }).bold();
            (vec![Span::styled(open, bar), Span::styled("  ", Style::new().bg(Color::Rgb(r, g, b))), Span::styled(close, bar)], 4)
        }
        None => {
            let mut st = Style::new();
            if current {
                st = st.fg(Color::Green).bold();
            }
            let w = u16::try_from(choice.label.chars().count() + 2).unwrap_or(u16::MAX);
            (vec![Span::styled(open, mark), Span::styled(choice.label.clone(), st), Span::styled(close, mark)], w)
        }
    }
}

/// A clickable part of a panel line: x, width, what clicking does.
type Spot = (u16, u16, Hit);

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

    /// Another light clicked while the colour picker is open (in the panel,
    /// or the picker's row of lights): the picker sets that one instead, what
    /// was picked or typed kept. Anything else: changed as in the panel.
    pub(super) fn ambi_switch(&mut self, row: usize, col: usize, actions: &Handle) {
        let asks = self.ambi_rows().get(row).and_then(|r| r.options.get(col)).is_some_and(|o| matches!(o.run, Run::Ask { .. }));
        match self.overlay {
            Overlay::AmbiColour { row: open, .. } if open == row && asks => self.overlay = Overlay::AmbiColour { row, col },
            _ => {
                self.prompt.clear();
                self.overlay = Overlay::Ambilight { row, col };
                self.ambi_pick(row, col, actions);
            }
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

    /// Keys in the colour picker: the arrows move through the grid, typing
    /// goes in its field (a name, #hex), ⏎ sets what's typed or else the
    /// colour picked, Esc goes back to the panel.
    pub(super) fn ambi_ask_key(&mut self, row: usize, col: usize, code: KeyCode, mods: KeyModifiers, actions: &Handle) {
        let (gx, gy) = self.pick;
        match code {
            KeyCode::Esc => self.overlay = Overlay::Ambilight { row, col },
            KeyCode::Enter => self.ambi_set(row, col, actions),
            // the next light (or back): what's picked or typed stays
            KeyCode::Tab | KeyCode::BackTab => {
                let n = self.ambi_rows().get(row).map_or(1, |r| r.options.len()).max(1);
                let to = if code == KeyCode::Tab { (col + 1) % n } else { (col + n - 1) % n };
                self.overlay = Overlay::AmbiColour { row, col: to };
            }
            KeyCode::Left => self.pick = ((gx + pick_width(gy) - 1) % pick_width(gy), gy),
            KeyCode::Right => self.pick = ((gx + 1) % pick_width(gy), gy),
            KeyCode::Up | KeyCode::Down => {
                let gy = if code == KeyCode::Up { (gy + PICK_HEIGHT - 1) % PICK_HEIGHT } else { (gy + 1) % PICK_HEIGHT };
                self.pick = (gx.min(pick_width(gy) - 1), gy);
            }
            KeyCode::Backspace => {
                self.prompt.pop();
            }
            KeyCode::Char('u') if mods.contains(KeyModifiers::CONTROL) => self.prompt.clear(),
            KeyCode::Char(c) if !mods.contains(KeyModifiers::CONTROL) => self.prompt.push(c),
            _ => {}
        }
    }

    /// Set the colour the picker has (what's typed, else the one picked) on
    /// option `col` of row `row`, and go back to the panel.
    pub(super) fn ambi_set(&mut self, row: usize, col: usize, actions: &Handle) {
        let typed = self.prompt.trim().to_string();
        let colour = if typed.is_empty() { hex(pick_colour(self.pick.0, self.pick.1)) } else { typed };
        let rows = self.ambi_rows();
        if let Some(Run::Ask { id, args, .. }) = rows.get(row).and_then(|r| r.options.get(col)).map(|o| &o.run) {
            let mut args = args.clone();
            args.push(colour.clone());
            let _ = actions.dispatch(id, args);
            self.note = format!("{}: {colour}...", self.panel_title());
        }
        self.prompt.clear();
        self.overlay = Overlay::Ambilight { row, col };
    }

    /// The panel's lines for `width`, each with its clickable spots (x,
    /// width, what): what's needed (pairing...), or the rows of choices
    /// themselves (the current one green; while it's being changed, the
    /// cursor on row `row`, option `col`). The lights row is the lights:
    /// "all", then a block in each light's colour, side by side.
    pub(super) fn ambilight_lines(&self, width: u16, cursor: Option<(usize, usize)>) -> Vec<(Line<'static>, Vec<Spot>)> {
        let dim = Style::new().fg(Color::DarkGray);
        let open = Hit::Input(Input::AmbilightDialog);
        let whole = |l: Line<'static>, h: Option<Hit>| (l, h.map(|h| vec![(0, width, h)]).unwrap_or_default());
        let s = match self.panel_view() {
            None => return vec![whole(Line::from(format!("reading the {}...", self.panel_title())).italic(), None)],
            Some(View::Missing(why)) => return vec![whole(Line::from(why.clone()), None)],
            Some(View::NeedsPairing { why, .. }) => {
                return vec![
                    whole(Line::from(why.clone()), None),
                    whole(Line::from(vec![Span::styled(" P ", Style::new().fg(Color::Black).bg(Color::Yellow)), Span::raw(" pair now: the TV shows a PIN to type in")]), Some(Hit::Input(Input::Pair))),
                ];
            }
            Some(View::Failed(e)) => return vec![whole(Line::from(format!("! {e}")).fg(Color::Red), None)],
            Some(View::Shown(s)) => s,
        };
        let mut out = Vec::new();
        // what the rows don't say (who has the lights: the TV, or outside control)
        let said: Vec<&str> = s.rows.iter().map(|r| r.name).chain(["style", "lights", "hue"]).collect();
        for (k, v) in s.info.iter().filter(|(k, _)| !said.contains(&k.as_str())) {
            out.push(whole(Line::from(vec![Span::styled(format!("{k:<9}"), dim), Span::raw(v.clone())]), Some(open.clone())));
        }
        const LABEL: u16 = 9;
        for (ri, r) in s.rows.iter().enumerate() {
            // the row the cursor is in: its name bold
            let name = if cursor.is_some_and(|(r, _)| r == ri) { Style::new().bold() } else { dim };
            let mut spans = vec![Span::styled(format!("{:<9}", r.name), name)];
            let mut spots = Vec::new();
            let mut x = LABEL;
            let mut side: Option<String> = None;
            for (ci, choice) in r.options.iter().enumerate() {
                // the sides' lights apart
                if choice.colour.is_some() {
                    let this = choice.label.split(' ').next().unwrap_or("").to_string();
                    if side.as_ref().is_some_and(|s| *s != this) {
                        spans.push(Span::styled("│", dim));
                        x += 1;
                    }
                    side = Some(this);
                }
                let (shown, w) = option_spans(choice, cursor == Some((ri, ci)), r.current == Some(ci));
                // the options wrap under themselves
                if x + w > width && x > LABEL {
                    out.push((Line::from(std::mem::take(&mut spans)), std::mem::take(&mut spots)));
                    spans.push(Span::raw(" ".repeat(usize::from(LABEL))));
                    x = LABEL;
                }
                spots.push((x, w, Hit::AmbiOption(ri, ci)));
                spans.extend(shown);
                // a word gets a space after it; a light has its own
                let gap = u16::from(choice.colour.is_none());
                if gap == 1 {
                    spans.push(Span::raw(" "));
                }
                x += w + gap;
            }
            // what it does, beside its options, else under them
            if let Some(help) = r.help {
                let w = u16::try_from(help.chars().count()).unwrap_or(u16::MAX);
                if x + 1 + w > width {
                    out.push((Line::from(std::mem::take(&mut spans)), std::mem::take(&mut spots)));
                    spans.push(Span::raw(" ".repeat(usize::from(LABEL))));
                }
                spans.push(Span::styled(format!(" {help}"), dim));
            }
            out.push((Line::from(spans), spots));
        }
        out
    }

    pub(super) fn ambilight_height(&self, width: u16) -> u16 {
        u16::try_from(self.ambilight_lines(width.saturating_sub(2), None).len()).unwrap_or(1) + 2
    }

    /// The panel: its rows of choices (the Ambilight: power, style, its
    /// setting, Hue, the lights), changed right here: L (or a click) puts
    /// the cursor in it, its border yellow while it's there.
    pub(super) fn draw_ambilight(&mut self, f: &mut Frame, area: ratatui::layout::Rect) {
        let cursor = match self.overlay {
            Overlay::Ambilight { row, col } | Overlay::AmbiColour { row, col } => Some((row, col)),
            _ => None,
        };
        let (title, colour) = if cursor.is_some() {
            (format!(" {}: ←→ change, ⏎ colour, esc done ", self.panel_title()), Color::Yellow)
        } else {
            (format!(" {} - L (or click) changes it, l hides ", self.panel_title()), Color::DarkGray)
        };
        let block = Block::bordered().title(title).border_style(Style::new().fg(colour));
        let inner = block.inner(area);
        f.render_widget(block, area);
        if cursor.is_some() {
            self.hits.push((area, Hit::Popup));
        }
        for (i, (line, spots)) in self.ambilight_lines(inner.width, cursor).into_iter().enumerate() {
            let y = inner.y + u16::try_from(i).unwrap_or(u16::MAX);
            if y >= inner.y + inner.height {
                break;
            }
            let r = ratatui::layout::Rect { x: inner.x, y, width: inner.width, height: 1 };
            f.render_widget(Paragraph::new(line), r);
            // the whole row puts the cursor in; each option is its own spot
            self.hits.push((r, Hit::Input(Input::AmbilightDialog)));
            for (x, w, h) in spots {
                self.hits.push((ratatui::layout::Rect { x: inner.x + x, y, width: w.min(inner.width.saturating_sub(x)), height: 1 }, h));
            }
        }
    }

    /// The colour picker for option `col` of row `row` (a light, or all of
    /// them): a grid of colours to move through or click, or one typed.
    pub(super) fn draw_ambi_colour(&mut self, f: &mut Frame, row: usize, col: usize) {
        let rows = self.ambi_rows();
        let choice = rows.get(row).and_then(|r| r.options.get(col));
        let (prompt, hint) = match choice.map(|o| &o.run) {
            Some(Run::Ask { prompt, hint, .. }) => (prompt.clone(), *hint),
            _ => ("Colour:".to_string(), ""),
        };
        let dim = Style::new().fg(Color::DarkGray);
        let block = |(r, g, b): (u8, u8, u8)| Style::new().bg(Color::Rgb(r, g, b));
        let mut text = vec![Line::from(prompt.trim_end_matches(':').to_string())];
        // the lights, together: the one being set marked; click one (or
        // Tab) to set that one instead
        let mut lights: Vec<(usize, usize, usize)> = Vec::new();
        if let Some(r) = rows.get(row) {
            let mut spans = vec![Span::styled("for  ", Style::new().bold())];
            let mut x = 5;
            let mut side: Option<&str> = None;
            for (ci, o) in r.options.iter().enumerate() {
                if o.colour.is_some() {
                    let this = o.label.split(' ').next().unwrap_or("");
                    if side.is_some_and(|s| s != this) {
                        spans.push(Span::styled("│", dim));
                        x += 1;
                    }
                    side = Some(this);
                }
                let (shown, w) = option_spans(o, ci == col, false);
                let w = usize::from(w);
                lights.push((x, w, ci));
                spans.extend(shown);
                let gap = usize::from(o.colour.is_none());
                if gap == 1 {
                    spans.push(Span::raw(" "));
                }
                x += w + gap;
            }
            spans.push(Span::styled(" tab: the next", dim));
            text.push(Line::from(spans));
        }
        let lights_line = text.len() - 1;
        text.push(Line::default());
        let grid_top = text.len();
        let mut spots = Vec::new();
        for (gy, name) in PICK_ROWS.iter().enumerate() {
            let mut spans: Vec<Span> = Vec::new();
            let mut x = 0;
            for gx in 0..pick_width(gy) {
                let on = self.pick == (gx, gy) && self.prompt.is_empty();
                let colour = pick_colour(gx, gy);
                let dark = u32::from(colour.0) * 3 + u32::from(colour.1) * 6 + u32::from(colour.2) < 1280;
                let label = if gy == SPECIALS {
                    format!(" {} ", SPECIAL.get(gx).map_or("", |s| s.0))
                } else if on {
                    "▐▌".to_string()
                } else {
                    "  ".to_string()
                };
                let mut st = block(colour).fg(if dark { Color::White } else { Color::Black });
                if on {
                    st = st.add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
                }
                let w = label.chars().count();
                spots.push((grid_top + gy, x, w, gx, gy));
                spans.push(Span::styled(label, st));
                spans.push(Span::raw(" "));
                x += w + 1;
            }
            spans.push(Span::styled(format!(" {name}"), dim));
            text.push(Line::from(spans));
        }
        text.push(Line::default());
        text.push(Line::from(vec![
            Span::styled("or type: ", dim),
            Span::styled(format!("{}█", self.prompt), Style::new().fg(Color::White).bg(Color::DarkGray)),
            Span::styled(" ".repeat(20usize.saturating_sub(self.prompt.chars().count() + 1)), Style::new().bg(Color::DarkGray)),
        ]));
        text.push(Line::from(Span::styled(hint, dim)));
        // the light's colour now, and the one picked
        let mut cmp = vec![Span::styled("now ", dim)];
        match choice.and_then(|c| c.colour) {
            Some(c) => cmp.push(Span::styled("    ", block(c))),
            None => cmp.push(Span::styled("(each its own)", dim)),
        }
        cmp.push(Span::styled("  →  ", dim));
        if self.prompt.is_empty() {
            cmp.push(Span::styled("    ", block(pick_colour(self.pick.0, self.pick.1))));
        } else {
            cmp.push(Span::raw(self.prompt.clone()));
        }
        text.push(Line::default());
        text.push(Line::from(cmp));
        text.push(Line::from("arrows or click pick  type a name or #hex  ⏎ set  esc back").fg(Color::DarkGray));
        let r = popup(f, &format!(" {} colour ", self.panel_title()), text, Color::Yellow);
        self.hits.push((r, Hit::Popup));
        for (line, x, w, gx, gy) in spots {
            let y = r.y + 1 + u16::try_from(line).unwrap_or(0);
            let x = r.x + 1 + u16::try_from(x).unwrap_or(0);
            self.hits.push((ratatui::layout::Rect { x, y, width: u16::try_from(w).unwrap_or(1), height: 1 }, Hit::AmbiPick(gx, gy)));
        }
        for (x, w, ci) in lights {
            let y = r.y + 1 + u16::try_from(lights_line).unwrap_or(0);
            let x = r.x + 1 + u16::try_from(x).unwrap_or(0);
            self.hits.push((ratatui::layout::Rect { x, y, width: u16::try_from(w).unwrap_or(1), height: 1 }, Hit::AmbiOption(row, ci)));
        }
    }
}

/// The picker's rows: a lighter, a full and a darker of each hue, then
/// white, warm and off.
const PICK_ROWS: [&str; 4] = ["lighter", "full", "darker", ""];
/// The row of named colours.
pub const SPECIALS: usize = 3;
/// Hues across the grid.
const HUES: usize = 12;
/// The named colours' row: (name, colour).
const SPECIAL: [(&str, (u8, u8, u8)); 3] = [("white", (255, 255, 255)), ("warm", (255, 147, 41)), ("off", (0, 0, 0))];

/// How many colours row `gy` of the picker has.
pub fn pick_width(gy: usize) -> usize {
    if gy == SPECIALS { SPECIAL.len() } else { HUES }
}

/// The picker's rows.
pub const PICK_HEIGHT: usize = PICK_ROWS.len();

/// The colour at column `gx`, row `gy` of the picker.
pub fn pick_colour(gx: usize, gy: usize) -> (u8, u8, u8) {
    if gy == SPECIALS {
        return SPECIAL[gx.min(SPECIAL.len() - 1)].1;
    }
    let (s, v) = match gy {
        0 => (0.45, 1.0),
        1 => (1.0, 1.0),
        _ => (1.0, 0.45),
    };
    hsv(gx as f64 * 360.0 / HUES as f64, s, v)
}

/// A colour from hue (degrees), saturation and value (0 to 1).
pub fn hsv(h: f64, s: f64, v: f64) -> (u8, u8, u8) {
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let to = |f: f64| ((f + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    (to(r), to(g), to(b))
}

/// `#rrggbb`.
pub fn hex((r, g, b): (u8, u8, u8)) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picker_colours() {
        assert_eq!(hsv(0.0, 1.0, 1.0), (255, 0, 0));
        assert_eq!(hsv(120.0, 1.0, 1.0), (0, 255, 0));
        assert_eq!(hsv(240.0, 1.0, 1.0), (0, 0, 255));
        assert_eq!(pick_colour(0, 1), (255, 0, 0));
        assert_eq!(pick_colour(4, 1), (0, 255, 0));
        // lighter and darker of red
        assert_eq!(pick_colour(0, 0), (255, 140, 140));
        assert_eq!(pick_colour(0, 2), (115, 0, 0));
        assert_eq!(pick_colour(1, SPECIALS), (255, 147, 41));
        assert_eq!(hex((255, 136, 0)), "#ff8800");
        assert_eq!((pick_width(0), pick_width(SPECIALS)), (12, 3));
    }
}

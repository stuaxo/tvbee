//! Drawing the main screen: what's on, the footer's buttons, the system
//! panel, and pop-up helpers.

use super::*;

/// Height of the memory graph.
pub(super) const GRAPH_ROWS: u16 = 3;

/// Something on screen that responds to a click.
#[derive(Debug, Clone, PartialEq)]
pub enum Hit {
    Input(Input),
    /// An option in the Ambilight dialog (row, option).
    AmbiOption(usize, usize),
    /// The progress bar: clicking jumps there.
    Progress { x0: u16, width: u16 },
    /// The volume bar: clicking sets that level.
    Volume { x0: u16, width: u16 },
    MenuItem(usize),
    /// Row N of the launcher's current tab.
    AppItem(usize),
    AppTab(AppTab),
    /// Choice N in the launcher's actions pop-up.
    AppChoice(usize),
    /// A channel (by id) in the guide.
    GuideRow(u32),
    /// The star of a channel in the guide: never switches channel.
    GuideStar(u32),
    GuideTab(GuideTab),
    /// Option N in the options dialog.
    OptionItem(usize),
    /// The options dialog's tabs: its options (false), the plugins (true).
    OptionTab(bool),
    /// Part of the guide's details panel.
    Details(DetailsClick),
    /// A field of what's on that opens something.
    Open(nowplaying::Open),
    ConfirmYes,
    ConfirmNo,
    /// Inside a pop-up but on nothing in particular.
    Popup,
    /// The whole link's buttons: copy, open (worked out by where).
    UrlButtons,
    /// The row of things to do with what's playing (see [`App::chips`]);
    /// each has its own hit, this marks the row.
    Chips,
    /// Menu item N, from a button on the now-playing card.
    Chip(usize),
}

/// Columns `s` takes.
fn width_of(s: &str) -> u16 {
    u16::try_from(Span::raw(s).width()).unwrap_or(u16::MAX)
}

/// The whole link's buttons, as drawn after the label.
const COPY_BUTTON: &str = "[copy y]";
const OPEN_BUTTON: &str = "[open ↗]";

/// `(c + 0.5) / width`: the fraction that puts a bar's marker on cell `c`.
pub fn bar_fraction(x: u16, x0: u16, width: u16) -> f64 {
    let c = f64::from(x.saturating_sub(x0).min(width.saturating_sub(1)));
    ((c + 0.5) / f64::from(width.max(1))).clamp(0.0, 1.0)
}

/// A clickable button in the footer: the key, what it does, and its effect.
pub(super) struct Button {
    pub(super) key: &'static str,
    pub(super) label: &'static str,
    pub(super) input: Input,
}

pub(super) const fn button(key: &'static str, label: &'static str, input: Input) -> Button {
    Button { key, label, input }
}

pub(super) fn buttons(mode: Mode, exact: bool, ambilight: bool) -> Vec<Button> {
    use Action::*;
    use Input::{Do, OpenMenu, Quit, ToggleHelp, ToggleMode};
    let mut b = match mode {
        Mode::Player => vec![
            button("space", "play/pause", Do(PlayPause)),
            // exact seeking (YouTube) vs fast-forward/rewind key presses
            button("←", if exact { "-10s" } else { "rw" }, Do(Seek(-10.0))),
            button("→", if exact { "+10s" } else { "ff" }, Do(Seek(10.0))),
            button("↑", "vol+", Do(Volume(1))),
            button("↓", "vol-", Do(Volume(-1))),
            button("m", "mute", Do(Mute)),
        ],
        // keys go to the TV: the rest are buttons to click (no key)
        Mode::Remote => {
            return vec![
                button("↑", "up", Do(Key(19))),
                button("↓", "down", Do(Key(20))),
                button("←", "left", Do(Key(21))),
                button("→", "right", Do(Key(22))),
                button("⏎", "ok", Do(Key(KEY_OK))),
                button("esc", "back", Do(Key(KEY_BACK))),
                button("⌫", "delete", Do(Key(KEY_DEL))),
                button("home", "home", Do(Key(KEY_HOME))),
                button("tab", "player mode", ToggleMode),
                button("^p", "cmds", Input::OpenPalette),
                button("^c", "quit", Quit),
                button("", "play/pause", Do(PlayPause)),
                button("", "vol+", Do(Volume(1))),
                button("", "vol-", Do(Volume(-1))),
                button("", "info", Input::ToggleInfo),
                button("", "ambilight", Input::ToggleAmbilight),
                button("", "preview", Input::TogglePreview),
                button("", "apps", Input::OpenApps),
                button("", "search", Input::OpenSearch),
                button("", "guide", Input::OpenGuide),
                button("", "options", Input::OpenOptions),
                button("", "more", OpenMenu),
                button("", "help", ToggleHelp),
            ];
        }
    };
    b.extend([
        button("⌫", "back", Do(Key(KEY_BACK))),
        button("h", "home", Do(Key(KEY_HOME))),
        button("tab", if mode == Mode::Player { "remote" } else { "player" }, ToggleMode),
        button("i", "info", Input::ToggleInfo),
        button("l", "ambilight", Input::ToggleAmbilight),
        button("v", "preview", Input::TogglePreview),
        button("a", "apps", Input::OpenApps),
        button("/", "search", Input::OpenSearch),
        button("g", "guide", Input::OpenGuide),
        button(",", "options", Input::OpenOptions),
        button(".", "more", OpenMenu),
        button(":", "cmds", Input::OpenPalette),
        button("?", "help", ToggleHelp),
        button("q", "quit", Quit),
    ]);
    if !ambilight {
        b.retain(|x| x.label != "ambilight");
    }
    b
}

/// What goes first when the keys don't fit one row, by label: the least
/// used first (they're all still keys, in the help and the palette).
const DROP_FIRST: &[&str] = &[
    // remote mode's click-only buttons
    "help", "more", "options", "guide", "search", "apps", "preview", "ambilight", "info", "vol-", "vol+", "play/pause",
    // then remote mode's keys, the obvious ones last
    "back", "home", "delete", "ok", "right", "left", "down", "up",
];

/// `bs` cut down to one row of `width`: the least used go first (see
/// [`DROP_FIRST`]); playback, the mode, commands, help and quit stay.
pub(super) fn one_row(mut bs: Vec<Button>, width: u16) -> Vec<Button> {
    let fits = |bs: &[Button]| flow(bs, width).iter().all(|(r, _)| *r == 0);
    // player mode's own order of going: the panels, then the rest
    let player_first = ["preview", "options", "info", "ambilight", "more", "back", "home", "search", "apps", "guide", "mute"];
    let order: Vec<&str> = player_first.iter().chain(DROP_FIRST).copied().collect();
    for label in order {
        if fits(&bs) {
            break;
        }
        // a click-only button (remote mode) before a key with the same name
        let at = bs.iter().rposition(|b| b.label == label && b.key.is_empty()).or_else(|| bs.iter().position(|b| b.label == label));
        if let Some(i) = at {
            bs.remove(i);
        }
    }
    bs
}

/// A button on the now-playing card: " key " and " what".
fn chip_width(key: &str, text: &str) -> u16 {
    u16::try_from(key.chars().count() + 2 + text.chars().count() + 1).unwrap_or(u16::MAX)
}

/// A row of them, with two spaces between.
fn chips_width(chips: &[(String, String, Hit)]) -> usize {
    chips.iter().map(|(k, t, _)| usize::from(chip_width(k, t)) + 2).sum::<usize>().saturating_sub(2)
}

/// From this wide (inside the border), the panels get a column of their
/// own on the right.
pub(super) const SIDE_FROM: u16 = 100;

/// The D-pad's height: three rows of keys and a border.
const DPAD_ROWS: u16 = 5;

/// A small pane (an editor split, a corner of the screen): one row of keys.
pub(super) fn compact(area: ratatui::layout::Rect) -> bool {
    area.width < 65 || area.height < 18
}

/// The keys a small pane keeps, short enough for one row: playback,
/// seeking, commands, help, quit (everything else is in the palette and
/// the help).
pub(super) fn compact_buttons(mode: Mode) -> Vec<Button> {
    use Action::*;
    match mode {
        Mode::Player => vec![
            button("space", "play", Input::Do(PlayPause)),
            button("←", "", Input::Do(Seek(-10.0))),
            button("→", "", Input::Do(Seek(10.0))),
            button(":", "cmds", Input::OpenPalette),
            button("?", "help", Input::ToggleHelp),
            button("q", "quit", Input::Quit),
        ],
        // typing goes to the TV: keys that aren't typing
        Mode::Remote => vec![
            button("tab", "player", Input::ToggleMode),
            button("^p", "cmds", Input::OpenPalette),
            button("^c", "quit", Input::Quit),
        ],
    }
}

/// A key in a label: `[F]`, highlighted.
fn mnemonic(key: &str) -> Span<'static> {
    Span::styled(format!("[{key}]"), Style::new().fg(Color::Yellow).bold())
}

/// The TV's health and the quick keys, always at the top: load (and
/// whether it's rising), memory free (red when low) with [F]ree, the
/// panel (Ambilight) if there is one, and [:] commands. Each part is a
/// (text, what clicking it does).
pub(super) fn spine(system: Option<&tveee_core::builtin::info::System>, panel: Option<&str>) -> Vec<(Vec<Span<'static>>, Option<Input>)> {
    let dim = Style::new().fg(Color::DarkGray);
    let mut parts = Vec::new();
    if let Some((one, five, _)) = system.and_then(|s| s.load) {
        let trend = if one > five * 1.1 { "▲" } else if one < five * 0.9 { "▼" } else { " " };
        parts.push((vec![Span::styled("load ", dim), Span::raw(format!("{one:.2}{trend}"))], None));
    }
    let free = Input::Do(Action::Run { id: "apps.free", args: Vec::new() });
    match system.and_then(|s| s.mem) {
        Some(m) => {
            // under 150 MB the TV starts closing apps itself
            let low = m.available < 150 * 1024;
            let amount = Span::styled(tveee_core::builtin::info::size(m.available), if low { Style::new().fg(Color::Red).bold() } else { Style::new() });
            parts.push((vec![amount, Span::raw(" "), mnemonic("F"), Span::raw("ree")], Some(free)));
        }
        None => parts.push((vec![mnemonic("F"), Span::raw("ree memory")], Some(free))),
    }
    if let Some(title) = panel {
        parts.push((vec![mnemonic("l"), Span::raw(format!(" {title}"))], Some(Input::ToggleAmbilight)));
    }
    parts.push((vec![mnemonic(":"), Span::raw(" commands")], Some(Input::OpenPalette)));
    parts
}

pub(super) fn button_width(b: &Button) -> u16 {
    // " key " + " label", or " label " for a button with no key
    let key = if b.key.is_empty() { 0 } else { Span::raw(b.key).width() + 2 };
    u16::try_from(key + Span::raw(b.label).width() + 1 + usize::from(b.key.is_empty())).unwrap_or(u16::MAX)
}

/// Lay buttons out left to right, wrapping at `width`: (row, x offset) of each.
pub(super) fn flow(bs: &[Button], width: u16) -> Vec<(u16, u16)> {
    const GAP: u16 = 2;
    let (mut row, mut x) = (0u16, 0u16);
    bs.iter()
        .map(|b| {
            let w = button_width(b);
            if x > 0 && x + w > width {
                (row, x) = (row + 1, 0);
            }
            let at = (row, x);
            x += w + GAP;
            at
        })
        .collect()
}

/// Break `s` into lines of at most `w` characters, at spaces.
pub(super) fn textwrap(s: &str, w: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for word in s.split_whitespace() {
        match out.last_mut() {
            Some(l) if l.chars().count() + 1 + word.chars().count() <= w => {
                l.push(' ');
                l.push_str(word);
            }
            _ => out.push(word.to_string()),
        }
    }
    out
}

/// Where the memory graph starts (MB): 0, unless memory has stayed well up
/// all along; then just under the lowest, so changes near the top show
/// instead of a solid block.
pub(super) fn graph_floor(used: &[u64], total: u64) -> u64 {
    let (Some(&lo), Some(&hi)) = (used.iter().min(), used.iter().max()) else { return 0 };
    if total == 0 || lo * 2 < total {
        return 0;
    }
    // room below the lowest: as much again as it moves, and a little more
    let floor = lo.saturating_sub((hi - lo) + total / 20);
    floor / 50 * 50
}

/// `line` in rows of at most `w` characters, broken at spaces where it can;
/// the rows after the first start with `hang` spaces. Unlike [`textwrap`]
/// the spacing inside a row is kept (lined-up columns stay lined up).
pub(super) fn wrap_keep(line: &str, w: usize, hang: usize) -> Vec<String> {
    let w = w.max(hang + 8);
    let mut out = Vec::new();
    let mut rest: Vec<char> = line.trim_end().chars().collect();
    while rest.len() > w {
        let start = if out.is_empty() { 1 } else { hang + 1 };
        let cut = (start..=w).rev().find(|&i| rest[i] == ' ').unwrap_or(w);
        out.push(rest[..cut].iter().collect::<String>().trim_end().to_string());
        let tail: String = rest[cut..].iter().collect();
        rest = format!("{}{}", " ".repeat(hang), tail.trim_start()).chars().collect();
    }
    out.push(rest.into_iter().collect());
    out
}

/// A bordered box centred over the screen, sized to its text. Returns where.
pub(super) fn popup(f: &mut Frame, title: &str, text: Vec<Line>, border: Color) -> ratatui::layout::Rect {
    popup_titled(f, title, "", text, border)
}

/// [`popup`] with a second title at the top right (a position, a count).
pub(super) fn popup_titled(f: &mut Frame, title: &str, right: &str, text: Vec<Line>, border: Color) -> ratatui::layout::Rect {
    let h = u16::try_from(text.len()).unwrap_or(u16::MAX).saturating_add(2);
    let widest = text.iter().map(Line::width).max().unwrap_or(0).max(title.chars().count() + right.chars().count() + 1);
    let w = u16::try_from(widest).unwrap_or(u16::MAX).saturating_add(4);
    let area = f.area();
    let rect = ratatui::layout::Rect {
        x: area.x + area.width.saturating_sub(w) / 2,
        y: area.y + area.height.saturating_sub(h) / 2,
        width: w.min(area.width),
        height: h.min(area.height),
    };
    f.render_widget(ratatui::widgets::Clear, rect);
    f.render_widget(
        Paragraph::new(text).block(
            Block::bordered()
                .title(title.to_string())
                .title(Line::from(right.to_string()).right_aligned())
                .border_style(Style::new().fg(border)),
        ),
        rect,
    );
    rect
}

impl App {
    pub(super) fn draw(&mut self, f: &mut Frame) {
        self.hits.clear();
        self.links.clear();
        // the system figures are read only while something shows them
        let wanted = self.info_shown();
        if wanted != self.system_watching {
            let _ = self.engine.send(Action::WatchSystem(wanted));
            self.system_watching = wanted;
        }
        let outer = Block::bordered()
            .title(Line::from(format!(" tveee ui - {} ", self.ip)).bold())
            .title(
                Line::from(match self.mode {
                    Mode::Player => " player ",
                    Mode::Remote => " remote ",
                })
                .right_aligned()
                .style(Style::new().fg(Color::Black).bg(match self.mode {
                    Mode::Player => Color::Cyan,
                    Mode::Remote => Color::Magenta,
                })),
            )
            .border_style(Style::new().fg(Color::DarkGray));
        let area = outer.inner(f.area());
        f.render_widget(outer, f.area());

        let small = compact(area);
        let bs = buttons(self.mode, self.st.snap.as_ref().is_some_and(|s| s.exact), self.panel_id().is_some());
        let bs = if small { compact_buttons(self.mode) } else { bs };
        // one row of keys at any width (the rest: the palette, help)
        let bs = one_row(bs, area.width);
        let places = flow(&bs, area.width);
        let rows = places.iter().map(|(r, _)| r + 1).max().unwrap_or(1);
        let [top, body, _, footer, status] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(4),
            Constraint::Length(u16::from(!small)),
            Constraint::Length(rows),
            Constraint::Length(1),
        ])
        .areas(area);
        self.draw_spine(f, top);

        // wide: a column on the right for the D-pad (remote mode) and the
        // panels; else the panels go below what's playing
        self.wide = body.width >= SIDE_FROM;
        let body = if self.wide {
            let w = (body.width * 2 / 5).clamp(44, 64);
            let [left, side] = Layout::horizontal([Constraint::Min(50), Constraint::Length(w)]).areas(body);
            self.draw_side(f, side);
            left
        } else {
            self.stack_panels(f, body)
        };
        self.draw_body(f, body);

        // footer: one clickable button per action
        for (b, (row, x)) in bs.iter().zip(places) {
            let rect = ratatui::layout::Rect { x: footer.x + x, y: footer.y + row, width: button_width(b), height: 1 };
            if rect.y >= footer.y + footer.height || rect.x + rect.width > footer.x + footer.width {
                continue;
            }
            let line = if b.key.is_empty() {
                // click only: a button of its own
                Line::from(Span::styled(format!(" {} ", b.label), Style::new().fg(Color::Black).bg(Color::DarkGray)))
            } else {
                Line::from(vec![
                    Span::styled(format!(" {} ", b.key), Style::new().fg(Color::Black).bg(Color::Gray)),
                    Span::styled(format!(" {}", b.label), Style::new().fg(Color::Gray)),
                ])
            };
            f.render_widget(Paragraph::new(line), rect);
            self.hits.push((rect, Hit::Input(b.input.clone())));
        }

        let line = match &self.st.error {
            Some(e) => Line::from(format!("! {e}")).style(Style::new().fg(Color::Red)),
            None => Line::from(self.note.clone()).style(Style::new().add_modifier(Modifier::ITALIC)),
        };
        f.render_widget(Paragraph::new(line), status);

        self.draw_overlay(f);
    }

    /// Whether the system panel shows: as `i` last left it, else when the
    /// screen is wide (there's a column for it) and there's something to
    /// show (not in standby, or before the first reading).
    pub(super) fn info_shown(&self) -> bool {
        let read = self.st.snap.as_ref().is_some_and(|s| s.system.mem.is_some() && !s.standby && !self.st.seeded);
        self.info.unwrap_or(self.wide && read)
    }

    /// A narrow screen: the open panels below what's playing (preview,
    /// Ambilight, system), each leaving it at least 4 rows. Returns what's
    /// left for it.
    pub(super) fn stack_panels(&mut self, f: &mut Frame, body: ratatui::layout::Rect) -> ratatui::layout::Rect {
        // (rows below what's playing, the rest above)
        let below = |body: ratatui::layout::Rect, want: u16| {
            let h = want.min(body.height.saturating_sub(4));
            let [top, panel] = Layout::vertical([Constraint::Min(4), Constraint::Length(h)]).areas(body);
            (panel, top)
        };
        let mut body = body;
        if self.preview_open {
            let (panel, top) = below(body, (body.height / 2).clamp(6, 14));
            self.draw_preview(f, panel);
            body = top;
        }
        if self.ambi_open && self.panel_id().is_some() {
            let (panel, top) = below(body, self.ambilight_height());
            self.draw_ambilight(f, panel);
            body = top;
        }
        if self.info_shown() {
            let (panel, top) = below(body, self.panel_height());
            self.draw_panel(f, panel);
            body = top;
        }
        body
    }

    /// A wide screen's right-hand column, top to bottom: the D-pad (remote
    /// mode), the preview, the Ambilight and the system panel, each as
    /// tall as it wants while there's room.
    pub(super) fn draw_side(&mut self, f: &mut Frame, side: ratatui::layout::Rect) {
        let mut rest = side;
        let mut take = |want: u16| -> Option<ratatui::layout::Rect> {
            if rest.height < 3 {
                return None;
            }
            let h = want.min(rest.height);
            let [top, more] = Layout::vertical([Constraint::Length(h), Constraint::Min(0)]).areas(rest);
            rest = more;
            Some(top)
        };
        if self.mode == Mode::Remote
            && let Some(r) = take(DPAD_ROWS)
        {
            self.draw_dpad(f, r);
        }
        if self.preview_open {
            // about 16:9 in cells twice as tall as wide, and a border
            let want = (side.width.saturating_sub(2) * 9 / 32 + 3).max(6);
            if let Some(r) = take(want) {
                self.draw_preview(f, r);
            }
        }
        if self.ambi_open
            && self.panel_id().is_some()
            && let Some(r) = take(self.ambilight_height())
        {
            self.draw_ambilight(f, r);
        }
        if self.info_shown()
            && let Some(r) = take(self.panel_height())
        {
            self.draw_panel(f, r);
        }
    }

    /// The remote's keys to click (remote mode, wide screens): arrows and
    /// OK, back, home, and play/pause and volume.
    pub(super) fn draw_dpad(&mut self, f: &mut Frame, area: ratatui::layout::Rect) {
        use Action::{Key, PlayPause, Volume};
        let block = Block::bordered().title(" remote - click, or type ").border_style(Style::new().fg(Color::DarkGray));
        let inner = block.inner(area);
        f.render_widget(block, area);
        // (row, column, label, what it does): columns of 7 cells
        let keys: [(u16, u16, &str, Action); 11] = [
            (0, 1, "  ↑  ", Key(19)),
            (1, 0, "  ←  ", Key(21)),
            (1, 1, " ok  ", Key(KEY_OK)),
            (1, 2, "  →  ", Key(22)),
            (2, 1, "  ↓  ", Key(20)),
            (0, 3, "back ", Key(KEY_BACK)),
            (1, 3, "home ", Key(KEY_HOME)),
            (2, 3, " del ", Key(KEY_DEL)),
            (0, 4, " ⏯   ", PlayPause),
            (1, 4, "vol+ ", Volume(1)),
            (2, 4, "vol- ", Volume(-1)),
        ];
        for (row, col, label, act) in keys {
            let r = ratatui::layout::Rect { x: inner.x + 1 + col * 7, y: inner.y + row, width: 5, height: 1 };
            if r.right() > inner.right() || r.y >= inner.bottom() {
                continue;
            }
            f.render_widget(Paragraph::new(Span::styled(label, Style::new().fg(Color::Black).bg(Color::Gray))), r);
            self.hits.push((r, Hit::Input(Input::Do(act))));
        }
    }

    /// The top line ([`spine`]), each part clickable.
    pub(super) fn draw_spine(&mut self, f: &mut Frame, area: ratatui::layout::Rect) {
        // load and memory only when they're wanted (else they aren't read)
        let system = self.st.snap.as_ref().map(|s| &s.system).filter(|_| self.opt(tveee_core::options::SHOW_LOAD));
        let panel = self.panel_id().map(|_| self.panel_title());
        let mut x = area.x;
        let sep = Span::styled(" │ ", Style::new().fg(Color::DarkGray));
        for (i, (spans, hit)) in spine(system, panel).into_iter().enumerate() {
            if i > 0 {
                f.render_widget(Paragraph::new(Line::from(sep.clone())), ratatui::layout::Rect { x, y: area.y, width: 3.min(area.right().saturating_sub(x)), height: 1 });
                x += 3;
            }
            let line = Line::from(spans);
            let w = u16::try_from(line.width()).unwrap_or(u16::MAX);
            if x + w > area.right() {
                break;
            }
            let r = ratatui::layout::Rect { x, y: area.y, width: w, height: 1 };
            f.render_widget(Paragraph::new(line), r);
            if let Some(h) = hit {
                self.hits.push((r, Hit::Input(h)));
            }
            x += w;
        }
    }

    /// The what / title / progress / url / volume lines, each in its own rows so
    /// the bars' positions are known for clicks.
    pub(super) fn draw_body(&mut self, f: &mut Frame, body: ratatui::layout::Rect) {
        const LABEL: u16 = 9;
        let width = usize::from(body.width);
        let label = |l: &str| Span::styled(format!("{l:<9}"), Style::new().fg(Color::DarkGray));
        // (line, what clicking it does)
        let mut lines: Vec<(Line, Option<Hit>)> = Vec::new();
        // the link's rows: (line, text shown, where it goes), made terminal
        // hyperlinks once drawn
        let mut url_rows: Vec<(usize, String, String)> = Vec::new();
        let settling = self.settling_label();
        // what can be done with it, fitted to one row
        let mut chips = self.chips();
        while !chips.is_empty() && 9 + chips_width(&chips) > width {
            // the app's later actions go before guide and search
            let at = chips.iter().rposition(|(_, _, h)| matches!(h, Hit::Chip(_))).unwrap_or(chips.len() - 1);
            chips.remove(at);
        }
        match &self.st.snap {
            None => lines.push((Line::from("waiting for the TV...").italic(), None)),
            Some(s) => {
                match &s.activity {
                    // still settling: the action's label, not the half-way state
                    _ if settling.is_some() => {
                        let label_text = settling.clone().unwrap_or_default();
                        lines.push((Line::from(vec![label("what"), Span::raw(label_text).italic()]), None));
                    }
                    Some(a) => {
                        let mut what = vec![label("what"), Span::raw(a.what.clone()).bold()];
                        if self.st.seeded {
                            let when = self.st.seen_at.map_or_else(|| "last seen".to_string(), tveee_core::engine::last::ago);
                            what.push(Span::styled(format!("  ({when}; reading the TV...)"), Style::new().fg(Color::DarkGray).italic()));
                        }
                        lines.push((Line::from(what), None));
                        // fields that open something look like links
                        let field = |f: &nowplaying::Field| {
                            let v = Span::raw(f.value.clone());
                            let v = if f.open.is_some() { v.fg(Color::Cyan).underlined() } else { v };
                            (Line::from(vec![label(f.label), v]), f.open.map(Hit::Open))
                        };
                        lines.extend(a.fields.iter().filter(|f| f.place == nowplaying::Place::Top).map(field));
                        if let Some(t) = &a.title {
                            lines.push((Line::from(vec![label("title"), Span::raw(t.clone())]), None));
                        }
                        if let Some(mut p) = a.progress {
                            if !p.paused {
                                p.position += self.st.at.elapsed().as_secs_f64();
                                if let Some(d) = p.duration {
                                    p.position = p.position.min(d);
                                }
                            }
                            // leave room for the label and " 1:02:03 / 1:02:03 (100%)"
                            let w = width.saturating_sub(9 + 26).max(10);
                            // jumping needs a video tveee can seek in (not live TV)
                            let hit = p.duration.filter(|_| s.exact).map(|_| Hit::Progress {
                                x0: body.x + LABEL,
                                width: u16::try_from(w).unwrap_or(u16::MAX),
                            });
                            let bar_line = Line::from(vec![
                                label("progress"),
                                Span::styled(progress_bar(&p, w), Style::new().fg(Color::Cyan)),
                            ]);
                            lines.push((bar_line, hit));
                        }
                        if let Some(u) = &a.url {
                            let u = links::clean(u);
                            let room = width.saturating_sub(9).max(10);
                            let style = Style::new().fg(Color::Cyan).underlined();
                            let toggle = Some(Hit::Input(Input::ToggleUrl));
                            if self.url_full {
                                for (i, row) in links::rows(&u, room).into_iter().enumerate() {
                                    let head = if i == 0 { label("url") } else { Span::raw(" ".repeat(9)) };
                                    url_rows.push((lines.len(), row.clone(), u.clone()));
                                    lines.push((Line::from(vec![head, Span::styled(row, style)]), toggle.clone()));
                                }
                                let button = Style::new().fg(Color::Black).bg(Color::Cyan);
                                url_rows.push((lines.len(), OPEN_BUTTON.to_string(), u.clone()));
                                lines.push((
                                    Line::from(vec![
                                        Span::raw(" ".repeat(9)),
                                        Span::styled(COPY_BUTTON, button),
                                        Span::raw(" "),
                                        Span::styled(OPEN_BUTTON, button),
                                        Span::styled("   u shortens it", Style::new().fg(Color::DarkGray)),
                                    ]),
                                    Some(Hit::UrlButtons),
                                ));
                            } else {
                                let short = links::shorten_middle(&u, room);
                                url_rows.push((lines.len(), short.clone(), u.clone()));
                                lines.push((Line::from(vec![label("url"), Span::styled(short, style)]), toggle));
                            }
                        }
                        lines.extend(a.fields.iter().filter(|f| f.place == nowplaying::Place::Bottom).map(field));
                        if !chips.is_empty() {
                            let mut spans = vec![label("do")];
                            for (i, (key, text, _)) in chips.iter().enumerate() {
                                if i > 0 {
                                    spans.push(Span::raw("  "));
                                }
                                spans.push(Span::styled(format!(" {key} "), Style::new().fg(Color::Black).bg(Color::Gray)));
                                spans.push(Span::styled(format!(" {text}"), Style::new().fg(Color::Gray)));
                            }
                            lines.push((Line::from(spans), Some(Hit::Chips)));
                        }
                    }
                    None => lines.push((Line::from("nothing playing that tveee recognises").italic(), None)),
                }
                if s.standby {
                    lines.push((Line::from("press . then w to wake the TV").fg(Color::Yellow), None));
                }
                lines.push((Line::default(), None));
                let vol_w = width.saturating_sub(9 + 8).clamp(10, 40);
                // this refresh's volume, else the last one: as it is while
                // recent, greyed once it's old; a grey empty bar before any
                let (known, stale) = volume_shown(s.volume, self.st.last_volume.map(|(v, at)| (v, at.elapsed())));
                let vol_hit = Some(Hit::Volume { x0: body.x + LABEL, width: u16::try_from(vol_w).unwrap_or(u16::MAX) });
                let (vol, hit) = match (known, stale, s.muted) {
                    (_, _, Some(true)) => (
                        Span::styled("muted (click to unmute)", Style::new().fg(Color::Yellow)),
                        Some(Hit::Input(Input::Do(Action::Mute))),
                    ),
                    (Some((v, max)), _, _) if max > 0 => (
                        Span::styled(format!("{} {v}/{max}", bar(f64::from(v) / f64::from(max), vol_w)), Style::new().fg(Color::Green)),
                        vol_hit,
                    ),
                    (None, Some((v, max)), _) if max > 0 => (
                        Span::styled(format!("{} {v}/{max}", bar(f64::from(v) / f64::from(max), vol_w)), Style::new().fg(Color::DarkGray)),
                        vol_hit,
                    ),
                    _ => (Span::styled(bar(0.0, vol_w), Style::new().fg(Color::DarkGray)), None),
                };
                lines.push((Line::from(vec![label("volume"), vol]), hit));
            }
        }
        let mut y = body.y;
        for (n, (line, hit)) in lines.into_iter().enumerate() {
            let para = Paragraph::new(line).wrap(Wrap { trim: false });
            let h = u16::try_from(para.line_count(body.width).max(1)).unwrap_or(1);
            if y + h > body.y + body.height {
                break;
            }
            let rect = ratatui::layout::Rect { x: body.x, y, width: body.width, height: h };
            f.render_widget(para, rect);
            for (_, text, url) in url_rows.iter().filter(|(i, _, _)| *i == n) {
                // the open button sits after the copy button
                let x = if text == OPEN_BUTTON { body.x + LABEL + width_of(COPY_BUTTON) + 1 } else { body.x + LABEL };
                self.links.push(links::Drawn { x, y, text: text.clone(), url: url.clone() });
            }
            if hit == Some(Hit::Chips) {
                let mut x = body.x + LABEL;
                for (key, text, h) in &chips {
                    let w = chip_width(key, text);
                    self.hits.push((ratatui::layout::Rect { x, y, width: w, height: 1 }, h.clone()));
                    x += w + 2;
                }
                y += h;
                continue;
            }
            if hit == Some(Hit::UrlButtons) {
                let copy_w = width_of(COPY_BUTTON);
                let copy = ratatui::layout::Rect { x: body.x + LABEL, y, width: copy_w, height: 1 };
                let open = ratatui::layout::Rect { x: copy.x + copy_w + 1, y, width: width_of(OPEN_BUTTON), height: 1 };
                self.hits.push((copy, Hit::Input(Input::CopyUrl)));
                self.hits.push((open, Hit::Input(Input::OpenUrl)));
                y += h;
                continue;
            }
            if let Some(hit) = hit {
                let r = match hit {
                    Hit::Progress { x0, width } | Hit::Volume { x0, width } => {
                        ratatui::layout::Rect { x: x0, y, width, height: 1 }
                    }
                    _ => rect,
                };
                self.hits.push((r, hit));
            }
            y += h;
        }
    }

    /// Things to do with what's playing, as (key, what, click): the
    /// actions of the app in front (Kodi's subtitles and audio, ...; their
    /// key is in the menu, so ".k"), the guide on live TV, and search where
    /// tveee knows how. None in standby, or before the first reading.
    pub(super) fn chips(&self) -> Vec<(String, String, Hit)> {
        let Some(s) = self.st.snap.as_ref().filter(|s| !s.standby && !self.st.seeded) else { return Vec::new() };
        let pkg = s.focus.as_deref().map(|f| f.split('/').next().unwrap_or(f));
        let provider = pkg.and_then(tveee_core::provider::for_package);
        let live = s.kind.as_deref().is_some_and(|k| k.starts_with("live-tv"));
        let mut out = Vec::new();
        if live {
            out.push(("g".to_string(), "guide".to_string(), Hit::Input(Input::OpenGuide)));
        }
        if let Some(p) = provider {
            for (i, a) in self.menu.iter().enumerate() {
                if a.category != p.id() {
                    continue;
                }
                let Some(key) = a.key else { continue };
                // subtitles while music plays: no
                if a.media.is_some() && a.media != s.activity.as_ref().and_then(|x| x.media) {
                    continue;
                }
                let what = a.title.strip_prefix(&format!("{}: ", a.category)).unwrap_or(a.title);
                out.push((format!(".{key}"), what.to_string(), Hit::Chip(i)));
            }
        }
        // music playing in an app with a visualiser (Kodi)
        let music = s.activity.as_ref().and_then(|a| a.media) == Some("audio");
        if music && provider.and_then(|p| p.player()).is_some_and(|p| p.has_visualiser()) {
            out.push(("z".to_string(), "visualiser".to_string(), Hit::Input(Input::ToggleVisualiser)));
        }
        if live || provider.is_some() {
            out.push(("/".to_string(), "search".to_string(), Hit::Input(Input::OpenSearch)));
        }
        out
    }

    pub(super) fn panel_disks(&self) -> Vec<(&'static str, info::Disk)> {
        self.st.snap.as_ref().map_or_else(Vec::new, |s| {
            info::interesting_disks(&s.system.disks).into_iter().map(|(l, d)| (l, d.clone())).collect()
        })
    }

    pub(super) fn panel_height(&self) -> u16 {
        // power, app, memory text, graph (3), swap, disks..., load
        let disks = u16::try_from(self.panel_disks().len()).unwrap_or(0);
        2 + 1 + 1 + 1 + GRAPH_ROWS + 1 + disks + 1
    }

    /// The system panel: power, app, memory graph, swap, storage, load.
    pub(super) fn draw_panel(&self, f: &mut Frame, area: ratatui::layout::Rect) {
        let block = Block::bordered()
            .title(" system - i hides ")
            .border_style(Style::new().fg(Color::DarkGray));
        let inner = block.inner(area);
        f.render_widget(block, area);
        let Some(s) = &self.st.snap else { return };
        let sys = &s.system;
        let label = |l: &str| Span::styled(format!("{l:<9}"), Style::new().fg(Color::DarkGray));
        let width = usize::from(inner.width);
        // in the side column (or a narrow pane): shorter bars and words
        let narrow = width < 60;
        let bar_w = if narrow { 6 } else { width.saturating_sub(9 + 34).clamp(10, 40) };
        let gauge = |frac: f64, text: String| {
            let hot = frac >= 0.9;
            let colour = if hot { Color::Red } else { Color::Green };
            vec![Span::styled(bar(frac, bar_w), Style::new().fg(colour)), Span::raw(format!(" {text}"))]
        };

        let mut rows: Vec<(Line, u16)> = Vec::new();
        let uptime = sys.uptime_secs.map_or(String::new(), |u| format!("   up {}", info::human_uptime(u)));
        rows.push((Line::from(vec![label("power"), Span::raw(format!("{}{uptime}", s.power.as_deref().unwrap_or("?")))]), 1));
        // the kind first: the window name is long and gets cut off
        let kind = s.kind.as_ref().map_or(String::new(), |k| format!("[{k}] "));
        rows.push((
            Line::from(vec![
                label("app"),
                Span::styled(kind, Style::new().fg(Color::Cyan)),
                // narrow: the package (the activity's name says little more)
                Span::raw(s.focus.as_deref().map_or("", |f| if narrow { f.split('/').next().unwrap_or(f) } else { f }).to_string()),
            ]),
            1,
        ));
        match sys.mem {
            Some(m) => {
                let mb = |k: u64| k / 1024;
                let text = if narrow {
                    format!("{}/{} MB used, {} MB free", mb(m.used()), mb(m.total), mb(m.available))
                } else {
                    format!("{} MB used of {} MB ({} MB available)", mb(m.used()), mb(m.total), mb(m.available))
                };
                rows.push((Line::from(vec![label("memory"), Span::raw(text)]), 1));
                rows.push((Line::default(), GRAPH_ROWS)); // the graph goes here
                let swap = if m.swap_total > 0 {
                    gauge(m.swap_used() as f64 / m.swap_total as f64, format!("{}/{} MB", mb(m.swap_used()), mb(m.swap_total)))
                } else {
                    vec![Span::raw("none")]
                };
                rows.push((Line::from([vec![label("swap")], swap].concat()), 1));
            }
            None => rows.push((Line::from(vec![label("memory"), Span::raw("unknown")]), 1)),
        }
        for (name, d) in self.panel_disks() {
            let frac = if d.total_kb > 0 { d.used_kb as f64 / d.total_kb as f64 } else { 0.0 };
            if narrow {
                // the disk's name as the label: "internal  ━━┃─ 3.5/3.6 GB, 101 MB free"
                let used = info::size(d.used_kb);
                let used = used.trim_end_matches(" GB").trim_end_matches(" MB");
                let text = format!("{used}/{}, {} free", info::size(d.total_kb), info::size(d.avail_kb));
                rows.push((Line::from([vec![label(name)], gauge(frac, text)].concat()), 1));
            } else {
                let text = info::disk_summary(name, &d);
                rows.push((Line::from([vec![label("storage")], gauge(frac, text)].concat()), 1));
            }
        }
        if let Some((a, b, c)) = sys.load {
            let cores = sys.cores.map_or(String::new(), |n| format!(" ({n} cores)"));
            rows.push((Line::from(vec![label("load"), Span::raw(format!("{a:.2} {b:.2} {c:.2}{cores}"))]), 1));
        }

        let mut y = inner.y;
        for (line, h) in rows {
            if y + h > inner.y + inner.height {
                break;
            }
            let rect = ratatui::layout::Rect { x: inner.x, y, width: inner.width, height: h };
            if h == GRAPH_ROWS {
                self.draw_mem_graph(f, rect, sys.mem.map_or(0, |m| m.total / 1024));
            } else {
                f.render_widget(Paragraph::new(line), rect);
            }
            y += h;
        }
    }

    /// Memory used over time, newest on the right, scaled to total RAM.
    pub(super) fn draw_mem_graph(&self, f: &mut Frame, rect: ratatui::layout::Rect, total_mb: u64) {
        let x = rect.x + 9;
        let w = rect.width.saturating_sub(9 + 1);
        let n = usize::from(w).min(self.st.mem_history.len());
        let recent = &self.st.mem_history[self.st.mem_history.len() - n..];
        let hot = recent.last().is_some_and(|u| total_mb > 0 && *u * 10 >= total_mb * 9);
        let floor = graph_floor(recent, total_mb);
        let shifted: Vec<u64> = recent.iter().map(|u| u.saturating_sub(floor)).collect();
        let graph = ratatui::widgets::Sparkline::default()
            .data(&shifted)
            .max(total_mb.saturating_sub(floor).max(1))
            .style(Style::new().fg(if hot { Color::Yellow } else { Color::Cyan }));
        f.render_widget(graph, ratatui::layout::Rect { x, y: rect.y, width: w, height: rect.height });
        // the scale on the left: top, bottom (when it isn't 0), how long
        let bottom = rect.height.saturating_sub(1);
        let last = format!("last {}s", recent.len());
        let notes = if floor > 0 && rect.height >= 3 {
            // the scale's ends at its ends, how long between
            vec![(0, format!("{total_mb} MB")), (bottom - 1, last), (bottom, format!("{floor} MB"))]
        } else {
            vec![(0, format!("{total_mb} MB")), (bottom, last)]
        };
        for (dy, note) in notes {
            f.render_widget(
                Paragraph::new(Line::from(note).fg(Color::DarkGray)),
                ratatui::layout::Rect { x: rect.x, y: rect.y + dy, width: 9.min(rect.width), height: 1 },
            );
        }
    }
}

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
            button("←", if exact { "-10s" } else { "rew" }, Do(Seek(-10.0))),
            button("→", if exact { "+10s" } else { "ffw" }, Do(Seek(10.0))),
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
        button("?", "help", ToggleHelp),
        button("q", "quit", Quit),
    ]);
    if !ambilight {
        b.retain(|x| x.label != "ambilight");
    }
    b
}

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
        let places = flow(&bs, area.width);
        // a small pane keeps one row of keys (the rest: the palette, help)
        let rows = if small { 1 } else { places.iter().map(|(r, _)| r + 1).max().unwrap_or(1) };
        let [top, body, _, footer, status] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(4),
            Constraint::Length(u16::from(!small)),
            Constraint::Length(rows),
            Constraint::Length(1),
        ])
        .areas(area);
        self.draw_spine(f, top);

        // the preview: beside the rest when there's room, else below it
        let body = if self.preview_open {
            if body.width >= 90 {
                let w = (body.width / 2).min(64);
                let [left, right] = Layout::horizontal([Constraint::Min(40), Constraint::Length(w)]).areas(body);
                self.draw_preview(f, right);
                left
            } else {
                let h = (body.height / 2).clamp(6, 14);
                let [top, panel] = Layout::vertical([Constraint::Min(4), Constraint::Length(h)]).areas(body);
                self.draw_preview(f, panel);
                top
            }
        } else {
            body
        };
        let body = if self.ambi_open && self.panel_id().is_some() {
            let want = self.ambilight_height();
            let h = want.min(body.height.saturating_sub(4));
            let [top, panel] = Layout::vertical([Constraint::Min(4), Constraint::Length(h)]).areas(body);
            self.draw_ambilight(f, panel);
            top
        } else {
            body
        };
        let body = if self.info {
            let want = self.panel_height();
            // keep at least 4 rows for what's playing
            let h = want.min(body.height.saturating_sub(4));
            let [top, panel] = Layout::vertical([Constraint::Min(4), Constraint::Length(h)]).areas(body);
            self.draw_panel(f, panel);
            top
        } else {
            body
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

    /// The top line ([`spine`]), each part clickable.
    pub(super) fn draw_spine(&mut self, f: &mut Frame, area: ratatui::layout::Rect) {
        let system = self.st.snap.as_ref().map(|s| &s.system);
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
                            what.push(Span::styled("  (last seen; waiting for the TV...)", Style::new().fg(Color::DarkGray).italic()));
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
        let bar_w = width.saturating_sub(9 + 34).clamp(10, 40);
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
                Span::raw(s.focus.clone().unwrap_or_default()),
            ]),
            1,
        ));
        match sys.mem {
            Some(m) => {
                let mb = |k: u64| k / 1024;
                let text = format!(
                    "{} MB used of {} MB ({} MB available)",
                    mb(m.used()),
                    mb(m.total),
                    mb(m.available)
                );
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
            let text = info::disk_summary(name, &d);
            rows.push((Line::from([vec![label("storage")], gauge(frac, text)].concat()), 1));
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
        let graph = ratatui::widgets::Sparkline::default()
            .data(recent)
            .max(total_mb.max(1))
            .style(Style::new().fg(if hot { Color::Yellow } else { Color::Cyan }));
        f.render_widget(graph, ratatui::layout::Rect { x, y: rect.y, width: w, height: rect.height });
        let note = format!("last {}s", recent.len());
        f.render_widget(
            Paragraph::new(Line::from(note).fg(Color::DarkGray)),
            ratatui::layout::Rect { x: rect.x, y: rect.y + rect.height - 1, width: 9.min(rect.width), height: 1 },
        );
    }
}

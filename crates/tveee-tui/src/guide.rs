//! The live TV guide: its rows, the timeline grid, the details panel, and
//! the guide pop-up's keys and drawing.

use super::*;

/// A run of the guide grid: a programme's edge, its start time, its title,
/// or empty time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seg {
    Edge,
    Time,
    Title,
    Gap,
}

/// One channel's row of the timeline grid: `width` cells from `start`,
/// `spc` seconds a cell. A programme is `│Title` over the cells its time
/// covers (`│HH:MM Title` if it starts off the ruler's half hours); one that started before the left edge shows `◂Title`. Runs
/// of the programme starting at `focus` are flagged (to draw it marked).
pub fn grid_row(events: &[tveee_core::provider::Event], start: i64, spc: i64, width: usize, offset: i64, focus: Option<i64>) -> Vec<(Seg, String, bool)> {
    let mut buf = vec![(' ', Seg::Gap, false); width];
    let end = start + spc * width as i64;
    let cell = |t: i64| (t - start).div_euclid(spc).clamp(0, width as i64) as usize;
    for e in events.iter().filter(|e| e.end > start && e.start < end) {
        let (x0, x1) = (cell(e.start), cell(e.end));
        if x1 <= x0 {
            continue;
        }
        let mut content: Vec<(char, Seg)> = Vec::new();
        if e.start >= start {
            content.push(('│', Seg::Edge));
            // the ruler says the time on the hour and half hour; and a
            // narrow block keeps its room for the title
            if e.start % GUIDE_STEP != 0 && x1 - x0 >= MIN_FOR_TIME {
                content.extend(format!("{} ", tveee_core::builtin::guide::hhmm(e.start, offset)).chars().map(|c| (c, Seg::Time)));
            }
        } else {
            content.push(('◂', Seg::Edge));
        }
        content.extend(e.title.chars().map(|c| (c, Seg::Title)));
        let w = x1 - x0;
        if content.len() > w && w > 2 {
            content.truncate(w - 1);
            content.push(('…', Seg::Title));
        }
        let marked = focus == Some(e.start);
        for (i, (c, seg)) in content.into_iter().take(w).enumerate() {
            buf[x0 + i] = (c, seg, marked);
        }
        // the whole block, spaces too
        if marked {
            for cell in &mut buf[x0..x1] {
                cell.2 = true;
            }
        }
    }
    let mut out: Vec<(Seg, String, bool)> = Vec::new();
    for (c, seg, marked) in buf {
        match out.last_mut() {
            Some((s, text, m)) if *s == seg && *m == marked => text.push(c),
            _ => out.push((seg, c.to_string(), marked)),
        }
    }
    out
}

/// What clicking part of the details panel does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailsClick {
    Open,
    Focus,
    Unfocus,
    Hide,
}

/// The details panel under the guide: hidden (one line), open, or open
/// with the keyboard in it (to scroll).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Details {
    Closed,
    Open,
    Focused,
}

/// Lines in the open details panel, at most and at least.
pub(super) const DETAILS_ROWS: usize = 7;
pub(super) const DETAILS_MIN: usize = 3;

/// The guide's key help (under 80 columns), and the details panel's.
pub(super) const GUIDE_KEYS: &str = "←→ ½ hour  tab tabs  ⏎ watch  * favourite  ⇧tab details   F2 apps  F3 remote";
pub(super) const GUIDE_KEYS_DETAILS: &str = "details: ⇧tab back to the guide  h hide";

/// Freeview's tags at the end of a description ("... [S,AD]"), as words.
const DVB_TAGS: [(&str, &str); 4] = [("S", "subtitles"), ("AD", "audio description"), ("SL", "signed"), ("HD", "HD")];

/// A description without the access tags at its end, and what they say.
/// Unknown tags stay in the text.
pub fn split_tags(text: &str) -> (&str, Vec<&'static str>) {
    let mut rest = text.trim_end();
    let mut found: Vec<&'static str> = Vec::new();
    while let Some(open) = rest.strip_suffix(']').and_then(|r| r.rfind('[')) {
        let inner = &rest[open + 1..rest.len() - 1];
        let words: Option<Vec<&'static str>> = inner.split(',').map(|t| DVB_TAGS.iter().find(|(k, _)| *k == t.trim()).map(|(_, w)| *w)).collect();
        let Some(words) = words else { break };
        for w in words.into_iter().rev() {
            if !found.contains(&w) {
                found.insert(0, w);
            }
        }
        rest = rest[..open].trim_end();
    }
    (rest, found)
}

/// The details panel's text for programme `e` on channel `ch`, wrapped to
/// `width`: title, then when/how long/channel/genre, then the description.
/// `info`: None while it's being read, Some(None) if the guide has none.
pub fn details_text(
    e: &tveee_core::provider::Event,
    ch: &tveee_core::provider::Channel,
    info: Option<Option<&str>>,
    width: usize,
    offset: i64,
) -> Vec<(String, Seg)> {
    use tveee_core::builtin::guide::{day_hhmm, hhmm};
    let mins = (e.end - e.start) / 60;
    let length = if mins >= 60 { format!("{}h{:02}", mins / 60, mins % 60) } else { format!("{mins} min") };
    let (info, tags) = match info {
        Some(Some(i)) => {
            let (text, tags) = split_tags(i);
            (Some(Some(text)), tags)
        }
        other => (other, Vec::new()),
    };
    let mut meta = vec![format!("{}–{} ({length})", day_hhmm(e.start, offset), hhmm(e.end, offset)), ch.label()];
    meta.extend(e.genre_name().map(str::to_string));
    meta.extend(tags.into_iter().map(str::to_string));
    let mut out = vec![(e.title.clone(), Seg::Title), (meta.join(" · "), Seg::Time), (String::new(), Seg::Gap)];
    match info {
        Some(Some(info)) => {
            for para in info.split('\n') {
                out.extend(textwrap(para, width).into_iter().map(|l| (l, Seg::Gap)));
            }
        }
        Some(None) => out.push(("(no description in the broadcast guide)".into(), Seg::Time)),
        // still being read: empty, so the text just appears (the header says it's coming)
        None => {}
    }
    out
}

/// Cells a programme needs before its start time is shown: "│HH:MM " and a
/// few letters of title.
const MIN_FOR_TIME: usize = 11;

/// The time ruler over the grid: half-hour labels, and ▼ at `now`.
pub fn grid_ruler(start: i64, spc: i64, width: usize, now: i64, offset: i64) -> (String, Option<usize>) {
    let mut buf = vec![' '; width];
    let mut t = (start + GUIDE_STEP - 1).div_euclid(GUIDE_STEP) * GUIDE_STEP;
    while t < start + spc * width as i64 {
        let x = ((t - start) / spc) as usize;
        for (i, c) in tveee_core::builtin::guide::hhmm(t, offset).chars().enumerate() {
            if x + i < width {
                buf[x + i] = c;
            }
        }
        t += GUIDE_STEP;
    }
    let marker = (now >= start && now < start + spc * width as i64).then(|| ((now - start) / spc) as usize);
    // not over a time: just before it instead, else just after it
    let marker = marker.and_then(|m| match buf.get(m) {
        Some(' ') | None => Some(m),
        Some(_) => {
            let first = (0..=m).rev().take_while(|&i| buf[i] != ' ').last().unwrap_or(m);
            let last = (m..width).take_while(|&i| buf[i] != ' ').last().unwrap_or(m);
            first.checked_sub(1).or(Some(last + 1)).filter(|&i| i < width && buf[i] == ' ')
        }
    });
    (buf.into_iter().collect(), marker)
}

/// Seconds a grid cell stands for, so about two hours fit.
pub fn grid_scale(width: usize) -> i64 {
    (7200 / width.max(1) as i64).clamp(40, 120)
}

/// Where the highlight goes on switching to a tab showing `ids`: it stays
/// on channel `at` if it's there, else the channel that's on, else the
/// first.
pub fn guide_tab_pick(ids: &[u32], at: u32, current: Option<u32>) -> u32 {
    [Some(at), current].into_iter().flatten().find(|c| ids.contains(c)).or(ids.first().copied()).unwrap_or(at)
}

/// Where a movement key takes the highlight among `ids` (shown channel ids),
/// from channel `at` (if it's not shown, the first one).
pub fn guide_move(ids: &[u32], at: u32, code: KeyCode) -> u32 {
    let n = ids.len();
    let Some(i) = ids.iter().position(|&c| c == at) else { return ids.first().copied().unwrap_or(at) };
    ids[super::listnav::step(i, n, code).unwrap_or(i)]
}

impl App {
    /// The guide's rows, once fetched.
    pub(super) fn guide_rows(&self) -> &[GuideRow] {
        self.st.guide_ok().map_or(&[][..], |g| &g.rows[..])
    }

    /// Open the guide on `channel`, else the channel that's on. It's
    /// fetched afresh each time (what's on moves on); the last copy shows
    /// meanwhile.
    pub(super) fn open_guide(&mut self, channel: Option<u32>, actions: &Handle) {
        if live_provider().is_none() {
            self.note = "no live TV guide on this TV".into();
            return;
        }
        let on_now = self.st.snap.as_ref().and_then(|s| s.activity.as_ref()).and_then(|a| a.field("channel")).and_then(|f| match f.open {
            Some(nowplaying::Open::Guide { channel }) => channel,
            None => None,
        });
        self.prompt.clear();
        self.guide_later = 0;
        self.guide_view = self.guide_start();
        self.overlay = Overlay::Guide { tab: GuideTab::All, at: channel.or(on_now).unwrap_or(0) };
        let _ = actions.send(Action::LoadGuide);
        // the details panel's descriptions take the TV a while the first time
        let _ = actions.send(Action::PrefetchInfo);
    }

    /// Where the grid's left edge should be: the half hour before now, plus
    /// the steps taken.
    pub(super) fn guide_start(&self) -> i64 {
        tveee_core::builtin::guide::now().div_euclid(GUIDE_STEP) * GUIDE_STEP + self.guide_later * GUIDE_STEP
    }

    /// Whether the guide is sliding (so frames should come quickly).
    pub(super) fn animating(&self) -> bool {
        matches!(self.overlay, Overlay::Guide { .. }) && self.guide_view != self.guide_start()
    }

    /// Move the drawn grid toward where it should be: one cell every 10ms
    /// with scroll animations on, else straight there.
    pub(super) fn slide(&mut self, spc: i64) {
        let target = self.guide_start();
        if !tveee_core::options::get(tveee_core::options::SCROLL_ANIMATION) {
            self.guide_view = target;
            return;
        }
        let cells = (self.guide_slid.elapsed().as_millis() / 10).max(1) as i64;
        self.guide_slid = Instant::now();
        let step = (cells * spc).min((target - self.guide_view).abs());
        self.guide_view += step * (target - self.guide_view).signum();
    }

    /// The time the guide shows.
    pub(super) fn guide_time(&self) -> i64 {
        guide_time_at(tveee_core::builtin::guide::now(), self.guide_later)
    }

    /// Keys in the guide: typing filters, ↑/↓/page keys/home/end move, ←/→
    /// go earlier/later in time, Tab switches tabs, Enter watches, Esc closes.
    pub(super) fn guide_key(&mut self, tab: GuideTab, at: u32, code: KeyCode, mods: KeyModifiers, actions: &Handle) {
        // keys for the details panel while it has the keyboard
        if self.details == Details::Focused {
            match code {
                KeyCode::Esc | KeyCode::BackTab | KeyCode::Tab => self.details = Details::Open,
                KeyCode::Backspace | KeyCode::Char('h') => self.details = Details::Closed,
                KeyCode::Up => self.details_scroll = self.details_scroll.saturating_sub(1),
                KeyCode::Down => self.details_scroll += 1,
                KeyCode::PageUp => self.details_scroll = self.details_scroll.saturating_sub(DETAILS_ROWS),
                KeyCode::PageDown => self.details_scroll += DETAILS_ROWS,
                KeyCode::Home => self.details_scroll = 0,
                KeyCode::End => self.details_scroll = usize::MAX / 2,
                _ => {}
            }
            return;
        }
        match code {
            // shift+tab: into the details panel (opening it)
            KeyCode::BackTab => {
                self.details = Details::Focused;
                return;
            }
            KeyCode::Esc => {
                self.overlay = Overlay::None;
                return;
            }
            KeyCode::Char('c') if mods.contains(KeyModifiers::CONTROL) => {
                self.overlay = Overlay::None;
                return;
            }
            // the commands, from here too (":" is filter text: programme
            // names have colons)
            KeyCode::Char('p') if mods.contains(KeyModifiers::CONTROL) => {
                self.overlay = Overlay::None;
                self.open_palette();
                return;
            }
            KeyCode::Enter => {
                self.watch_channel(at, actions);
                return;
            }
            // * isn't in any channel name, so it can't be part of a filter
            KeyCode::Char('*') => {
                self.toggle_favourite(at, actions);
                return;
            }
            KeyCode::Tab => {
                self.guide_tab(tab.next(), at);
                return;
            }
            KeyCode::Right => self.guide_later = (self.guide_later + 1).min(GUIDE_AHEAD / GUIDE_STEP),
            KeyCode::Left => self.guide_later = self.guide_later.saturating_sub(1).max(0),
            KeyCode::Backspace => {
                self.prompt.pop();
            }
            // ctrl+u clears the filter (as in the palette); other ctrl keys
            // aren't letters to filter by
            KeyCode::Char('u') if mods.contains(KeyModifiers::CONTROL) => self.prompt.clear(),
            KeyCode::Char(c) if !mods.contains(KeyModifiers::CONTROL) => self.prompt.push(c),
            _ => {}
        }
        let ids: Vec<u32> = guide_shown(self.guide_rows(), tab, &self.prompt, self.guide_time()).iter().map(|r| r.channel.id).collect();
        // keep the highlight on its channel while it's shown; else the first
        self.overlay = Overlay::Guide { tab, at: guide_move(&ids, at, code) };
    }

    /// Switch the guide to `tab`, the highlight moving to a channel it shows.
    pub(super) fn guide_tab(&mut self, tab: GuideTab, at: u32) {
        let ids: Vec<u32> = guide_shown(self.guide_rows(), tab, &self.prompt, self.guide_time()).iter().map(|r| r.channel.id).collect();
        let current = self.st.guide_ok().and_then(|g| g.current);
        self.details_scroll = 0;
        self.overlay = Overlay::Guide { tab, at: guide_tab_pick(&ids, at, current) };
    }

    /// Star or unstar channel `id`: the star changes now, and the guide is
    /// read back from the TV once it's done (so a refusal puts it back).
    pub(super) fn toggle_favourite(&mut self, id: u32, actions: &Handle) {
        // the engine stars it at once (see `Handle::send`)
        let Some(row) = self.guide_rows().iter().find(|r| r.channel.id == id) else { return };
        let (ch, on) = (row.channel.clone(), !row.channel.favourite);
        self.note = format!("{} {}...", if on { "adding to favourites:" } else { "removing from favourites:" }, ch.label());
        let _ = actions.send(Action::Favourite(ch, on));
    }

    /// Switch to channel `id` and close the guide.
    pub(super) fn watch_channel(&mut self, id: u32, actions: &Handle) {
        let ch = self.guide_rows().iter().find(|r| r.channel.id == id).map(|r| r.channel.clone());
        self.overlay = Overlay::None;
        if let Some(ch) = ch {
            self.note = format!("switching to {}...", ch.label());
            self.settle(self.note.clone(), None);
            let _ = actions.send(Action::Watch(ch));
        }
    }

    pub(super) fn draw_guide(&mut self, f: &mut Frame, tab: GuideTab, at: u32) {
        let area = f.area();
        // the text, and the channel each line shows (for clicks)
        let mut text: Vec<Line> = Vec::new();
        let mut ids: Vec<Option<u32>> = Vec::new();
        let mut tabs: Vec<(GuideTab, String)> = Vec::new();
        // "12 of 191", top right
        let mut counter = String::new();
        // (text row, x within the box, width, what clicking does) for the details panel
        let mut details_hits: Vec<(usize, u16, u16, DetailsClick)> = Vec::new();
        let total = usize::from(area.width.saturating_sub(6)).clamp(50, 140);
        // the timeline grid, right of the channel names
        let ch_w = 24;
        let grid_w = total.saturating_sub(ch_w + 4).max(10);
        let spc = grid_scale(grid_w);
        if self.guide_slid.elapsed() > Duration::from_millis(200) {
            // not sliding lately: a slide starts from now
            self.guide_slid = Instant::now() - Duration::from_millis(10);
        }
        self.slide(spc);
        let title = match self.st.guide.as_deref() {
            None => {
                text.push(Line::from("reading the guide (the first time takes a few seconds)...").italic());
                " guide ".to_string()
            }
            Some(Err(e)) => {
                text.extend(textwrap(&guide_failed(e), total).into_iter().map(|l| Line::from(l).fg(Color::Yellow)));
                let hint = "esc, g: try again  ·  details in the log (: log)";
                text.extend(textwrap(hint, total).into_iter().map(|l| Line::from(l).fg(Color::DarkGray)));
                " guide ".to_string()
            }
            Some(Ok(g)) => {
                let off = tveee_core::builtin::guide::utc_offset();
                let fit = |s: &str, w: usize| -> String {
                    if s.chars().count() <= w { format!("{s:<w$}") } else { format!("{}…", s.chars().take(w.saturating_sub(1)).collect::<String>()) }
                };
                // tabs, with counts
                for t in GuideTab::ALL {
                    let n = guide_shown(&g.rows, t, "", 0).len();
                    let name = t.label();
                    tabs.push((t, format!(" {name} ({n}) ")));
                }
                text.push(Line::from(
                    tabs.iter()
                        .flat_map(|(t, l)| {
                            let st = if *t == tab { Style::new().fg(Color::Black).bg(Color::Magenta).bold() } else { Style::new().fg(Color::Gray) };
                            [Span::styled(l.clone(), st), Span::raw(" ")]
                        })
                        .collect::<Vec<_>>(),
                ));
                ids.push(None);
                let filter = if self.prompt.is_empty() {
                    Line::from("type to filter by number, name or programme").fg(Color::DarkGray)
                } else {
                    Line::from(vec![Span::raw("filter: "), Span::raw(self.prompt.clone()).bold(), Span::raw("▏")])
                };
                text.push(filter);
                ids.push(None);
                let view = self.guide_view;
                let t = self.guide_time();
                let dim = Style::new().fg(Color::DarkGray);
                let (ruler, marker) = grid_ruler(view, spc, grid_w, tveee_core::builtin::guide::now(), off);
                let mut head = vec![Span::styled(
                    format!("  {}  ", fit(&format!("channel   {}", tveee_core::builtin::guide::day_hhmm(view, off).split(' ').next().unwrap_or("")), ch_w)),
                    dim,
                )];
                match marker {
                    Some(m) => {
                        let r: Vec<char> = ruler.chars().collect();
                        head.push(Span::styled(r[..m].iter().collect::<String>(), dim));
                        head.push(Span::styled("▼", Style::new().fg(Color::Magenta).bold()));
                        head.push(Span::styled(r[m + 1..].iter().collect::<String>(), dim));
                    }
                    None => head.push(Span::styled(ruler, dim)),
                }
                text.push(Line::from(head));
                ids.push(None);
                let shown = guide_shown(&g.rows, tab, &self.prompt, t);
                let sel = shown.iter().position(|r| r.channel.id == at).unwrap_or(0);
                // the details panel: the highlighted channel's programme at the guide's time
                let picked = shown.get(sel).and_then(|r| {
                    let (on, after) = r.at(t);
                    on.or(after).map(|e| (e.clone(), r.channel.clone()))
                });
                let key = picked.as_ref().map(|(e, ch)| (ch.id, e.start));
                if key != self.details_of {
                    self.details_of = key;
                    self.details_scroll = 0;
                }
                // its lines, and whether the description is still being read
                let details = picked.as_ref().map(|(e, ch)| {
                    let key = (ch.id, e.start);
                    let info = match (e.info.as_deref(), self.st.infos.get(&key)) {
                        (Some(i), _) => Some(Some(i)),
                        (None, Some(i)) => Some(i.as_deref()),
                        (None, None) => {
                            if self.infos_asked.insert(key) {
                                self.want_info = Some(key);
                            }
                            None
                        }
                    };
                    (details_text(e, ch, info, total.saturating_sub(2), off), info.is_none())
                });
                // as tall as what it says, within limits; it takes its rows from the list
                let rows = details.as_ref().map_or(DETAILS_MIN, |(all, _)| all.len().clamp(DETAILS_MIN, DETAILS_ROWS));
                let panel_h = if self.details == Details::Closed { 1 } else { rows + 1 };
                let room = usize::from(area.height.saturating_sub(8)).saturating_sub(panel_h + 1).max(3);
                if shown.is_empty() {
                    let why = match (tab, self.prompt.is_empty()) {
                        (GuideTab::Favourites, true) => "no favourites yet: add channels to a favourites list on the TV".to_string(),
                        (GuideTab::Radio, true) => "no radio stations in the TV's channel list".to_string(),
                        _ => format!("nothing matches \"{}\"", self.prompt),
                    };
                    text.push(Line::from(why).italic());
                    ids.push(None);
                }
                // sub-headers from the country's lineup (Entertainment, HD, ...)
                let groups: Vec<Option<&str>> = shown.iter().map(|r| g.lineup.map(|l| l.group(r.channel.order().0))).collect();
                for item in guide_items(&groups, sel, room) {
                    let i = match item {
                        GuideItem::Header(name) => {
                            let rule = "─".repeat(total.saturating_sub(name.chars().count() + 4));
                            text.push(Line::from(vec![Span::styled(format!(" ─ {name} "), Style::new().fg(Color::Magenta).bold()), Span::styled(rule, dim)]));
                            ids.push(None);
                            continue;
                        }
                        GuideItem::Row(i) => i,
                    };
                    let r = shown[i];
                    let mark = if g.current == Some(r.channel.id) { "▶" } else { " " };
                    let star = if r.channel.favourite { "★" } else { "☆" };
                    // not on the TV yet: tveee will send it when it can
                    let wait = if g.pending.contains_key(&r.channel.id) { " ·" } else { "" };
                    // "(radio)" tells them apart in the other tabs; not needed in Radio
                    let radio = if r.channel.radio && tab != GuideTab::Radio { " (radio)" } else { "" };
                    let name = format!("{star} {}{radio}{wait}", r.channel.label());
                    let mut spans = vec![Span::raw(format!("{mark}{}  ", fit(&name, ch_w + 1)))];
                    // times and edges dimmed, so the titles stand out (not on the highlighted row)
                    // on the highlighted row, the programme the details are about is marked
                    let focus = (i == sel).then(|| r.at(t)).and_then(|(on, after)| on.or(after)).map(|e| e.start);
                    for (seg, run, marked) in grid_row(&r.events, view, spc, grid_w, off, focus) {
                        let span = match seg {
                            Seg::Edge | Seg::Time if i != sel => Span::styled(run, dim),
                            _ => Span::raw(run),
                        };
                        // marked: drawn the other way round, within the reversed row
                        spans.push(if marked { span.style(Style::new().fg(Color::Magenta).bg(Color::Black).bold()) } else { span });
                    }
                    let line = Line::from(spans);
                    text.push(if i == sel { line.reversed() } else { line });
                    ids.push(Some(r.channel.id));
                }
                // keep the box the same height while filtering
                while text.len() < room + 3 {
                    text.push(Line::default());
                    ids.push(None);
                }
                let panel_top = text.len();
                let focused = self.details == Details::Focused;
                let bar_style = if focused { Style::new().fg(Color::Black).bg(Color::Magenta).bold() } else { Style::new().fg(Color::Magenta) };
                match (self.details, &details) {
                    (Details::Closed, _) => {
                        let what = picked.as_ref().map_or(String::new(), |(e, _)| format!(": {}", e.title));
                        text.push(Line::from(vec![Span::styled("▸ details", bar_style), Span::styled(format!("{what}   (shift+tab or click to open)"), dim)]));
                        details_hits.push((panel_top, 0, total as u16, DetailsClick::Open));
                    }
                    (_, None) => {
                        text.push(Line::from(vec![Span::styled("▾ details", bar_style), Span::styled("  nothing on then", dim)]));
                        details_hits.push((panel_top, 0, total as u16, DetailsClick::Hide));
                        // the same height as with something in it
                        for k in 0..rows {
                            text.push(Line::default());
                            details_hits.push((panel_top + 1 + k, 0, total as u16, DetailsClick::Focus));
                        }
                    }
                    (_, Some((all, reading))) => {
                        self.details_scroll = self.details_scroll.min(all.len().saturating_sub(rows));
                        let more = if *reading {
                            "  reading descriptions from the TV...".to_string()
                        } else if all.len() > DETAILS_ROWS {
                            format!("  {}-{} of {} lines", self.details_scroll + 1, (self.details_scroll + DETAILS_ROWS).min(all.len()), all.len())
                        } else {
                            String::new()
                        };
                        let head = "▾ details ";
                        let (focus_b, hide_b) = (if focused { "[back to the guide]" } else { "[focus: scroll]" }, "[hide]");
                        text.push(Line::from(vec![
                            Span::styled(head, bar_style),
                            Span::raw(" "),
                            Span::styled(focus_b, Style::new().fg(Color::Cyan)),
                            Span::raw(" "),
                            Span::styled(hide_b, Style::new().fg(Color::Cyan)),
                            Span::styled(more, dim),
                        ]));
                        let fx = (head.chars().count() + 1) as u16;
                        details_hits.push((panel_top, fx, focus_b.chars().count() as u16, if focused { DetailsClick::Unfocus } else { DetailsClick::Focus }));
                        details_hits.push((panel_top, fx + focus_b.chars().count() as u16 + 1, hide_b.len() as u16, DetailsClick::Hide));
                        for (k, (line, seg)) in all.iter().skip(self.details_scroll).take(rows).enumerate() {
                            text.push(match seg {
                                Seg::Title => Line::from(format!(" {line}")).bold(),
                                Seg::Time => Line::from(format!(" {line}")).fg(Color::DarkGray),
                                _ => Line::from(format!(" {line}")),
                            });
                            details_hits.push((panel_top + 1 + k, 0, total as u16, DetailsClick::Focus));
                        }
                        for k in all.len().saturating_sub(self.details_scroll)..rows {
                            text.push(Line::default());
                            details_hits.push((panel_top + 1 + k, 0, total as u16, DetailsClick::Focus));
                        }
                    }
                }
                while ids.len() < text.len() {
                    ids.push(None);
                }
                text.push(Line::default());
                if !shown.is_empty() {
                    counter = format!(" {} of {} ", sel + 1, shown.len());
                }
                let keys = if focused { GUIDE_KEYS_DETAILS } else { GUIDE_KEYS };
                text.push(Line::from(keys).fg(Color::DarkGray));
                let refreshing = if self.st.guide_loading { " (refreshing…)" } else { "" };
                format!(" guide: {}{refreshing} ", g.place.as_deref().unwrap_or(g.provider))
            }
        };
        let r = popup_titled(f, &title, &counter, text, Color::Magenta);
        self.hits.push((r, Hit::Popup));
        for (row, id) in ids.into_iter().enumerate() {
            if let Some(id) = id {
                let y = r.y + 1 + u16::try_from(row).unwrap_or(0);
                self.hits.push((ratatui::layout::Rect { x: r.x + 1, y, width: r.width.saturating_sub(2), height: 1 }, Hit::GuideRow(id)));
                // the star (after the ▶ column), with a cell either side to be easy to hit
                self.hits.push((ratatui::layout::Rect { x: r.x + 1, y, width: 4.min(r.width), height: 1 }, Hit::GuideStar(id)));
            }
        }
        for (row, x, w, click) in details_hits {
            let y = r.y + 1 + u16::try_from(row).unwrap_or(0);
            self.hits.push((ratatui::layout::Rect { x: r.x + 1 + x, y, width: w.min(r.width.saturating_sub(2)), height: 1 }, Hit::Details(click)));
        }
        // tab labels on the first row
        let mut x = r.x + 1;
        for (t, l) in &tabs {
            let w = u16::try_from(l.chars().count()).unwrap_or(0);
            self.hits.push((ratatui::layout::Rect { x, y: r.y + 1, width: w, height: 1 }, Hit::GuideTab(*t)));
            x += w + 1;
        }
    }
}

/// Why the guide couldn't be read, in a line: a connection error is said
/// plainly (curl's own words go to the log).
pub(super) fn guide_failed(e: &str) -> String {
    let low = e.to_lowercase();
    let unreachable = ["failed to connect", "could not connect", "connection refused", "couldn't connect", "no route to host"];
    if unreachable.iter().any(|s| low.contains(s)) {
        "couldn't read the guide: the TV didn't answer (it may be busy or still starting)".into()
    } else if low.contains("timed out") || low.contains("timeout") || low.contains("didn't answer within") {
        "couldn't read the guide: the TV took too long to answer (it may be busy)".into()
    } else {
        format!("couldn't read the guide: {e}")
    }
}

#[cfg(test)]
mod failed_tests {
    use super::guide_failed;

    #[test]
    fn connection_errors_are_said_plainly() {
        let curl = "https https://192.168.1.19:1926/6/channeldb/tv/channelLists/all: curl: (7) Failed to connect to 192.168.1.19 port 1926 after 26 ms: Could not connect to server";
        let s = guide_failed(curl);
        assert!(s.contains("didn't answer") && !s.contains("curl"), "{s}");
        assert!(guide_failed("curl: (28) Operation timed out after 10001 milliseconds").contains("too long"));
        assert_eq!(guide_failed("no live TV guide on this TV"), "couldn't read the guide: no live TV guide on this TV");
    }
}

//! The list pane (`t`): the list the app in front shows (Kodi's folder, an
//! add-on's listing), one column, like the guide without its times: type
//! to filter, the usual list keys, ⏎ to choose.
//!
//! The items are read when it opens, and again only when the list itself
//! changes (the refresh reads where the app's list is: which list, and its
//! highlighted row) or after choosing (the engine reads it again then).
//! Kodi's highlight is followed while it moves.

use super::*;
use tveee_core::provider::{ContextList, ListPlace};

/// A step of the trail to the list shown (the breadcrumbs): its title, its
/// key, and how to go back to it.
pub use tveee_core::provider::ListCrumb as Crumb;

/// The breadcrumbs for list `l` arriving: the steps its app names from
/// where it is (a share's path: every step), then the trail's own steps
/// below the last of those (the library's albums, gone through here), or
/// "…" for the steps between that weren't seen; then `l` itself.
pub fn breadcrumbs(trail: &[Crumb], l: &ContextList) -> Vec<Crumb> {
    let t = next_trail(trail, l);
    if l.crumbs.is_empty() {
        return t;
    }
    let here = t.last().cloned().unwrap_or(Crumb { title: l.title.clone(), key: l.key.clone(), go: l.reopen.clone() });
    let mut out = l.crumbs.clone();
    if !l.crumbs_whole {
        let last = l.crumbs.last().map(|c| c.key.clone()).unwrap_or_default();
        match t.iter().position(|c| c.key == last) {
            Some(i) => out.extend(t[i + 1..t.len().saturating_sub(1)].iter().cloned()),
            None => out.push(Crumb { title: "…".into(), key: String::new(), go: None }),
        }
    }
    out.push(here);
    out
}

/// The trail after list `l` arrives: back along it if `l` is on it already;
/// one step on if `l` is inside its last list (or any on it); else a new
/// trail, starting from where `l` is (its way up, if it has one).
pub fn next_trail(trail: &[Crumb], l: &ContextList) -> Vec<Crumb> {
    let here = Crumb { title: l.title.clone(), key: l.key.clone(), go: l.reopen.clone() };
    let mut out: Vec<Crumb> = trail.to_vec();
    if let Some(i) = out.iter().position(|c| c.key == l.key) {
        out.truncate(i + 1);
        out[i].title.clone_from(&l.title);
        return out;
    }
    match &l.up {
        Some((up, go)) => match out.iter().position(|c| c.key == *up) {
            Some(i) => {
                out.truncate(i + 1);
                out.push(here);
                out
            }
            None => vec![Crumb { title: "‹ up".into(), key: up.clone(), go: Some(go.clone()) }, here],
        },
        None => vec![here],
    }
}

/// Whether `label` has every word of `filter` (any case).
pub fn list_matches(label: &str, filter: &str) -> bool {
    let label = label.to_lowercase();
    filter.split_whitespace().all(|w| label.contains(&w.to_lowercase()))
}

impl App {
    /// The list as last read, if it's one.
    pub(super) fn list_ok(&self) -> Option<&ContextList> {
        self.st.list.as_deref().and_then(|l| l.as_ref().ok()).and_then(Option::as_ref)
    }

    /// The items the filter lets through, as indexes into the list.
    pub(super) fn list_shown(&self) -> Vec<usize> {
        self.list_ok().map_or_else(Vec::new, |l| l.items.iter().enumerate().filter(|(_, i)| list_matches(&i.label, &self.prompt)).map(|(n, _)| n).collect())
    }

    /// Open the list pane: read the list, and have the refresh say where
    /// it is while it's open.
    pub(super) fn open_list(&mut self, actions: &Handle) {
        self.prompt.clear();
        self.list_seen = None;
        let at = self.list_ok().and_then(|l| l.at.or(l.playing)).unwrap_or(0);
        self.overlay = Overlay::List { sel: at };
        let _ = actions.send(Action::WatchList(true));
        let _ = actions.send(Action::LoadList);
        self.list_watching = true;
    }

    pub(super) fn close_list(&mut self, actions: &Handle) {
        self.overlay = Overlay::None;
        self.prompt.clear();
        self.location = None;
        if self.list_watching {
            let _ = actions.send(Action::WatchList(false));
            self.list_watching = false;
        }
    }

    /// Keys in the list pane: the list keys move (through what the filter
    /// shows), ⏎ chooses, typing filters, Esc closes.
    pub(super) fn list_key(&mut self, sel: usize, code: KeyCode, mods: KeyModifiers, actions: &Handle) {
        // typing a location (^L): keys go to it
        if self.location.is_some() {
            return self.location_key(code, mods, actions);
        }
        if code == KeyCode::Char('l') && mods.contains(KeyModifiers::CONTROL) {
            match self.list_ok().filter(|l| l.open_location.is_some()) {
                Some(l) => self.location = Some(l.location.clone().unwrap_or_default()),
                None => self.note = "this list can't be opened at a typed location".into(),
            }
            return;
        }
        let shown = self.list_shown();
        let pos = shown.iter().position(|&i| i == sel).unwrap_or(0);
        match code {
            KeyCode::Esc => return self.close_list(actions),
            KeyCode::Char('c') if mods.contains(KeyModifiers::CONTROL) => return self.close_list(actions),
            KeyCode::Enter => return self.list_choose(sel, actions),
            c if listnav::is_move(c) => {
                if let Some(to) = listnav::step(pos, shown.len(), c) {
                    self.overlay = Overlay::List { sel: shown[to] };
                }
                return;
            }
            // nothing typed: back to the list this one's in
            KeyCode::Backspace | KeyCode::Left if self.prompt.is_empty() => return self.list_up(actions),
            KeyCode::Backspace => {
                self.prompt.pop();
            }
            KeyCode::Char('u') if mods.contains(KeyModifiers::CONTROL) => self.prompt.clear(),
            KeyCode::Char(c) if !mods.contains(KeyModifiers::CONTROL) => self.prompt.push(c),
            _ => return,
        }
        // the filter changed: keep the highlight if it's still shown, else the first
        let shown = self.list_shown();
        if !shown.contains(&sel) {
            self.overlay = Overlay::List { sel: shown.first().copied().unwrap_or(0) };
        }
    }

    /// Keys while typing a location: ⏎ opens it (in the app: Kodi opens any
    /// path it knows, smb://, /storage/..., musicdb://albums/), Esc goes
    /// back to the list.
    pub(super) fn location_key(&mut self, code: KeyCode, mods: KeyModifiers, actions: &Handle) {
        let Some(text) = self.location.as_mut() else { return };
        match code {
            KeyCode::Esc => self.location = None,
            KeyCode::Enter => {
                let typed = text.trim().to_string();
                let template = self.list_ok().and_then(|l| l.open_location.clone());
                if let (false, Some(t)) = (typed.is_empty(), template) {
                    self.note = format!("opening {typed}...");
                    self.prompt.clear();
                    let _ = actions.send(Action::ListGo(tveee_core::more::Go::Link(t.replace("{}", &typed))));
                }
                self.location = None;
            }
            KeyCode::Backspace => {
                text.pop();
            }
            KeyCode::Char('u') if mods.contains(KeyModifiers::CONTROL) => text.clear(),
            KeyCode::Char(c) if !mods.contains(KeyModifiers::CONTROL) => text.push(c),
            _ => {}
        }
    }

    /// Go up: to the list this one's in (Kodi's "..").
    pub(super) fn list_up(&mut self, actions: &Handle) {
        match self.list_ok().and_then(|l| l.up.clone()) {
            Some((_, go)) => {
                self.note = "up...".into();
                let _ = actions.send(Action::ListGo(go));
            }
            None => self.note = "this list isn't inside another".into(),
        }
    }

    /// Go back along the trail to crumb `i`.
    pub(super) fn list_crumb(&mut self, i: usize, actions: &Handle) {
        if let Some(go) = self.crumbs.get(i).and_then(|c| c.go.clone()) {
            self.prompt.clear();
            self.note = format!("{}...", self.crumbs[i].title);
            let _ = actions.send(Action::ListGo(go));
        }
    }

    /// Choose item `sel`: a folder opens (the list follows), anything else
    /// plays (the pane closes).
    pub(super) fn list_choose(&mut self, sel: usize, actions: &Handle) {
        let Some(item) = self.list_ok().and_then(|l| l.items.get(sel)).cloned() else { return };
        self.note = format!("{}...", item.label);
        let _ = actions.send(Action::ListGo(item.go));
        if item.folder {
            self.prompt.clear();
        } else {
            self.close_list(actions);
        }
    }

    /// After the state changed: read the list again if the app now shows
    /// another, and follow its highlight when it moves. Stops the refresh
    /// reading where the list is once the pane has closed another way.
    pub(super) fn list_sync(&mut self, change: Change) {
        let Overlay::List { sel } = self.overlay else {
            if self.list_watching {
                let _ = self.engine.send(Action::WatchList(false));
                self.list_watching = false;
            }
            return;
        };
        let place: Option<ListPlace> = self.st.snap.as_ref().and_then(|s| s.list_place.clone());
        let loaded = self.list_ok().map(|l| l.key.clone());
        // a new list came: start where the app is in it
        if change == Change::List {
            if let Some(l) = self.list_ok().cloned() {
                self.crumbs = breadcrumbs(&self.crumbs, &l);
            }
            if let Some(l) = self.list_ok() {
                let at = place.as_ref().filter(|p| p.key == l.key).and_then(|p| l.find(p).or_else(|| l.find_playing(&p.playing_id))).or(l.at).or(l.playing).unwrap_or(0);
                self.overlay = Overlay::List { sel: at };
            }
            self.list_seen = place;
            return;
        }
        let Some(p) = place else { return };
        // an app with a highlight of its own (Kodi) is followed by that;
        // one without (YouTube) by what's playing
        let has_focus = !p.focus_id.is_empty() || !p.focus_label.is_empty();
        if loaded.as_deref() != Some(p.key.as_str()) {
            // another list on the app's screen: its items, once
            if !self.st.list_loading {
                let _ = self.engine.send(Action::LoadList);
            }
        } else if has_focus && self.list_seen.as_ref().is_none_or(|s| s.focus_id != p.focus_id || s.focus_label != p.focus_label) {
            // the app's highlight moved: follow it
            if let Some(at) = self.list_ok().and_then(|l| l.find(&p)) {
                self.overlay = Overlay::List { sel: at };
            }
        } else if !has_focus && self.list_seen.as_ref().is_none_or(|s| s.playing_id != p.playing_id) {
            // no highlight of its own (YouTube): follow what's playing
            if let Some(at) = self.list_ok().and_then(|l| l.find_playing(&p.playing_id)) {
                self.overlay = Overlay::List { sel: at };
            }
        } else {
            self.overlay = Overlay::List { sel };
        }
        self.list_seen = Some(p);
    }

    /// The list pane: its heading and count, the filter, the rows that fit
    /// (the highlight kept in view; the app's own highlight marked ›), and
    /// the keys.
    pub(super) fn draw_list(&mut self, f: &mut Frame, sel: usize) {
        let area = f.area();
        let total = usize::from(area.width.saturating_sub(6)).clamp(40, 100);
        let dim = Style::new().fg(Color::DarkGray);
        let app_name = self.st.snap.as_ref().and_then(front_pkg).map(|p| tveee_core::builtin::apps::by_package(&p).map_or(p.clone(), |a| a.name.to_string())).unwrap_or_else(|| "the app".into());
        let mut text: Vec<Line> = Vec::new();
        let mut rows_at: Vec<Option<usize>> = Vec::new();
        // the breadcrumbs: which text line, and each clickable crumb (x, width, crumb)
        let mut crumb_line: Option<usize> = None;
        let mut crumb_spots: Vec<(u16, u16, usize)> = Vec::new();
        let (title, right) = match self.st.list.as_deref() {
            None => {
                text.push(Line::from("reading the list...").italic());
                (" list ".to_string(), String::new())
            }
            Some(Err(e)) => {
                text.extend(textwrap(&format!("couldn't read the list: {e}"), total).into_iter().map(|l| Line::from(l).fg(Color::Yellow)));
                (" list ".to_string(), String::new())
            }
            Some(Ok(None)) => {
                text.push(Line::from(format!("{app_name} isn't showing a list (open one there, a folder say)")).italic());
                (" list ".to_string(), String::new())
            }
            Some(Ok(Some(l))) => {
                let shown = self.list_shown();
                let pos = shown.iter().position(|&i| i == sel);
                // typing a location (^L): it, in place of the breadcrumbs
                if let Some(typed) = &self.location {
                    text.push(Line::from(vec![
                        Span::styled("location ", dim),
                        Span::styled(format!("{typed}█"), Style::new().fg(Color::White).bg(Color::DarkGray)),
                        Span::styled("  ⏎ open  esc back", dim),
                    ]));
                    rows_at.push(None);
                } else if self.crumbs.len() > 1 {
                    // the trail to here (the last one is here), each to click
                    let mut spans = Vec::new();
                    let mut x = 0u16;
                    for (i, c) in self.crumbs.iter().enumerate() {
                        if i > 0 {
                            spans.push(Span::styled(" › ", dim));
                            x += 3;
                        }
                        let last = i + 1 == self.crumbs.len();
                        let st = if last { Style::new().bold() } else { Style::new().fg(Color::Cyan) };
                        let w = u16::try_from(c.title.chars().count()).unwrap_or(0);
                        if !last && c.go.is_some() {
                            crumb_spots.push((x, w, i));
                        }
                        spans.push(Span::styled(c.title.clone(), st));
                        x += w;
                    }
                    crumb_line = Some(text.len());
                    text.push(Line::from(spans));
                    rows_at.push(None);
                }
                text.push(if self.prompt.is_empty() {
                    Line::from(Span::styled("type to filter", dim))
                } else {
                    Line::from(vec![Span::styled("filter: ", dim), Span::raw(self.prompt.clone())])
                });
                rows_at.push(None);
                // the app's own highlight, marked
                // the same marks in every list: ▶ playing now, › the app's own highlight
                let place = self.st.snap.as_ref().and_then(|s| s.list_place.as_ref()).filter(|p| p.key == l.key);
                let theirs = place.map_or(l.at, |p| l.find(p));
                let playing = place.map_or(l.playing, |p| l.find_playing(&p.playing_id));
                // rows that fit: heading, filter, hint and borders aside
                let room = usize::from(area.height.saturating_sub(8)).max(3);
                // the rows: the items shown, each part's subheading before it
                let mut rows: Vec<std::result::Result<usize, String>> = Vec::new();
                let mut part: Option<&str> = None;
                for &i in &shown {
                    let s = l.items[i].section.as_deref();
                    if s.is_some() && s != part {
                        rows.push(Err(s.unwrap_or_default().to_string()));
                    }
                    part = s;
                    rows.push(Ok(i));
                }
                let at_row = rows.iter().position(|r| *r == Ok(sel)).unwrap_or(0);
                let start = at_row.saturating_sub(room / 2).min(rows.len().saturating_sub(room));
                let meta_w = shown.iter().filter_map(|&i| l.items[i].meta.as_ref()).map(|m| m.chars().count()).max().unwrap_or(0).min(24);
                let label_w = total.saturating_sub(4 + meta_w + 2);
                if shown.is_empty() {
                    text.push(Line::from(if l.items.is_empty() { "(empty)" } else { "nothing matches" }).italic());
                    rows_at.push(None);
                }
                for row in rows.iter().skip(start).take(room) {
                    let i = match row {
                        Ok(i) => *i,
                        Err(heading) => {
                            text.push(Line::from(Span::styled(format!(" ─ {heading} "), Style::new().fg(Color::Magenta).bold())));
                            rows_at.push(None);
                            continue;
                        }
                    };
                    let item = &l.items[i];
                    let mark = if playing == Some(i) { "▶" } else if theirs == Some(i) { "›" } else { " " };
                    let name = format!("{}{}", item.label, if item.folder { "/" } else { "" });
                    let name: String = if name.chars().count() > label_w { format!("{}…", name.chars().take(label_w.saturating_sub(1)).collect::<String>()) } else { name };
                    let meta = item.meta.clone().unwrap_or_default();
                    let line = Line::from(vec![Span::raw(format!(" {mark} {name:<label_w$}  ")), Span::styled(format!("{meta:>meta_w$}"), dim)]);
                    text.push(if i == sel { line.reversed() } else { line });
                    rows_at.push(Some(i));
                }
                let more = if rows.len() > room { "  (scrolls)" } else { "" };
                text.push(Line::default());
                rows_at.push(None);
                let reading = if self.st.list_loading { "  reading..." } else { "" };
                // ^L only where a location can be typed (Kodi's folders)
                let loc = if l.open_location.is_some() { "^L location  " } else { "" };
                text.push(Line::from(format!("↑↓ pgup pgdn home end  ⏎/click choose  ⌫/← up  {loc}type to filter  esc closes{more}{reading}")).fg(Color::DarkGray));
                rows_at.push(None);
                let count = match pos {
                    Some(p) => format!(" {} of {} ", p + 1, shown.len()),
                    None => format!(" {} ", shown.len()),
                };
                (format!(" {} ({app_name}) ", l.title), count)
            }
        };
        // as wide as it can be, so it doesn't change width as it scrolls
        if let Some(first) = text.first_mut() {
            let w = first.width();
            first.spans.push(Span::raw(" ".repeat(total.saturating_sub(w))));
        }
        let r = popup_titled(f, &title, &right, text, Color::Magenta);
        self.hits.push((r, Hit::Popup));
        if let Some(line) = crumb_line {
            let y = r.y + 1 + u16::try_from(line).unwrap_or(0);
            for (x, w, i) in crumb_spots {
                self.hits.push((ratatui::layout::Rect { x: r.x + 1 + x, y, width: w, height: 1 }, Hit::Crumb(i)));
            }
        }
        for (row, at) in rows_at.into_iter().enumerate() {
            if let Some(i) = at {
                let y = r.y + 1 + u16::try_from(row).unwrap_or(0);
                if y + 1 < r.bottom() {
                    self.hits.push((ratatui::layout::Rect { x: r.x + 1, y, width: r.width.saturating_sub(2), height: 1 }, Hit::ListRow(i)));
                }
            }
        }
    }
}

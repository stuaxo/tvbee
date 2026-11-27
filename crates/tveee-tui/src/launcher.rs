//! The app launcher: its rows and tabs, the actions pop-up, keys and drawing.

use super::*;

/// Something the launcher's actions pop-up offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppChoice {
    Open,
    Front,
    Restart,
    Close,
    Search,
    Cancel,
}

impl AppChoice {
    /// What it says in the menu for the app called `app`.
    pub fn label(self, app: &str) -> String {
        match self {
            AppChoice::Open => "open".into(),
            AppChoice::Front => "bring to front".into(),
            AppChoice::Restart => "restart (close, then start again)".into(),
            AppChoice::Close => "close (force-stop)".into(),
            AppChoice::Search => format!("search {app}"),
            AppChoice::Cancel => "cancel".into(),
        }
    }
}

/// The choices for an app: running ones can be brought forward, closed or
/// restarted; "home" can't be closed.
pub fn app_choices(e: &Entry, running: bool, fixed: bool) -> Vec<AppChoice> {
    let home = e.target == "home";
    let mut v = vec![if running { AppChoice::Front } else { AppChoice::Open }];
    // Android restarts a persistent app as soon as it's stopped
    if running && !home && !fixed {
        v.extend([AppChoice::Restart, AppChoice::Close]);
    }
    if !home {
        v.push(AppChoice::Search);
    }
    v.push(AppChoice::Cancel);
    v
}

/// What a choice does: an action to run, or the search box (None, true).
pub fn choice_action(c: AppChoice, e: &Entry) -> (Option<Action>, bool) {
    match c {
        AppChoice::Open => (Some(Action::Launch(e.target.clone())), false),
        AppChoice::Front => (Some(Action::Front(e.target.clone())), false),
        AppChoice::Restart => (Some(Action::Restart { pkg: e.package.clone(), target: e.target.clone() }), false),
        AppChoice::Close => (Some(Action::Close(e.package.clone())), false),
        AppChoice::Search => (None, true),
        AppChoice::Cancel => (None, false),
    }
}

/// Keys in the launcher (`list` is the current tab's rows): arrows + Enter,
/// a letter to jump to the next app whose alias (or name) starts with it, so
/// every letter jumps; Tab, ← or → switch tabs; Esc or Backspace close.
pub fn apps_key(tab: AppTab, sel: usize, code: KeyCode, list: &[Entry]) -> (Overlay, Option<Action>) {
    let n = list.len();
    let at = |sel| (Overlay::Apps { tab, sel }, None);
    match code {
        KeyCode::Esc | KeyCode::Backspace => (Overlay::None, None),
        KeyCode::Tab | KeyCode::Left | KeyCode::Right => (Overlay::Apps { tab: tab.other(), sel: 0 }, None),
        KeyCode::Char(' ') if n > 0 => (Overlay::AppActions { tab, sel, at: 0 }, None),
        KeyCode::Up if n > 0 => at(sel.checked_sub(1).unwrap_or(n - 1)),
        KeyCode::Down if n > 0 => at((sel + 1) % n),
        KeyCode::Enter => match list.get(sel) {
            Some(e) => (Overlay::None, Some(Action::Launch(e.target.clone()))),
            None => at(sel),
        },
        KeyCode::Char(c) if n > 0 => {
            // next match after the current one, wrapping
            let c = c.to_ascii_lowercase();
            let starts = |e: &Entry| e.alias.map_or_else(|| e.name.to_lowercase(), str::to_string).starts_with(c);
            let next = (1..=n).map(|k| (sel + k) % n).find(|&i| starts(&list[i]));
            at(next.unwrap_or(sel))
        }
        _ => at(sel),
    }
}

impl App {
    /// Carry out choice `at` of the actions pop-up for launcher row `sel`.
    pub(super) fn run_choice(&mut self, tab: AppTab, sel: usize, at: usize, actions: &Handle) {
        let rows = tab_list(self.st.apps_list(), tab);
        let Some(e) = rows.get(sel) else {
            self.overlay = Overlay::None;
            return;
        };
        let running = self.st.running.contains(&e.package);
        let Some(choice) = app_choices(e, running, self.st.fixed.contains(&e.package)).get(at).copied() else { return };
        match choice_action(choice, e) {
            // done: back to the list, with what's running brought up to date
            (Some(a), _) => {
                self.apply_overlay(Overlay::Apps { tab, sel }, Some(a), actions);
                let _ = actions.send(Action::CheckRunning);
            }
            (None, true) => self.open_search(Some(e.package.clone())),
            // cancel: back to the list
            (None, false) => self.overlay = Overlay::Apps { tab, sel },
        }
    }

    /// Keys in the actions pop-up.
    pub(super) fn choice_key(&mut self, tab: AppTab, sel: usize, at: usize, code: KeyCode, actions: &Handle) {
        let rows = tab_list(self.st.apps_list(), tab);
        let n = rows.get(sel).map_or(1, |e| app_choices(e, self.st.running.contains(&e.package), self.st.fixed.contains(&e.package)).len());
        match code {
            KeyCode::Up => self.overlay = Overlay::AppActions { tab, sel, at: (at + n - 1) % n },
            KeyCode::Down | KeyCode::Tab => self.overlay = Overlay::AppActions { tab, sel, at: (at + 1) % n },
            KeyCode::Enter | KeyCode::Char(' ') => self.run_choice(tab, sel, at, actions),
            // anything else goes back to the list
            _ => self.overlay = Overlay::Apps { tab, sel },
        }
    }

    /// The launcher pop-up; `behind` another pop-up it's dimmed and inert.
    pub(super) fn draw_apps(&mut self, f: &mut Frame, tab: AppTab, sel: usize, behind: bool) {
        let pkg = self.st.snap.as_ref().and_then(|s| s.focus.as_deref()).map(|f| f.split('/').next().unwrap_or(f).to_string());
        let all = self.st.apps_list();
        let list = tab_list(all, tab);
        let show_pkgs = tveee_core::options::get(tveee_core::options::SHOW_PACKAGES);
        // the tab labels, with counts
        let labels: Vec<(AppTab, String)> = [AppTab::Known, AppTab::Other]
            .into_iter()
            .map(|t| {
                let n = all.iter().filter(|e| t.holds(e)).count();
                let name = if t == AppTab::Known { "Apps" } else { "Other TV apps" };
                (t, format!(" {name} ({n}) "))
            })
            .collect();
        let tab_line = Line::from(
            labels
                .iter()
                .flat_map(|(t, l)| {
                    let st = if *t == tab {
                        Style::new().fg(Color::Black).bg(Color::Green).bold()
                    } else {
                        Style::new().fg(Color::Gray)
                    };
                    [Span::styled(l.clone(), st), Span::raw(" ")]
                })
                .collect::<Vec<_>>(),
        );
        // (line, the entry it shows); section headings show none
        let mut rows: Vec<(Line, Option<usize>)> = Vec::new();
        if self.st.apps.is_none() {
            rows.push((Line::from("looking for installed apps...").italic(), None));
        } else if list.is_empty() {
            rows.push((Line::from("none on this TV").italic(), None));
        }
        // wide enough for the other tab too, so switching doesn't resize the box
        let wide = all.iter().map(|e| e.name.chars().count()).max().unwrap_or(0).min(40);
        let mut section = "";
        for (i, e) in list.iter().enumerate() {
            if tab == AppTab::Other && e.section != section {
                section = e.section;
                rows.push((Line::from(format!(" {section}")).bold(), None));
            }
            let on = if Some(e.package.as_str()) == pkg.as_deref() { "  (on now)" } else { "" };
            let extra = e.alias.or(e.tag).unwrap_or("");
            let dot = match (self.st.running.contains(&e.package) && e.target != "home", self.st.fixed.contains(&e.package)) {
                (true, true) => "⊘",
                (true, false) => "●",
                _ => " ",
            };
            // fixed columns, so both tabs draw the same width; a space after
            // the dot, which some terminals draw wider than one cell
            // and its package, if asked for
            let pkg_col = if show_pkgs { format!("  {}", e.package) } else { String::new() };
            let line = Line::from(format!(" {dot} {:<wide$}  {extra:<12}{on:<10}{pkg_col}", e.name));
            rows.push((if i == sel { line.reversed() } else { line }, Some(i)));
        }
        // the hints, fitted to the terminal's width
        let hint_w = usize::from(f.area().width.saturating_sub(4));
        let hints_for = |more: &str| -> Vec<String> {
            [
                format!("↑/↓ choose  ⏎ open (or to front)  space actions  / search{more}"),
                "● running  ⊘ always running: Android won't let it close".to_string(),
                "letter = jump  tab/←/→ switch  # package names  click / right-click  esc closes   F1 guide  F3 remote".to_string(),
            ]
            .iter()
            .flat_map(|h| super::drawing::wrap_keep(h, hint_w, 0))
            .collect()
        };
        // scroll so the highlighted row stays in view (room for the hints,
        // with "(scrolls)" if it comes to that)
        let hint_rows = hints_for("  (scrolls)").len();
        let room = usize::from(f.area().height).saturating_sub(8 + hint_rows).max(3);
        let at = rows.iter().position(|(_, i)| *i == Some(sel)).unwrap_or(0);
        let start = at.saturating_sub(room.saturating_sub(1)).min(rows.len().saturating_sub(room));
        let scrolls = rows.len() > room;
        let shown: Vec<(Line, Option<usize>)> = rows.into_iter().skip(start).take(room).collect();
        let ids: Vec<Option<usize>> = shown.iter().map(|(_, i)| *i).collect();
        let mut shown: Vec<Line> = shown.into_iter().map(|(l, _)| l).collect();
        // pad to the taller tab, so switching doesn't move the box (or its tabs)
        let tallest = [AppTab::Known, AppTab::Other].iter().map(|t| tab_rows(all, *t)).max().unwrap_or(0);
        while shown.len() < tallest.min(room) {
            shown.push(Line::default());
        }
        let mut text = vec![tab_line, Line::default()];
        text.extend(shown);
        text.push(Line::default());
        text.extend(hints_for(if scrolls { "  (scrolls)" } else { "" }).into_iter().map(|h| Line::from(h).fg(Color::DarkGray)));
        let r = popup(f, " launch an app ", text, Color::Green);
        if behind {
            // under another pop-up: dim, and not clickable
            f.buffer_mut().set_style(r, Style::new().fg(Color::DarkGray).bg(Color::Reset).remove_modifier(Modifier::BOLD));
            return;
        }
        self.hits.push((r, Hit::Popup));
        // tab labels on the first row
        let mut x = r.x + 1;
        for (t, l) in &labels {
            let w = u16::try_from(l.chars().count()).unwrap_or(0);
            self.hits.push((ratatui::layout::Rect { x, y: r.y + 1, width: w, height: 1 }, Hit::AppTab(*t)));
            x += w + 1;
        }
        // app rows start after the tabs and a blank line
        for (row, id) in ids.into_iter().enumerate() {
            if let Some(i) = id {
                let y = r.y + 3 + u16::try_from(row).unwrap_or(0);
                let width = r.width.saturating_sub(2);
                self.hits.push((ratatui::layout::Rect { x: r.x + 1, y, width, height: 1 }, Hit::AppItem(i)));
            }
        }
    }

    pub(super) fn draw_app_actions(&mut self, f: &mut Frame, tab: AppTab, sel: usize, at: usize) {
        // the list stays behind it, dimmed
        self.draw_apps(f, tab, sel, true);
        let rows = tab_list(self.st.apps_list(), tab);
        let Some(e) = rows.get(sel) else { return };
        let running = self.st.running.contains(&e.package);
        let fixed = self.st.fixed.contains(&e.package);
        let choices = app_choices(e, running, fixed);
        let mut text: Vec<Line> = vec![
            Line::from(format!(
                "{}  {}",
                e.package,
                match (running, fixed) {
                    (true, true) => "(always running: Android won't let it close)",
                    (true, false) => "(running)",
                    _ => "(not running)",
                }
            )).fg(Color::DarkGray),
            Line::default(),
        ];
        for (i, c) in choices.iter().enumerate() {
            let line = Line::from(format!("  {}", c.label(&e.name)));
            text.push(if i == at { line.reversed() } else { line });
        }
        text.push(Line::default());
        text.push(Line::from("↑/↓ choose  ⏎ do it  esc back  or click").fg(Color::DarkGray));
        let r = popup(f, &format!(" {} ", e.name), text, Color::Yellow);
        self.hits.push((r, Hit::Popup));
        for i in 0..choices.len() {
            let y = r.y + 3 + u16::try_from(i).unwrap_or(0);
            self.hits.push((ratatui::layout::Rect { x: r.x + 1, y, width: r.width.saturating_sub(2), height: 1 }, Hit::AppChoice(i)));
        }
    }
}

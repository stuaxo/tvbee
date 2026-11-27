//! Pop-ups over the main screen: help, the "more" menu, options, search,
//! text prompts, pairing PIN and "are you sure?".

use super::*;

/// What is drawn over the main screen, if anything.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Overlay {
    None,
    /// Typing the PIN the TV shows, to pair (the text is `App::prompt`).
    Pin,
    /// Changing the Ambilight: the highlighted row and option.
    Ambilight { row: usize, col: usize },
    /// Typing a colour for option `col` of the dialog's lights row (0: all).
    AmbiColour { row: usize, col: usize },
    Help,
    /// The "more" menu, with the highlighted item.
    Menu(usize),
    /// "Are you sure?" for menu item N; `yes` is the highlighted answer.
    Confirm { id: &'static str, yes: bool },
    /// The command palette, with the highlighted match (what's typed is
    /// `App::prompt`).
    Palette(usize),
    /// The log, scrolled up this many lines from the end.
    Logs(usize),
    /// The app launcher: which tab, and the highlighted app in it.
    Apps { tab: AppTab, sel: usize },
    /// Typing what action `id` needs (the text is `App::prompt`).
    Prompt(&'static str),
    /// Typing a search; N picks the app from `App::search_targets`.
    Search(usize),
    /// What to do with launcher row `sel` of `tab`; `at` is the highlighted choice.
    AppActions { tab: AppTab, sel: usize, at: usize },
    /// tveee's options, with N highlighted.
    Options(usize),
    /// The live TV guide on `tab`, with channel `at` (an id) highlighted;
    /// the filter typed is `App::prompt`.
    Guide { tab: GuideTab, at: u32 },
}

/// An app the search box can search in, and how.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchTarget {
    pub package: String,
    pub name: String,
    /// "search", "results link" or "keys" (typing into the app).
    pub method: &'static str,
}

/// The apps to offer, `first` (the app in front, or the launcher's pick)
/// leading, then everything with a proper search.
pub fn search_targets(first: Option<&str>, searchable: &[search::Searchable]) -> Vec<SearchTarget> {
    let name = |pkg: &str| apps::by_package(pkg).map_or_else(|| pkg.to_string(), |a| list_name(a.name));
    let mut out: Vec<SearchTarget> = Vec::new();
    let pkgs = first.into_iter().map(str::to_string).chain(searchable.iter().map(|s| s.package.clone()));
    for p in pkgs {
        // home screens have nothing to search
        if out.iter().any(|t| t.package == p) || apps::by_package(&p).is_some_and(|a| a.kind == apps::Kind::Launcher) {
            continue;
        }
        out.push(SearchTarget { name: name(&p), method: search::method(searchable, &p), package: p });
    }
    out
}

/// Keys in the search box: Tab / ↑ / ↓ change the app; the rest edit the
/// query (as in [`prompt_key`]); Enter searches; Esc cancels.
pub fn search_key(at: usize, code: KeyCode, mods: KeyModifiers, text: &mut String, targets: &[SearchTarget]) -> (Overlay, Option<Action>) {
    let n = targets.len().max(1);
    match code {
        KeyCode::Tab | KeyCode::Down => (Overlay::Search((at + 1) % n), None),
        KeyCode::BackTab | KeyCode::Up => (Overlay::Search((at + n - 1) % n), None),
        KeyCode::Enter => match targets.get(at) {
            Some(t) if !text.trim().is_empty() => (
                Overlay::None,
                Some(Action::Search { pkg: t.package.clone(), query: text.trim().to_string(), keys: t.method == "keys" }),
            ),
            _ => (Overlay::Search(at), None),
        },
        _ => match prompt_key("", code, mods, text) {
            (Overlay::None, _) => (Overlay::None, None),
            _ => (Overlay::Search(at), None),
        },
    }
}

/// Keys while typing what action `id` needs: edit `text`; Enter runs it
/// with that, Esc cancels. Ctrl-U clears.
pub fn prompt_key(id: &'static str, code: KeyCode, mods: KeyModifiers, text: &mut String) -> (Overlay, Option<Action>) {
    let here = Overlay::Prompt(id);
    match code {
        KeyCode::Esc => (Overlay::None, None),
        KeyCode::Enter => {
            let t = text.trim().to_string();
            if t.is_empty() { (here, None) } else { (Overlay::None, Some(Action::Run { id, args: vec![t] })) }
        }
        KeyCode::Backspace => {
            text.pop();
            (here, None)
        }
        KeyCode::Char('u') if mods.contains(KeyModifiers::CONTROL) => {
            text.clear();
            (here, None)
        }
        KeyCode::Char(c) if !mods.contains(KeyModifiers::CONTROL) => {
            text.push(c);
            (here, None)
        }
        _ => (here, None),
    }
}

/// Pasted text, as one line (newlines and other control characters dropped).
pub fn paste_text(s: &str) -> String {
    s.trim_end_matches(['\n', '\r'])
        .chars()
        .map(|c| if c == '\n' || c == '\r' || c == '\t' { ' ' } else { c })
        .filter(|c| !c.is_control())
        .collect()
}

/// What a key does while an overlay is open: the new overlay, and maybe an
/// action to run. Esc (or the key that opened it) closes; it never quits.
pub fn overlay_key(overlay: Overlay, code: KeyCode, items: &[ActionDescriptor]) -> (Overlay, Option<Action>) {
    let pick_item = |i: usize| pick(items, i);
    match overlay {
        Overlay::None => (Overlay::None, None),
        // any key closes the help
        Overlay::Help => (Overlay::None, None),
        Overlay::Menu(sel) => match code {
            KeyCode::Esc | KeyCode::Char('.') | KeyCode::Char('q') | KeyCode::Backspace => (Overlay::None, None),
            KeyCode::Up => (Overlay::Menu(sel.checked_sub(1).unwrap_or(items.len().saturating_sub(1))), None),
            KeyCode::Down => (Overlay::Menu(if sel + 1 >= items.len() { 0 } else { sel + 1 }), None),
            KeyCode::Enter => pick_item(sel),
            KeyCode::Char(c) => match items.iter().position(|it| it.key == Some(c)) {
                Some(i) => pick_item(i),
                None => (overlay, None),
            },
            _ => (overlay, None),
        },
        // handled by apps_key / prompt_key
        Overlay::Apps { tab, sel } => (Overlay::Apps { tab, sel }, None),
        Overlay::Prompt(id) => (Overlay::Prompt(id), None),
        // handled by App::palette_key
        Overlay::Palette(sel) => (Overlay::Palette(sel), None),
        Overlay::Logs(up) => match code {
            KeyCode::Up => (Overlay::Logs(up + 1), None),
            KeyCode::Down => (Overlay::Logs(up.saturating_sub(1)), None),
            KeyCode::PageUp => (Overlay::Logs(up + 10), None),
            KeyCode::PageDown => (Overlay::Logs(up.saturating_sub(10)), None),
            KeyCode::End => (Overlay::Logs(0), None),
            _ => (Overlay::None, None),
        },
        Overlay::Search(i) => (Overlay::Search(i), None),
        Overlay::AppActions { tab, sel, at } => (Overlay::AppActions { tab, sel, at }, None),
        // handled by App::guide_key
        Overlay::Guide { tab, at } => (Overlay::Guide { tab, at }, None),
        // handled by App::options_key
        Overlay::Options(i) => (Overlay::Options(i), None),
        Overlay::Confirm { id, yes } => match code {
            KeyCode::Char('y') | KeyCode::Char('Y') => (Overlay::None, Some(run(id))),
            KeyCode::Left | KeyCode::Right | KeyCode::Tab => (Overlay::Confirm { id, yes: !yes }, None),
            KeyCode::Enter if yes => (Overlay::None, Some(run(id))),
            // anything else, including Enter on "No", cancels
            _ => (Overlay::None, None),
        },
        // handled where it's typed (the PIN is `App::prompt`)
        Overlay::Pin => (Overlay::Pin, None),
        Overlay::Ambilight { row, col } => (Overlay::Ambilight { row, col }, None),
        Overlay::AmbiColour { row, col } => (Overlay::AmbiColour { row, col }, None),
    }
}

/// Running action `id` with nothing typed.
pub fn run(id: &'static str) -> Action {
    Action::Run { id, args: Vec::new() }
}

/// What it takes to run, as usage text ("up|down|<level>"): its own, or
/// its command's.
pub fn usage_of(a: &ActionDescriptor) -> &'static str {
    if !a.args.is_empty() { a.args } else { a.cli.map_or("", |c| c.usage) }
}

/// Whether running `a` needs something typed first: text it asks for, or
/// arguments it can't do without (`<level>`, not `[next|off]`).
pub fn needs_input(a: &ActionDescriptor) -> bool {
    let usage = usage_of(a);
    matches!(a.params, Params::Text { .. }) || (!usage.is_empty() && !usage.starts_with('['))
}

/// What choosing an action does: ask for what it needs, ask first if it's
/// risky, or run it.
pub fn choose(a: &ActionDescriptor) -> (Overlay, Option<Action>) {
    if needs_input(a) {
        (Overlay::Prompt(a.id), None)
    } else if a.confirm.is_some() {
        (Overlay::Confirm { id: a.id, yes: false }, None)
    } else {
        (Overlay::None, Some(run(a.id)))
    }
}

/// What choosing menu item `i` does (see [`choose`]).
pub fn pick(items: &[ActionDescriptor], i: usize) -> (Overlay, Option<Action>) {
    items.get(i).map_or((Overlay::None, None), choose)
}

pub(super) const HELP: &str = " everywhere
   space p  play/pause       ⌫      back
   h        home             [ ]    seek -/+ 60s
   0-9      jump to 0%-90%   + -    volume
   m        mute/unmute      r      refresh
   tab      switch mode      .      more (power, mirror, ...)
   : ctrl+p commands: everything tveee can do, found by typing a few letters
            (the ui's own too: the log of what tveee did lately, help, ...)
   F1 F2 F3 the guide, the apps, remote mode (also alt+1-3; from the lists too)
   F        free memory (force-stop idle apps)   S  sleep   R  restart the app
            in front, coming back where it was (YouTube, Kodi)
   i        system info      a      launch an app (space there: front / restart / close)
   l        Ambilight panel (TVs with one; P there pairs if needed)
   L        change the Ambilight: power, style and setting, Hue, any light's colour
   v        preview: a picture of what's on (scrcpy if installed, 1 a second;
            YouTube's storyboards; else a screenshot every few seconds)
   /        search the app in front (tab picks another app)
   g        live TV guide: now/next on every channel (type to filter, ←/→ later
            or earlier, tab favourites/radio, shift+tab programme details, ⏎ watch,
            * or click the star to add/remove a favourite);
            clicking the channel line opens it too
   . o      open a link (YouTube, Netflix, media files)
   ,        options (scroll animations, ...)
   q Esc    quit

 player mode
   ← →      seek -/+ 10s (exact in YouTube; ff/rew keys elsewhere)
   ↑ ↓      volume

 remote mode: the keyboard is the TV's (tab leaves, ctrl-c quits)
   arrows   move around the TV's menus
   ⏎ esc    OK, back
   ⌫ del    delete in a text box (backward, forward)
   home     the TV's home screen    pgup pgdn  page up/down
   typing   typed into the TV (a search box, say); pasting too
   other actions: the buttons along the bottom

 mouse
   click the buttons, the progress bar (jump) or volume bar (set)
   wheel    volume
   Shift+drag selects text as usual (to copy the url)";

impl App {
    /// Keys in the options dialog: ↑/↓ choose, Space/Enter toggle, Esc closes.
    pub(super) fn options_key(&mut self, i: usize, code: KeyCode) {
        let n = tveee_core::options::OPTIONS.len().max(1);
        match code {
            KeyCode::Esc | KeyCode::Char(',') | KeyCode::Char('q') | KeyCode::Backspace => self.overlay = Overlay::None,
            KeyCode::Up => self.overlay = Overlay::Options((i + n - 1) % n),
            KeyCode::Down | KeyCode::Tab => self.overlay = Overlay::Options((i + 1) % n),
            KeyCode::Enter | KeyCode::Char(' ') => self.toggle_option(i),
            _ => {}
        }
    }

    /// Flip option `i` and save it.
    pub(super) fn toggle_option(&mut self, i: usize) {
        let Some(o) = tveee_core::options::OPTIONS.get(i) else { return };
        let on = !tveee_core::options::get(o.name);
        self.note = match tveee_core::options::set(o.name, on) {
            Ok(()) => format!("{}: {}", o.label, if on { "on" } else { "off" }),
            Err(e) => format!("couldn't save the option: {e}"),
        };
        self.overlay = Overlay::Options(i);
    }

    /// Open the search box on `pkg` (or the app in front).
    pub(super) fn open_search(&mut self, pkg: Option<String>) {
        let front = self.st.snap.as_ref().and_then(|s| s.focus.as_deref()).map(|f| f.split('/').next().unwrap_or(f).to_string());
        let first = pkg.or(front);
        self.search_targets = search_targets(first.as_deref(), self.st.searchable_list());
        self.prompt.clear();
        self.overlay = Overlay::Search(0);
    }

    pub(super) fn draw_overlay(&mut self, f: &mut Frame) {
        match self.overlay {
            Overlay::None => {}
            Overlay::Help => self.draw_help(f),
            Overlay::Menu(sel) => self.draw_menu(f, sel),
            Overlay::Options(sel) => self.draw_options(f, sel),
            Overlay::Guide { tab, at } => self.draw_guide(f, tab, at),
            Overlay::Apps { tab, sel } => self.draw_apps(f, tab, sel, false),
            Overlay::AppActions { tab, sel, at } => self.draw_app_actions(f, tab, sel, at),
            Overlay::Search(at) => self.draw_search(f, at),
            Overlay::Prompt(id) => self.draw_prompt(f, id),
            Overlay::Palette(sel) => self.draw_palette(f, sel),
            Overlay::Logs(up) => self.draw_logs(f, up),
            Overlay::Ambilight { row, col } => self.draw_ambi_dialog(f, row, col),
            Overlay::AmbiColour { row, col } => self.draw_ambi_colour(f, row, col),
            Overlay::Pin => self.draw_pin(f),
            Overlay::Confirm { id, yes } => self.draw_confirm(f, id, yes),
        }
    }

    pub(super) fn draw_help(&mut self, f: &mut Frame) {
        let text: Vec<Line> = HELP.lines().map(Line::from).collect();
        let r = popup(f, " keys - any key closes ", text, Color::DarkGray);
        self.hits.push((r, Hit::Popup));
    }

    pub(super) fn draw_menu(&mut self, f: &mut Frame, sel: usize) {
        let (text, items) = self.menu_lines(sel);
        let r = popup(f, " more - esc closes ", text, Color::Cyan);
        self.hits.push((r, Hit::Popup));
        for (row, item) in items.into_iter().enumerate() {
            if let Some(i) = item {
                let y = r.y + 1 + u16::try_from(row).unwrap_or(0);
                self.hits.push((ratatui::layout::Rect { x: r.x + 1, y, width: r.width.saturating_sub(2), height: 1 }, Hit::MenuItem(i)));
            }
        }
    }

    pub(super) fn draw_options(&mut self, f: &mut Frame, sel: usize) {
        let mut text = Vec::new();
        for (i, o) in tveee_core::options::OPTIONS.iter().enumerate() {
            let tick = if tveee_core::options::get(o.name) { "[x]" } else { "[ ]" };
            let line = Line::from(vec![Span::raw(format!(" {tick} {:<20}", o.label)), Span::styled(format!("  {}", o.help), Style::new().fg(Color::DarkGray))]);
            text.push(if i == sel { line.reversed() } else { line });
        }
        text.push(Line::default());
        text.push(Line::from("↑/↓ choose  space/⏎/click toggle  esc closes  (saved in ~/.config/tveee/options.txt)").fg(Color::DarkGray));
        let r = popup(f, " options ", text, Color::Blue);
        self.hits.push((r, Hit::Popup));
        for i in 0..tveee_core::options::OPTIONS.len() {
            let y = r.y + 1 + u16::try_from(i).unwrap_or(0);
            self.hits.push((ratatui::layout::Rect { x: r.x + 1, y, width: r.width.saturating_sub(2), height: 1 }, Hit::OptionItem(i)));
        }
    }

    pub(super) fn draw_search(&mut self, f: &mut Frame, at: usize) {
        let field = 60usize;
        let n = self.prompt.chars().count();
        let shown: String = self.prompt.chars().skip(n.saturating_sub(field - 1)).collect();
        let target = self.search_targets.get(at);
        let app_name = target.map_or("(no app in front)".to_string(), |t| t.name.clone());
        let how = match target.map(|t| t.method) {
            Some("keys") => "no search of its own: tveee will press its search key and type (best effort)",
            Some("results link") => "opens its search results",
            Some(_) => "uses the app's own search",
            None => "",
        };
        let mut text = vec![
            Line::from(vec![
                Span::raw("Search in  "),
                Span::styled(format!(" {app_name} "), Style::new().fg(Color::Black).bg(Color::Cyan).bold()),
                Span::styled("   tab / ↑↓ change app", Style::new().fg(Color::DarkGray)),
            ]),
            Line::from(how).fg(if target.is_some_and(|t| t.method == "keys") { Color::Yellow } else { Color::DarkGray }),
            Line::default(),
            Line::from(vec![
                Span::styled(format!("{shown}█"), Style::new().fg(Color::White).bg(Color::DarkGray)),
                Span::styled(" ".repeat(field.saturating_sub(shown.chars().count() + 1)), Style::new().bg(Color::DarkGray)),
            ]),
            Line::default(),
        ];
        if self.st.searchable.is_none() {
            text.push(Line::from("(still checking which apps can search...)").italic());
        }
        text.push(Line::from("⏎ search   esc cancel   paste works   ctrl-u clears").fg(Color::DarkGray));
        let r = popup(f, " search ", text, Color::Cyan);
        self.hits.push((r, Hit::Popup));
    }

    pub(super) fn draw_prompt(&mut self, f: &mut Frame, id: &'static str) {
        let Some(it) = tveee_core::actions::find(&self.actions, id) else { return };
        // text it asks for, or the arguments it takes
        let (prompt, hint) = match it.params {
            Params::Text { prompt, hint } => (prompt.to_string(), hint),
            Params::None => (format!("{}:", it.title), usage_of(it)),
        };
        let field = 60usize;
        // show the end of long input
        let shown: String = {
            let n = self.prompt.chars().count();
            self.prompt.chars().skip(n.saturating_sub(field - 1)).collect()
        };
        let text = vec![
            Line::from(prompt),
            Line::from(vec![
                Span::styled(format!("{shown}█"), Style::new().fg(Color::White).bg(Color::DarkGray)),
                Span::styled(" ".repeat(field.saturating_sub(shown.chars().count() + 1)), Style::new().bg(Color::DarkGray)),
            ]),
            Line::from(hint).fg(Color::DarkGray),
            Line::default(),
            Line::from("⏎ go   esc cancel   paste works   ctrl-u clears").fg(Color::DarkGray),
        ];
        let r = popup(f, &format!(" {} ", it.title), text, Color::Yellow);
        self.hits.push((r, Hit::Popup));
    }

    /// The log's last lines, in most of the screen.
    pub(super) fn draw_logs(&mut self, f: &mut Frame, up: usize) {
        let area = f.area();
        let r = ratatui::layout::Rect {
            x: area.x + area.width / 20,
            y: area.y + area.height / 10,
            width: area.width - area.width / 10,
            height: area.height - area.height / 5,
        };
        let lines = tveee_core::log::recent();
        let room = usize::from(r.height.saturating_sub(2));
        let up = up.min(lines.len().saturating_sub(room));
        let end = lines.len() - up;
        let shown: Vec<Line> = if lines.is_empty() {
            vec![Line::from("nothing logged yet").italic()]
        } else {
            lines[end.saturating_sub(room)..end].iter().map(|l| Line::from(l.clone())).collect()
        };
        f.render_widget(ratatui::widgets::Clear, r);
        let title = format!(" log - {} lines (TVEEE_LOG=<file> keeps them) ", lines.len());
        let block = Block::bordered()
            .title(title)
            .title_bottom(Line::from(" ↑↓ pgup/pgdn scroll  end: newest  any other key closes ").right_aligned())
            .border_style(Style::new().fg(Color::DarkGray));
        f.render_widget(Paragraph::new(shown).block(block), r);
        self.hits.push((r, Hit::Popup));
    }

    pub(super) fn draw_pin(&mut self, f: &mut Frame) {
        let text = vec![
            Line::from("Type the PIN the TV is showing:"),
            Line::from(vec![
                Span::styled(format!("{}█", self.prompt), Style::new().fg(Color::White).bg(Color::DarkGray)),
                Span::styled(" ".repeat(8usize.saturating_sub(self.prompt.chars().count() + 1)), Style::new().bg(Color::DarkGray)),
            ]),
            Line::default(),
            Line::from("⏎ pair   esc cancel").fg(Color::DarkGray),
        ];
        let r = popup(f, " pair with the TV ", text, Color::Yellow);
        self.hits.push((r, Hit::Popup));
    }

    pub(super) fn draw_confirm(&mut self, f: &mut Frame, id: &'static str, yes: bool) {
        let Some(it) = tveee_core::actions::find(&self.actions, id) else { return };
        let mut text: Vec<Line> = it.confirm.unwrap_or("Are you sure?").lines().map(Line::from).collect();
        text.push(Line::default());
        let answers_row = u16::try_from(text.len()).unwrap_or(0);
        let pick = |label: &'static str, on: bool| {
            let st = if on { Style::new().fg(Color::Black).bg(Color::Red).bold() } else { Style::new().reversed() };
            Span::styled(format!(" {label} "), st)
        };
        text.push(Line::from(vec![pick("No", !yes), Span::raw("   "), pick("Yes", yes)]));
        text.push(Line::from("y = yes   ←/→ choose   ⏎ confirm   any other key cancels").fg(Color::DarkGray));
        let r = popup(f, &format!(" {} ", it.title), text, Color::Red);
        self.hits.push((r, Hit::Popup));
        let y = r.y + 1 + answers_row;
        // " No " then 3 spaces then " Yes "
        self.hits.push((ratatui::layout::Rect { x: r.x + 1, y, width: 4, height: 1 }, Hit::ConfirmNo));
        self.hits.push((ratatui::layout::Rect { x: r.x + 8, y, width: 5, height: 1 }, Hit::ConfirmYes));
    }

    /// Items grouped under their plugin, the highlighted one reversed; and for
    /// each line, the item it shows (None for headings).
    pub(super) fn menu_lines(&self, sel: usize) -> (Vec<Line<'static>>, Vec<Option<usize>>) {
        let (mut lines, mut items) = (Vec::new(), Vec::new());
        let mut group = "";
        for (i, it) in self.menu.iter().enumerate() {
            if it.category != group {
                group = it.category;
                lines.push(Line::from(group).bold());
                items.push(None);
            }
            let dots = if it.confirm.is_some() || it.params != Params::None { "..." } else { "" };
            let line = Line::from(format!("  {}  {}{dots}", it.key.unwrap_or(' '), it.title));
            lines.push(if i == sel { line.reversed() } else { line });
            items.push(Some(i));
        }
        lines.push(Line::default());
        lines.push(Line::from("↑/↓ choose  ⏎ run  press the key, or click").fg(Color::DarkGray));
        (lines, items)
    }
}

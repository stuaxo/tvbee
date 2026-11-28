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
    Help(usize),
    /// The "more" menu, with the highlighted item.
    Menu(usize),
    /// "Are you sure?" for menu item N; `yes` is the highlighted answer.
    Confirm { id: &'static str, yes: bool },
    /// The command palette, with the highlighted match (what's typed is
    /// `App::prompt`).
    Palette(usize),
    /// The log, scrolled up this many lines from the end.
    /// The log: line `at` (by its number) highlighted, or None following
    /// the newest; `from`: where a selection starts.
    Logs { at: Option<usize>, from: Option<usize> },
    /// Typing the file to save the log to (the name is `App::prompt`).
    SaveLog,
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
    /// The list the app in front shows (`t`), item `sel` (an index into it)
    /// highlighted; the filter typed is `App::prompt`.
    List { sel: usize },
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
        // arrows scroll the help; any other key closes it
        Overlay::Help(top) => match code {
            KeyCode::Up => (Overlay::Help(top.saturating_sub(1)), None),
            KeyCode::Down => (Overlay::Help(top.saturating_add(1)), None),
            KeyCode::PageUp => (Overlay::Help(top.saturating_sub(10)), None),
            KeyCode::PageDown => (Overlay::Help(top.saturating_add(10)), None),
            KeyCode::Home => (Overlay::Help(0), None),
            // drawing stops it at the last page
            KeyCode::End => (Overlay::Help(usize::MAX / 2), None),
            _ => (Overlay::None, None),
        },
        Overlay::Menu(sel) => match code {
            KeyCode::Esc | KeyCode::Char('.') | KeyCode::Char('q') | KeyCode::Backspace => (Overlay::None, None),
            c if super::listnav::is_move(c) => {
                let to = super::listnav::step(sel, items.len(), c).unwrap_or(sel);
                (Overlay::Menu(to), None)
            }
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
        // handled by App::logs_key / save_log_key
        Overlay::Logs { at, from } => (Overlay::Logs { at, from }, None),
        Overlay::SaveLog => (Overlay::SaveLog, None),
        Overlay::Search(i) => (Overlay::Search(i), None),
        Overlay::AppActions { tab, sel, at } => (Overlay::AppActions { tab, sel, at }, None),
        // handled by App::guide_key
        Overlay::Guide { tab, at } => (Overlay::Guide { tab, at }, None),
        // handled by App::list_key
        Overlay::List { sel } => (Overlay::List { sel }, None),
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

/// [`HELP`] in rows of at most `w`: as it is when it fits; else its two
/// columns one under the other, and long rows wrapped under their text.
pub(super) fn help_lines(w: usize) -> Vec<String> {
    if HELP.lines().all(|l| l.chars().count() <= w) {
        return HELP.lines().map(str::to_string).collect();
    }
    const COL2: usize = 29;
    let mut out = Vec::new();
    for line in HELP.lines() {
        let c: Vec<char> = line.chars().collect();
        // a key row ("   m        mute/unmute      r      refresh"): its
        // second column has a row of its own
        let key_row = c.len() > 3 && c[..3].iter().all(|&x| x == ' ') && c[3] != ' ';
        let two = key_row && c.len() > COL2 && c[COL2 - 3..COL2].iter().all(|&x| x == ' ') && c[COL2] != ' ';
        let rows = if two {
            // the second column's keys are narrower: lined up with the first's
            let second: String = c[COL2..].iter().collect();
            let (key, what) = second.split_once("  ").unwrap_or((&second, ""));
            vec![c[..COL2].iter().collect::<String>(), format!("   {key:<9}{}", what.trim_start())]
        } else {
            vec![line.to_string()]
        };
        for row in rows {
            let lead = row.chars().take_while(|&x| x == ' ').count();
            let hang = if lead == 3 { 12 } else { lead };
            out.extend(super::drawing::wrap_keep(&row, w, hang));
        }
    }
    out
}

pub(super) const HELP: &str = " everywhere
   space p  play/pause       ⌫      back
   h        home             [ ]    seek -/+ 60s
   0-9      jump to 0%-90%   + -    volume
   m        mute/unmute      r      refresh
   tab      switch mode      .      more (power, mirror, ...)
   lists    ↑ ↓ one, pgup pgdn a page, home end the first and last
   : ctrl+p commands: everything tveee can do, found by typing a few letters
            (the ui's own too: the log of what tveee did lately, help, ...)
   F1 F2 F3 the guide, the apps, remote mode (also alt+1-3; from the lists too)
   F        free memory (force-stop idle apps)   S  sleep   R  restart the app
            in front, coming back where it was (YouTube, Kodi)
   i        system info      a      launch an app (space there: front / restart / close)
   u        the link whole / shortened    y  copy the link (click it: the same;
            its buttons copy or open it; ctrl+click opens it in most terminals)
   l        Ambilight panel (TVs with one; P there pairs if needed)
   L        change the Ambilight in its panel: ↑↓ ←→ (or click), ⏎ on a light:
            a colour picker (arrows or click, or type a name or #hex)
   v        preview: a picture of what's on (scrcpy if installed, 1 a second;
            YouTube's storyboards; else a screenshot every few seconds)
   /        search the app in front (tab picks another app)
   z        music visualiser: show or hide it (Kodi, while it plays music)
   t        the list the app shows (Kodi's folder or add-on, YouTube's chapters
            and links): choose from it, following the app (type to filter;
            ⌫ ← or alt+↑ up a level, tab the breadcrumbs, ^L a location)
   g        live TV guide: now/next on every channel (type to filter, ←/→ later
            or earlier, tab favourites/radio, shift+tab programme details, ⏎ watch,
            * or click the star to add/remove a favourite);
            clicking the channel line opens it too
   . o      open a link (YouTube, Netflix, media files)
   ,        options (scroll animations, ...)
   q Esc    quit

 player mode
   ← →      seek -/+ 10s (exact in YouTube; ff/rw keys elsewhere)
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
    /// The plugins its tab lets you change: the vendor ones (built-in ones
    /// are always on).
    pub(super) fn vendor_plugins(&self) -> Vec<&tveee_core::plugin::PluginState> {
        self.plugins.iter().filter(|p| p.source != tveee_core::plugin::Source::Builtin).collect()
    }

    /// Keys in the options dialog: ↑/↓ choose, Space/Enter change it,
    /// Tab/←/→ switch between options and plugins, Esc closes.
    pub(super) fn options_key(&mut self, i: usize, code: KeyCode) {
        let n = if self.options_plugins { self.vendor_plugins().len() } else { tveee_core::options::OPTIONS.len() }.max(1);
        match code {
            KeyCode::Esc | KeyCode::Char(',') | KeyCode::Char('q') | KeyCode::Backspace => self.overlay = Overlay::None,
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Left | KeyCode::Right => {
                self.options_plugins = !self.options_plugins;
                self.overlay = Overlay::Options(0);
            }
            c if super::listnav::is_move(c) => {
                let to = super::listnav::step(i, n, c).unwrap_or(i);
                self.overlay = Overlay::Options(to)
            }
            KeyCode::Enter | KeyCode::Char(' ') => self.toggle_option(i),
            KeyCode::Char('a') if self.options_plugins => self.plugin_choice(i, None),
            _ => {}
        }
    }

    /// Whether vendor plugin `p` is ticked: its saved choice, else whether
    /// it's on now.
    pub(super) fn plugin_ticked(&self, p: &tveee_core::plugin::PluginState) -> bool {
        self.plugin_opt(p.name).unwrap_or(p.enabled)
    }

    /// Option `name` (see [`App::opts`]).
    pub(super) fn opt(&self, name: &str) -> bool {
        match &self.opts {
            None => tveee_core::options::get(name),
            Some(v) => v.iter().rev().find(|(k, _)| k == name).map(|(_, on)| *on).unwrap_or_else(|| tveee_core::options::OPTIONS.iter().any(|o| o.name == name && o.default)),
        }
    }

    /// The choice saved for vendor plugin `name` (see [`App::opts`]).
    pub(super) fn plugin_opt(&self, name: &str) -> Option<bool> {
        match &self.opts {
            None => tveee_core::options::plugin(name),
            Some(v) => v.iter().rev().find(|(k, _)| *k == format!("plugin.{name}")).map(|(_, on)| *on),
        }
    }

    /// Save vendor plugin row `i`'s choice (None: back to checking the TV).
    pub(super) fn plugin_choice(&mut self, i: usize, choice: Option<bool>) {
        let Some(p) = self.vendor_plugins().get(i).map(|p| p.name) else { return };
        self.note = match tveee_core::options::set_plugin(p, choice) {
            Ok(()) => format!("plugin {p}: {} (from tveee's next start)", tveee_core::options::choice_word(choice)),
            Err(e) => format!("couldn't save it: {e}"),
        };
        self.overlay = Overlay::Options(i);
    }

    /// Change row `i` of the tab shown: flip an option, or a vendor
    /// plugin's tick (saved as its choice).
    pub(super) fn toggle_option(&mut self, i: usize) {
        if self.options_plugins {
            let Some(on) = self.vendor_plugins().get(i).map(|p| self.plugin_ticked(p)) else { return };
            self.plugin_choice(i, Some(!on));
            return;
        }
        let Some(o) = tveee_core::options::OPTIONS.get(i) else { return };
        let on = !self.opt(o.name);
        let saved = match &mut self.opts {
            // this app's own options (tests): changed here, not saved
            Some(v) => {
                v.push((o.name.to_string(), on));
                Ok(())
            }
            None => tveee_core::options::set(o.name, on),
        };
        self.note = match saved {
            Ok(()) => format!("{}: {}", o.label, if on { "on" } else { "off" }),
            Err(e) => format!("couldn't save the option: {e}"),
        };
        self.overlay = Overlay::Options(i);
        // what the refresh reads changed: a read now, so it shows at once
        // (load and memory back on weren't being read)
        if [tveee_core::options::SHOW_LOAD, tveee_core::options::LOW_BANDWIDTH].contains(&o.name) {
            let _ = self.engine.send(Action::Refresh);
        }
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
        // the commands over another pop-up: that first, under them
        if let (Overlay::Palette(_), Some((under, text))) = (self.overlay, self.under.clone()) {
            let (palette_text, over) = (std::mem::replace(&mut self.prompt, text), self.overlay);
            self.overlay = under;
            self.draw_one(f);
            self.overlay = over;
            self.prompt = palette_text;
        }
        self.draw_one(f);
    }

    fn draw_one(&mut self, f: &mut Frame) {
        match self.overlay {
            Overlay::None => {}
            Overlay::Help(top) => self.draw_help(f, top),
            Overlay::Menu(sel) => self.draw_menu(f, sel),
            Overlay::Options(sel) => self.draw_options(f, sel),
            Overlay::Guide { tab, at } => self.draw_guide(f, tab, at),
            Overlay::List { sel } => self.draw_list(f, sel),
            Overlay::Apps { tab, sel } => self.draw_apps(f, tab, sel, false),
            Overlay::AppActions { tab, sel, at } => self.draw_app_actions(f, tab, sel, at),
            Overlay::Search(at) => self.draw_search(f, at),
            Overlay::Prompt(id) => self.draw_prompt(f, id),
            Overlay::Palette(sel) => self.draw_palette(f, sel),
            Overlay::Logs { at, from } => self.draw_logs(f, at, from),
            Overlay::SaveLog => self.draw_save_log(f),
            // changed in the panel itself (its cursor): nothing over it
            Overlay::Ambilight { .. } => {}
            Overlay::AmbiColour { row, col } => self.draw_ambi_colour(f, row, col),
            Overlay::Pin => self.draw_pin(f),
            Overlay::Confirm { id, yes } => self.draw_confirm(f, id, yes),
        }
    }

    /// The keys, from row `top`: fitted to the terminal's width, and
    /// scrolling (↑/↓) when they don't all fit its height.
    pub(super) fn draw_help(&mut self, f: &mut Frame, top: usize) {
        let area = f.area();
        let w = usize::from(area.width.saturating_sub(4));
        let mut lines = help_lines(w);
        // wrapped to fit: the whole width (else a column of the screen's
        // own border shows beside it)
        if HELP.lines().any(|l| l.chars().count() > w)
            && let Some(first) = lines.first_mut()
        {
            *first = format!("{first:<w$}");
        }
        let room = usize::from(area.height.saturating_sub(2)).max(1);
        let top = top.min(lines.len().saturating_sub(room));
        // remembered as shown, so ↑ moves at once from the end
        self.overlay = Overlay::Help(top);
        let (title, right) = if lines.len() > room {
            (" keys - ↑/↓ scroll, any other key closes ", format!(" {}-{} of {} ", top + 1, (top + room).min(lines.len()), lines.len()))
        } else {
            (" keys - any key closes ", String::new())
        };
        let text: Vec<Line> = lines.into_iter().skip(top).take(room).map(Line::from).collect();
        let r = popup_titled(f, title, &right, text, Color::DarkGray);
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

    /// The options dialog: a tab of options, and one of plugins.
    pub(super) fn draw_options(&mut self, f: &mut Frame, sel: usize) {
        let dim = Style::new().fg(Color::DarkGray);
        let tab = |name: &'static str, on: bool| {
            if on { Span::styled(format!(" {name} "), Style::new().fg(Color::Black).bg(Color::Blue).bold()) } else { Span::styled(format!(" {name} "), Style::new().fg(Color::Gray)) }
        };
        let labels = [("Options", false), ("Plugins", true)];
        let mut text = vec![Line::from(labels.iter().flat_map(|(n, p)| [tab(n, *p == self.options_plugins), Span::raw(" ")]).collect::<Vec<_>>()), Line::default()];
        // rows that can be chosen start here
        let first_row = text.len();
        let rows;
        if self.options_plugins {
            let vendor: Vec<_> = self.vendor_plugins().into_iter().cloned().collect();
            rows = vendor.len();
            text.push(Line::from(Span::styled(" vendor", dim)));
            // the rows start under that heading
            let first_row_shift = 1;
            for (i, p) in vendor.iter().enumerate() {
                let choice = self.plugin_opt(p.name);
                let ticked = self.plugin_ticked(p);
                let tick = if ticked { "[x]" } else { "[ ]" };
                let note = match choice {
                    None => "  (auto)".to_string(),
                    Some(on) if on != p.enabled => "  (from the next start)".to_string(),
                    Some(_) => String::new(),
                };
                let line = Line::from(vec![Span::raw(format!(" {tick} {:<20}", p.name)), Span::styled(format!("  {}{note}", p.desc), dim)]);
                text.push(if i == sel { line.reversed() } else { line });
            }
            text.push(Line::default());
            text.push(Line::from(Span::styled(" built in (always on)", dim)));
            for p in self.plugins.iter().filter(|p| p.source == tveee_core::plugin::Source::Builtin) {
                text.push(Line::from(Span::styled(format!(" [x] {:<20}  {}", p.name, p.desc), dim)));
            }
            text.push(Line::default());
            text.push(Line::from("↑/↓ choose  space/⏎/click toggle  a: auto (found on the TV)  tab: options  esc closes").fg(Color::DarkGray));
            text.push(Line::from("changes apply from tveee's next start").fg(Color::DarkGray));
            let r = popup(f, " options ", text, Color::Blue);
            self.option_hits(r, first_row + first_row_shift, rows);
        } else {
            rows = tveee_core::options::OPTIONS.len();
            for (i, o) in tveee_core::options::OPTIONS.iter().enumerate() {
                let tick = if tveee_core::options::get(o.name) { "[x]" } else { "[ ]" };
                let line = Line::from(vec![Span::raw(format!(" {tick} {:<20}", o.label)), Span::styled(format!("  {}", o.help), dim)]);
                text.push(if i == sel { line.reversed() } else { line });
            }
            text.push(Line::default());
            text.push(Line::from("↑/↓ choose  space/⏎/click toggle  tab: plugins  esc closes  (saved in ~/.config/tveee/options.txt)").fg(Color::DarkGray));
            let r = popup(f, " options ", text, Color::Blue);
            self.option_hits(r, first_row, rows);
        }
    }

    /// The options dialog's clickable parts: its tabs, and `rows` rows from
    /// text row `first`.
    fn option_hits(&mut self, r: ratatui::layout::Rect, first: usize, rows: usize) {
        self.hits.push((r, Hit::Popup));
        let (opts, plugs) = (u16::try_from(" Options ".len()).unwrap_or(0), u16::try_from(" Plugins ".len()).unwrap_or(0));
        self.hits.push((ratatui::layout::Rect { x: r.x + 1, y: r.y + 1, width: opts, height: 1 }, Hit::OptionTab(false)));
        self.hits.push((ratatui::layout::Rect { x: r.x + 2 + opts, y: r.y + 1, width: plugs, height: 1 }, Hit::OptionTab(true)));
        for i in 0..rows {
            let y = r.y + 1 + u16::try_from(first + i).unwrap_or(0);
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

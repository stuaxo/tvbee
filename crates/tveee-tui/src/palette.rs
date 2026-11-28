//! The command palette (`:` or Ctrl-P): every action the enabled plugins
//! offer, found by typing a few letters of it (fuzzy, like an editor's).

use super::*;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use tveee_core::actions::Run;

/// How many matches the palette lists at once.
const ROWS: usize = 12;

/// What the ui itself can do, listed in the palette with the plugins'
/// actions (as ui: ...).
/// The ui's own entries: what it says, its key on the main screen ("": none),
/// and what it does. (The music visualiser is the media.visualiser action.)
pub const UI: &[(&str, &str, Input)] = &[
    ("ui: show the log (what tveee did lately)", "", Input::ShowLogs),
    ("ui: all keys (help)", "?", Input::ToggleHelp),
    ("ui: live TV guide", "g", Input::OpenGuide),
    ("ui: the list the app shows (Kodi's folder)", "t", Input::OpenList),
    ("ui: launch an app", "a", Input::OpenApps),
    ("ui: search the app in front", "/", Input::OpenSearch),
    ("ui: options", ",", Input::OpenOptions),
    ("ui: preview on/off", "v", Input::TogglePreview),
    ("ui: system info on/off", "i", Input::ToggleInfo),
    ("ui: show the whole link / shorten it", "u", Input::ToggleUrl),
    ("ui: copy the link", "y", Input::CopyUrl),
    ("ui: open the link in the browser", "", Input::OpenUrl),
    ("ui: remote / player mode", "tab", Input::ToggleMode),
    ("ui: quit", "q", Input::Quit),
];

/// A palette entry: a plugin's action (index into the actions), or
/// something the ui does (index into [`UI`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    Action(usize),
    Ui(usize),
    /// What's typed is a link: open it on the TV.
    Link,
}

/// Whether `query` is a link to open (pasted, say): a web address, or one
/// starting www.
pub fn is_link(query: &str) -> bool {
    let q = query.trim();
    !q.contains(char::is_whitespace) && (q.starts_with("http://") || q.starts_with("https://") || q.starts_with("www."))
}

/// What matches `query`, best first. Only actions that can run here (ones
/// that print for themselves need the command line). An empty query lists
/// the plugins' actions in their order, then the ui's.
/// How relevant an action is now: 0 the app in front's own (Kodi's, or the
/// guide's for live TV), 1 playback while something plays, 2 anything
/// else, 3 for another kind of thing playing (subtitles while music plays).
#[derive(Default, Clone)]
pub struct Now {
    /// The app in front's provider, and whether it has a guide (live TV).
    pub provider: Option<(&'static str, bool)>,
    /// What's playing ("video", "audio"), if anything is.
    pub playing: Option<&'static str>,
    /// The TV is in standby: waking it is what's wanted.
    pub standby: bool,
}

impl Now {
    pub fn rank(&self, a: &ActionDescriptor) -> u8 {
        if self.standby && a.id == "power.wake" {
            return 0;
        }
        let own = self.provider.is_some_and(|(id, guide)| a.category == id || (guide && a.category == "guide"));
        match (own, a.media) {
            (_, Some(m)) if self.playing != Some(m) => 3,
            (true, _) => 0,
            _ if a.category == "media" && self.playing.is_some() => 1,
            _ => 2,
        }
    }
}

pub fn palette_matches(actions: &[ActionDescriptor], query: &str) -> Vec<Item> {
    palette_ranked(actions, query, &Now::default())
}

/// The palette's items for `query`, the best matches first, and among
/// equals (or with nothing typed) the most relevant now ([`Now::rank`]).
pub fn palette_ranked(actions: &[ActionDescriptor], query: &str, now: &Now) -> Vec<Item> {
    // a link typed or pasted: opening it is what's wanted
    if is_link(query) {
        return vec![Item::Link];
    }
    let rank = |item: &Item| match item {
        Item::Link => 0,
        Item::Action(i) => now.rank(&actions[*i]),
        Item::Ui(_) => 2,
    };
    // (what's searched: the title as shown, and the id `tveee do` knows)
    let all = actions
        .iter()
        .enumerate()
        .filter(|(_, a)| matches!(a.run, Run::Do(_)))
        .map(|(i, a)| (Item::Action(i), format!("{} {}", a.title, a.id)))
        .chain(UI.iter().enumerate().map(|(i, (title, _, _))| (Item::Ui(i), title.to_string())));
    let query = query.trim();
    if query.is_empty() {
        // the most relevant first; the rest in their order
        let mut items: Vec<Item> = all.map(|(item, _)| item).collect();
        items.sort_by_key(|i| rank(i));
        return items;
    }
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let mut matcher = Matcher::new(Config::DEFAULT);
    let mut buf = Vec::new();
    let mut scored: Vec<(u32, usize, Item)> =
        all.enumerate().filter_map(|(order, (item, hay))| pattern.score(Utf32Str::new(&hay, &mut buf), &mut matcher).map(|s| (s, order, item))).collect();
    // best first; equal scores the most relevant, then their order
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(rank(&a.2).cmp(&rank(&b.2))).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, _, item)| item).collect()
}

/// The command palette's own state.
#[derive(Default)]
pub struct PaletteView {
    /// What's been typed to find a command.
    pub query: String,
    /// The pop-up it opened over (the log, the guide, ...): back to it when
    /// it closes, its own text as it was.
    pub under: Option<Overlay>,
}

impl App {
    pub(super) fn open_palette(&mut self) {
        // over nothing: nothing to go back to
        self.palette.under = None;
        self.palette.query.clear();
        self.overlay = Overlay::Palette(0);
    }

    /// Whether this key opens the commands over the pop-up that's open: ^P
    /// anywhere, : where it isn't typed (a search, a filter, a prompt).
    pub(super) fn palette_wanted(&self, code: KeyCode, mods: KeyModifiers) -> bool {
        let typing = matches!(
            self.overlay,
            Overlay::Search(_) | Overlay::Prompt(_) | Overlay::Pin | Overlay::Guide { .. } | Overlay::AmbiColour { .. } | Overlay::SaveLog
        );
        let open = !matches!(self.overlay, Overlay::None | Overlay::Palette(_));
        open && match code {
            KeyCode::Char('p') => mods.contains(KeyModifiers::CONTROL),
            KeyCode::Char(':') => !typing,
            _ => false,
        }
    }

    /// Open the commands over the pop-up that's open, to go back to it.
    pub(super) fn palette_over(&mut self) {
        // what it's over keeps its own text (a filter, a name typed)
        self.palette.under = Some(self.overlay);
        self.palette.query.clear();
        self.overlay = Overlay::Palette(0);
    }

    /// The palette closed: back to what it was over, if anything.
    pub(super) fn palette_closed(&mut self) {
        if let Some(o) = self.palette.under.take()
            && self.overlay == Overlay::None
        {
            self.overlay = o;
        }
    }

    /// What's listed now, for what's been typed.
    pub(super) fn palette_list(&self) -> Vec<Item> {
        palette_ranked(&self.actions, &self.palette.query, &self.palette_now())
    }

    /// What makes an action relevant now: the app in front, what's playing.
    pub(super) fn palette_now(&self) -> Now {
        let snap = self.st.snap.as_ref();
        let pkg = snap.and_then(|s| s.focus.as_deref()).map(|f| f.split('/').next().unwrap_or(f).to_string());
        let provider = pkg.as_deref().and_then(tveee_core::provider::for_package).map(|p| (p.id(), p.guide().is_some()));
        let playing = snap.and_then(|s| s.activity.as_ref()).and_then(|a| a.media);
        Now { provider, playing, standby: snap.is_some_and(|s| s.standby) }
    }

    /// Keys in the palette: typing narrows it, ↑/↓ choose, ⏎ runs (or asks
    /// for what it needs, or asks first), Esc closes.
    pub(super) fn palette_key(&mut self, sel: usize, code: KeyCode, mods: KeyModifiers, actions: &Handle) {
        let n = self.palette_list().len();
        match code {
            // Esc: what's typed first, then the palette
            KeyCode::Esc if !self.palette.query.is_empty() => {
                self.palette.query.clear();
                self.overlay = Overlay::Palette(0);
            }
            KeyCode::Esc => {
                self.overlay = Overlay::None;
                self.palette_closed();
            }
            c if super::listnav::is_move(c) => {
                let to = super::listnav::step(sel, n, c).unwrap_or(sel);
                self.overlay = Overlay::Palette(to);
            }
            KeyCode::Enter => match self.palette_list().get(sel) {
                Some(&Item::Action(i)) => {
                    let (next, action) = choose(&self.actions[i]);
                    self.apply_overlay(next, action, actions);
                    // run, and nothing new opened: back to what was under
                    if !matches!(self.overlay, Overlay::Prompt(_) | Overlay::Confirm { .. }) {
                        self.palette_closed();
                    }
                }
                Some(&Item::Ui(i)) => {
                    self.overlay = Overlay::None;
                    if self.apply(UI[i].2.clone(), actions) {
                        self.quit = true;
                    }
                    self.palette_closed();
                }
                Some(&Item::Link) => {
                    let link = self.palette.query.trim().to_string();
                    self.note = format!("opening {link}...");
                    let _ = actions.dispatch("apps.open_link", vec![link]);
                    self.overlay = Overlay::None;
                    self.palette_closed();
                }
                None => {}
            },
            KeyCode::Backspace => {
                self.palette.query.pop();
                self.overlay = Overlay::Palette(0);
            }
            KeyCode::Char('u') if mods.contains(KeyModifiers::CONTROL) => {
                self.palette.query.clear();
                self.overlay = Overlay::Palette(0);
            }
            KeyCode::Char(c) if !mods.contains(KeyModifiers::CONTROL) => {
                self.palette.query.push(c);
                self.overlay = Overlay::Palette(0);
            }
            _ => {}
        }
    }

    pub(super) fn draw_palette(&mut self, f: &mut Frame, sel: usize) {
        let list = self.palette_list();
        let width = 64usize;
        let mut text = vec![Line::from(vec![
            Span::styled("> ", Style::new().fg(Color::DarkGray)),
            Span::styled(format!("{}█", self.palette.query), Style::new().fg(Color::White)),
        ])];
        text.push(Line::from("─".repeat(width)).fg(Color::DarkGray));
        // a window of ROWS around the highlight
        let first = sel.saturating_sub(ROWS - 1).min(list.len().saturating_sub(ROWS));
        for (row, &item) in list.iter().enumerate().skip(first).take(ROWS) {
            // the right column: its key (a menu key after `.`), then its category,
            // so the palette teaches the shortcuts
            let (left, key, right) = match item {
                Item::Action(i) => {
                    let a = &self.actions[i];
                    let more = if needs_input(a) || a.confirm.is_some() { "…" } else { "" };
                    (format!("{}{more}", a.title), a.key.map(|k| format!(".{k}")).unwrap_or_default(), a.category)
                }
                Item::Ui(i) => (UI[i].0.to_string(), UI[i].1.to_string(), "ui"),
                Item::Link => ("open this link on the TV".to_string(), String::new(), "apps"),
            };
            let right_text = if key.is_empty() { right.to_string() } else { format!("{key}  {right}") };
            let gap = width.saturating_sub(left.chars().count() + right_text.chars().count() + 2);
            let line = Line::from(vec![
                Span::raw(format!(" {left}")),
                Span::raw(" ".repeat(gap)),
                Span::styled(format!("{right_text} "), Style::new().fg(Color::DarkGray)),
            ]);
            text.push(if row == sel { line.reversed() } else { line });
        }
        if list.is_empty() {
            text.push(Line::from(" nothing matches").italic().fg(Color::DarkGray));
        }
        text.push(Line::default());
        text.push(Line::from("type to find   …: asks first").fg(Color::DarkGray));
        let count = format!(" {} ", list.len());
        let r = popup_titled(f, " commands ", &count, text, Color::Cyan);
        self.hits.push((r, Hit::Popup));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tveee_core::actions::{Cx, Output};

    fn ok(_: &Cx, _: &[String]) -> tveee_core::error::Result<Output> {
        Ok(Output::default())
    }

    fn printed(_: &tveee_core::Ctx, _: &[String]) -> tveee_core::error::Result<()> {
        Ok(())
    }

    fn actions() -> Vec<ActionDescriptor> {
        vec![
            ActionDescriptor::new("kodi.subtitles", "kodi", "kodi: next subtitles", ok),
            ActionDescriptor::new("kodi.audio", "kodi", "kodi: next audio track", ok),
            ActionDescriptor::terminal("apps.apps", "apps", "list apps", printed),
            ActionDescriptor::new("power.off", "power", "power off", ok).confirm("sure?"),
            ActionDescriptor::new("media.volume", "media", "volume", ok).cli("vol", "up|down|<level>", "change volume"),
        ]
    }

    #[test]
    fn finds_by_a_few_letters() {
        let a = actions();
        let titles = |q| {
            palette_matches(&a, q)
                .into_iter()
                .filter_map(|i| match i {
                    Item::Action(i) => Some(a[i].id),
                    Item::Ui(_) | Item::Link => None,
                })
                .collect::<Vec<_>>()
        };
        // everything that can run here, in order; not what prints for itself
        assert_eq!(titles(""), ["kodi.subtitles", "kodi.audio", "power.off", "media.volume"]);
        // and the ui's own, after them
        assert_eq!(palette_matches(&a, "").last(), Some(&Item::Ui(UI.len() - 1)));
        assert_eq!(palette_matches(&a, "show the log").first(), Some(&Item::Ui(0)));
        assert_eq!(titles("subs"), ["kodi.subtitles"]);
        assert_eq!(titles("aud"), ["kodi.audio"]);
        assert_eq!(titles("pwr off"), ["power.off"]);
        // the id counts too
        assert_eq!(titles("media.vol"), ["media.volume"]);
        assert!(titles("zzzz").is_empty());
    }

    #[test]
    fn choosing_asks_for_what_it_needs() {
        let a = actions();
        assert_eq!(choose(&a[0]), (Overlay::None, Some(run("kodi.subtitles"))));
        assert_eq!(choose(&a[3]), (Overlay::Confirm { id: "power.off", yes: false }, None));
        // a level it can't do without
        assert_eq!(choose(&a[4]), (Overlay::Prompt("media.volume"), None));
    }

    #[test]
    fn the_relevant_first() {
        use tveee_core::actions::ActionDescriptor as A;
        let ok = |_: &tveee_core::actions::Cx, _: &[String]| Ok("".into());
        let a = vec![
            A::new("power.sleep", "power", "sleep", ok),
            A::new("media.next", "media", "next track", ok),
            A::new("kodi.subtitles", "kodi", "kodi: subtitles", ok).while_playing("video"),
            A::new("kodi.find", "kodi", "kodi: find in the library", ok),
        ];
        let order =
            |now: &Now| palette_ranked(&a, "", now).into_iter().filter_map(|i| if let Item::Action(n) = i { Some(a[n].id) } else { None }).collect::<Vec<_>>();
        // nothing in front: as listed (subtitles last: nothing's playing)
        assert_eq!(order(&Now::default()), ["power.sleep", "media.next", "kodi.find", "kodi.subtitles"]);
        // Kodi playing music: Kodi's own, then playback; subtitles last
        let music = Now { provider: Some(("kodi", false)), playing: Some("audio"), standby: false };
        assert_eq!(order(&music), ["kodi.find", "media.next", "power.sleep", "kodi.subtitles"]);
        // Kodi playing video: subtitles are relevant now
        let video = Now { provider: Some(("kodi", false)), playing: Some("video"), standby: false };
        assert_eq!(order(&video)[..2], ["kodi.subtitles", "kodi.find"]);
        // in standby, waking it first
        let mut with_wake = a.clone();
        with_wake.push(A::new("power.wake", "power", "wake", ok));
        let asleep = Now { standby: true, ..Now::default() };
        let first = palette_ranked(&with_wake, "", &asleep).into_iter().next();
        assert_eq!(first, Some(Item::Action(with_wake.len() - 1)));
        // typed: the match comes first, relevance breaks ties
        assert_eq!(palette_ranked(&a, "sleep", &music).first(), Some(&Item::Action(0)));
    }

    #[test]
    fn a_link_pasted_opens_it() {
        assert!(is_link("https://youtu.be/gwLRWgOWhBs") && is_link("www.example.com/x") && is_link("  http://a/b  "));
        assert!(!is_link("open http://a") && !is_link("youtube") && !is_link(""));
        assert_eq!(palette_matches(&[], "https://youtu.be/x"), vec![Item::Link]);
    }
}

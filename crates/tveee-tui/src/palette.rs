//! The command palette (`:` or Ctrl-P): every action the enabled plugins
//! offer, found by typing a few letters of it (fuzzy, like an editor's).

use super::*;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use tveee_core::actions::Run;

/// How many matches the palette lists at once.
const ROWS: usize = 12;

/// The actions matching `query`, best first, as indices into `actions`.
/// Only those that can run here: ones that print for themselves need the
/// command line. An empty query lists them all, in the plugins' order.
pub fn palette_matches(actions: &[ActionDescriptor], query: &str) -> Vec<usize> {
    let runnable = actions.iter().enumerate().filter(|(_, a)| matches!(a.run, Run::Do(_)));
    let query = query.trim();
    if query.is_empty() {
        return runnable.map(|(i, _)| i).collect();
    }
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let mut matcher = Matcher::new(Config::DEFAULT);
    let mut buf = Vec::new();
    let mut scored: Vec<(u32, usize)> = runnable
        .filter_map(|(i, a)| {
            // the title as shown, and the id `tveee do` knows it by
            let hay = format!("{} {}", a.title, a.id);
            pattern.score(Utf32Str::new(&hay, &mut buf), &mut matcher).map(|s| (s, i))
        })
        .collect();
    // best first; equal scores keep the plugins' order
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, i)| i).collect()
}

impl App {
    pub(super) fn open_palette(&mut self) {
        self.prompt.clear();
        self.overlay = Overlay::Palette(0);
    }

    /// What's listed now, for what's been typed.
    fn palette_list(&self) -> Vec<usize> {
        palette_matches(&self.actions, &self.prompt)
    }

    /// Keys in the palette: typing narrows it, ↑/↓ choose, ⏎ runs (or asks
    /// for what it needs, or asks first), Esc closes.
    pub(super) fn palette_key(&mut self, sel: usize, code: KeyCode, mods: KeyModifiers, actions: &Handle) {
        let n = self.palette_list().len();
        match code {
            KeyCode::Esc => self.overlay = Overlay::None,
            KeyCode::Up => self.overlay = Overlay::Palette(sel.checked_sub(1).unwrap_or(n.saturating_sub(1))),
            KeyCode::Down => self.overlay = Overlay::Palette(if sel + 1 >= n { 0 } else { sel + 1 }),
            KeyCode::Enter => {
                let Some(&i) = self.palette_list().get(sel) else { return };
                let (next, action) = choose(&self.actions[i]);
                self.apply_overlay(next, action, actions);
            }
            KeyCode::Backspace => {
                self.prompt.pop();
                self.overlay = Overlay::Palette(0);
            }
            KeyCode::Char('u') if mods.contains(KeyModifiers::CONTROL) => {
                self.prompt.clear();
                self.overlay = Overlay::Palette(0);
            }
            KeyCode::Char(c) if !mods.contains(KeyModifiers::CONTROL) => {
                self.prompt.push(c);
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
            Span::styled(format!("{}█", self.prompt), Style::new().fg(Color::White)),
        ])];
        text.push(Line::from("─".repeat(width)).fg(Color::DarkGray));
        // a window of ROWS around the highlight
        let first = sel.saturating_sub(ROWS - 1).min(list.len().saturating_sub(ROWS));
        for (row, &i) in list.iter().enumerate().skip(first).take(ROWS) {
            let a = &self.actions[i];
            let more = if needs_input(a) || a.confirm.is_some() { "…" } else { "" };
            let left = format!("{}{more}", a.title);
            let right = a.category;
            let gap = width.saturating_sub(left.chars().count() + right.chars().count() + 2);
            let line = Line::from(vec![
                Span::raw(format!(" {left}")),
                Span::raw(" ".repeat(gap)),
                Span::styled(format!("{right} "), Style::new().fg(Color::DarkGray)),
            ]);
            text.push(if row == sel { line.reversed() } else { line });
        }
        if list.is_empty() {
            text.push(Line::from(" nothing matches").italic().fg(Color::DarkGray));
        }
        text.push(Line::default());
        text.push(Line::from("type to find  ↑/↓ choose  ⏎ run  esc close   …: asks first").fg(Color::DarkGray));
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
        let titles = |q| palette_matches(&a, q).into_iter().map(|i| a[i].id).collect::<Vec<_>>();
        // everything that can run here, in order; not what prints for itself
        assert_eq!(titles(""), ["kodi.subtitles", "kodi.audio", "power.off", "media.volume"]);
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
}

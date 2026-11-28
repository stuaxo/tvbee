//! The log pop-up: what tveee did lately, newest at the bottom. A line is
//! highlighted and moved with the list keys; whole lines are selected
//! (shift+↑/↓, v then the arrows, a click and shift+click or a drag) and
//! copied (y or ⏎); s saves the whole log to a file, its name asked for.
//!
//! Lines are known by their number since tveee started
//! ([`tveee_core::log::recent_numbered`]), so a highlight and a selection
//! stay on their lines while newer ones arrive.

use super::*;

/// A day's date and the time, local, as a file name's part: 20261127-2130.
pub fn stamp(unix: i64, offset: i64) -> String {
    let t = unix + offset;
    let (days, secs) = (t.div_euclid(86400), t.rem_euclid(86400));
    // days since 1970-01-01 to a civil date (Howard Hinnant's algorithm)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}{m:02}{d:02}-{:02}{:02}", secs / 3600, secs % 3600 / 60)
}

/// The file name offered for saving the log.
pub fn default_log_name() -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    format!("tveee-log-{}.txt", stamp(now, tveee_core::builtin::guide::utc_offset()))
}

/// Where a typed name points: `~/` is home; anything else as typed
/// (relative: where tveee was started).
pub fn log_path(name: &str) -> std::path::PathBuf {
    match (name.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => std::path::PathBuf::from(home).join(rest),
        _ => std::path::PathBuf::from(name),
    }
}

/// The lines from `a` to `b` (either way round), by number, of `lines`
/// (numbered from `first`), joined.
pub fn lines_between(first: usize, lines: &[String], a: usize, b: usize) -> String {
    let (lo, hi) = (a.min(b), a.max(b));
    (lo..=hi).filter_map(|n| n.checked_sub(first).and_then(|i| lines.get(i))).cloned().collect::<Vec<_>>().join("\n")
}

/// The log pop-up's own state.
#[derive(Default)]
pub struct LogView {
    /// The log as it was before saving it was asked for (back to it after).
    pub back: Option<Overlay>,
    /// The top line shown (by number), so the view only moves as far as
    /// keeps the highlight in it.
    pub top: Option<usize>,
    /// A drag over the lines started (it selects).
    pub drag: bool,
    /// The file name ⏎ was pressed for that's there already (⏎ again
    /// writes over it).
    pub save_confirm: Option<String>,
}

impl App {
    /// Keys in the log: the list keys move the highlight (shift: select),
    /// v starts or ends a selection, y/⏎ copy, s saves, End follows the
    /// newest, Esc lets go of a selection then closes.
    pub(super) fn logs_key(&mut self, at: Option<usize>, from: Option<usize>, code: KeyCode, mods: KeyModifiers) {
        let (first, lines) = tveee_core::log::recent_numbered();
        let last = (first + lines.len()).saturating_sub(1);
        // the highlight: following the newest, it starts on it
        let cur = at.unwrap_or(last).clamp(first, last.max(first));
        let shift = mods.contains(KeyModifiers::SHIFT);
        let moved = |to: usize| to.clamp(first, last.max(first));
        let to = match code {
            KeyCode::Up => Some(moved(cur.saturating_sub(1))),
            KeyCode::Down => Some(moved(cur + 1)),
            KeyCode::PageUp => Some(moved(cur.saturating_sub(listnav::PAGE))),
            KeyCode::PageDown => Some(moved(cur + listnav::PAGE)),
            KeyCode::Home => Some(first),
            _ => None,
        };
        if let Some(to) = to {
            // shift (or a selection started with v) selects from where it was
            let from = if shift { from.or(Some(cur)) } else { from };
            self.overlay = Overlay::Logs { at: Some(to), from };
            return;
        }
        match code {
            KeyCode::End => self.overlay = Overlay::Logs { at: None, from: None },
            KeyCode::Char('v') => self.overlay = Overlay::Logs { at: Some(cur), from: if from.is_some() { None } else { Some(cur) } },
            KeyCode::Char('y') | KeyCode::Enter => {
                let text = lines_between(first, &lines, from.unwrap_or(cur), cur);
                let n = text.lines().count();
                self.note = links::copy_text(&text, &format!("{n} {}", if n == 1 { "line" } else { "lines" }));
                self.overlay = Overlay::Logs { at: Some(cur), from: None };
            }
            KeyCode::Char('s') => {
                self.log.back = Some(Overlay::Logs { at, from });
                self.log.save_confirm = None;
                self.prompt = default_log_name();
                self.overlay = Overlay::SaveLog;
            }
            KeyCode::Esc if from.is_some() => self.overlay = Overlay::Logs { at, from: None },
            KeyCode::Esc | KeyCode::Char('q') => self.overlay = Overlay::None,
            _ => {}
        }
    }

    /// A click on log line `n`: highlighted; with shift, or dragged to,
    /// selected from where the highlight was.
    pub(super) fn log_click(&mut self, n: usize, extend: bool) {
        let Overlay::Logs { at, from } = self.overlay else { return };
        self.overlay = if extend {
            let (first, lines) = tveee_core::log::recent_numbered();
            let cur = at.unwrap_or((first + lines.len()).saturating_sub(1));
            Overlay::Logs { at: Some(n), from: from.or(Some(cur)) }
        } else {
            Overlay::Logs { at: Some(n), from: None }
        };
    }

    /// Typing the file to save the log to: ⏎ saves (asking again before
    /// writing over a file), Esc goes back to the log.
    pub(super) fn save_log_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        match code {
            KeyCode::Esc => self.overlay = self.log.back.take().unwrap_or(Overlay::None),
            KeyCode::Enter => {
                let name = self.prompt.trim().to_string();
                if name.is_empty() {
                    self.note = "type a file name to save the log to".into();
                    return;
                }
                let path = log_path(&name);
                if path.exists() && self.log.save_confirm.as_deref() != Some(name.as_str()) {
                    self.note = format!("{} is there already: ⏎ again writes over it", path.display());
                    self.log.save_confirm = Some(name);
                    return;
                }
                let (_, lines) = tveee_core::log::recent_numbered();
                let mut text = lines.join("\n");
                text.push('\n');
                self.note = match std::fs::write(&path, text) {
                    Ok(()) => format!("saved {} lines of the log to {}", lines.len(), path.display()),
                    Err(e) => format!("couldn't save the log to {}: {e}", path.display()),
                };
                self.log.save_confirm = None;
                self.overlay = self.log.back.take().unwrap_or(Overlay::None);
            }
            KeyCode::Backspace => {
                self.prompt.pop();
                self.log.save_confirm = None;
            }
            KeyCode::Char('u') if mods.contains(KeyModifiers::CONTROL) => self.prompt.clear(),
            KeyCode::Char(c) if !mods.contains(KeyModifiers::CONTROL) => {
                self.prompt.push(c);
                self.log.save_confirm = None;
            }
            _ => {}
        }
    }

    pub(super) fn draw_logs(&mut self, f: &mut Frame, at: Option<usize>, from: Option<usize>) {
        let area = f.area();
        let r = ratatui::layout::Rect {
            x: area.x + area.width / 20,
            y: area.y + area.height / 10,
            width: area.width - area.width / 10,
            height: area.height - area.height / 5,
        };
        let (first, lines) = tveee_core::log::recent_numbered();
        let room = usize::from(r.height.saturating_sub(2)).max(1);
        let last = (first + lines.len()).saturating_sub(1);
        // the top line shown: following the newest, the end; else moved
        // only as far as keeps the highlight in view
        let top = match at {
            None => (first + lines.len()).saturating_sub(room).max(first),
            Some(cur) => {
                let top = self.log.top.unwrap_or(cur).max(first);
                let top = if cur < top {
                    cur
                } else if cur >= top + room {
                    cur + 1 - room
                } else {
                    top
                };
                top.min((last + 1).saturating_sub(room).max(first))
            }
        };
        self.log.top = at.map(|_| top);
        let (lo, hi) = match (at, from) {
            (Some(cur), Some(f)) => (cur.min(f), cur.max(f)),
            (Some(cur), None) => (cur, cur),
            _ => (usize::MAX, 0),
        };
        let mut rows: Vec<(usize, Line)> = Vec::new();
        let shown: Vec<Line> = if lines.is_empty() {
            vec![Line::from("nothing logged yet").italic()]
        } else {
            (top..(top + room).min(last + 1))
                .map(|n| {
                    let text = lines[n - first].clone();
                    let line = Line::from(text);
                    let line = if (lo..=hi).contains(&n) { line.reversed() } else { line };
                    rows.push((n, line.clone()));
                    line
                })
                .collect()
        };
        f.render_widget(ratatui::widgets::Clear, r);
        let picked = if lo <= hi { hi - lo + 1 } else { 0 };
        let title = match (at, from) {
            (None, _) => format!(" log - {} lines, following the newest (TVEEE_LOG=<file> keeps them) ", lines.len()),
            (Some(_), Some(_)) => format!(" log - {picked} lines selected ",),
            (Some(cur), None) => format!(" log - line {} of {} ", cur - first + 1, lines.len()),
        };
        let keys = if from.is_some() { " y copy them  s save the log " } else { " shift/v select  y copy  s save  end follows new lines " };
        let block = Block::bordered().title(title).title_bottom(Line::from(keys).right_aligned()).border_style(Style::new().fg(Color::DarkGray));
        f.render_widget(Paragraph::new(shown).block(block), r);
        let bar = scrollbar(f, Bar::Log, r.right().saturating_sub(1), r.y + 1, r.height.saturating_sub(2), lines.len(), top.saturating_sub(first));
        self.hits.push((r, Hit::Popup));
        self.hits.extend(bar.map(|b| (b.area, Hit::Scrollbar(b))));
        for (i, (n, _)) in rows.iter().enumerate() {
            let y = r.y + 1 + u16::try_from(i).unwrap_or(0);
            self.hits.push((ratatui::layout::Rect { x: r.x + 1, y, width: r.width.saturating_sub(2), height: 1 }, Hit::LogLine(*n)));
        }
    }

    pub(super) fn draw_save_log(&mut self, f: &mut Frame) {
        let text = vec![
            Line::from("Save the log (all the lines kept) to:"),
            Line::from(vec![Span::styled(format!("{}█", self.prompt), Style::new().fg(Color::White).bg(Color::DarkGray))]),
            Line::default(),
            Line::from("~/ is your home; else where tveee was started").fg(Color::DarkGray),
        ];
        let r = popup(f, " save the log ", text, Color::Blue);
        self.hits.push((r, Hit::Popup));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps() {
        // 2026-11-27 21:30 UTC
        assert_eq!(stamp(1_795_815_000, 0), "20261127-2130");
        // an hour ahead
        assert_eq!(stamp(1_795_815_000, 3600), "20261127-2230");
        assert_eq!(stamp(0, 0), "19700101-0000");
        assert_eq!(stamp(951_782_400, 0), "20000229-0000");
    }

    #[test]
    fn lines_picked() {
        let lines: Vec<String> = ["a", "b", "c", "d"].iter().map(std::string::ToString::to_string).collect();
        // numbered from 10: lines 11 to 13, either way round
        assert_eq!(lines_between(10, &lines, 11, 13), "b\nc\nd");
        assert_eq!(lines_between(10, &lines, 13, 11), "b\nc\nd");
        assert_eq!(lines_between(10, &lines, 12, 12), "c");
        // gone (pushed out): what's left
        assert_eq!(lines_between(10, &lines, 8, 10), "a");
    }

    #[test]
    fn paths() {
        assert_eq!(log_path("x.txt"), std::path::PathBuf::from("x.txt"));
        if let Some(home) = std::env::var_os("HOME") {
            assert_eq!(log_path("~/logs/x.txt"), std::path::PathBuf::from(home).join("logs/x.txt"));
        }
    }
}

//! The preview panel: a lane that makes pictures of what's on, and drawing them.

use super::*;

/// What a player said it has a picture of, as of a refresh.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct ArtNow {
    pub(super) pkg: String,
    pub(super) art: String,
    /// The picture changes as it plays (a video frame): ask for it again,
    /// at the position now.
    pub(super) moves: bool,
    pub(super) position: Option<f64>,
    pub(super) paused: bool,
    /// When that was read.
    pub(super) at: Instant,
}

impl ArtNow {
    /// How far in it is now (moving on from the refresh if it's playing).
    pub(super) fn now(&self) -> Option<f64> {
        let p = self.position?;
        Some(if self.paused { p } else { p + self.at.elapsed().as_secs_f64() })
    }
}

/// Said when the preview comes out black.
pub(super) const HIDDEN_VIDEO: &str = "black: the TV plays this video on a layer pictures can't see";

/// Whether to ask the terminal how it draws pictures: `TVEEE_PICTURES=detect`
/// (or `halfblocks` never), else when it's one known to draw them.
pub fn picture_terminal(env: impl Fn(&str) -> Option<String>) -> bool {
    match env("TVEEE_PICTURES").as_deref() {
        Some("detect") => return true,
        Some("halfblocks") => return false,
        _ => {}
    }
    let term = env("TERM").unwrap_or_default();
    let program = env("TERM_PROGRAM").unwrap_or_default();
    env("KITTY_WINDOW_ID").is_some()
        || env("WEZTERM_EXECUTABLE").is_some()
        || env("KONSOLE_VERSION").is_some()
        || ["kitty", "foot", "mlterm", "wezterm"].iter().any(|t| term.contains(t))
        || ["iTerm.app", "WezTerm", "ghostty", "rio"].contains(&program.as_str())
}

/// Tell app `pkg`'s player the preview has moved on (stop its downloads).
pub(super) fn stop_pictures(pkg: Option<String>) {
    if let Some(player) = pkg.as_deref().and_then(crate::provider::for_package).and_then(|p| p.player()) {
        player.stop();
    }
}

/// The preview lane: pictures of what's on while the panel is open.
///
/// YouTube: its storyboard frame at once, then frames from a small copy of
/// the video (downloaded meanwhile) at the TV's position, which keeps them in
/// step. Anything else: scrcpy's pictures (the default for apps without
/// their own way), or a screenshot every few seconds without it.
pub(super) fn preview_lane(shared: &Shared, tx: &Sender<Msg>) {
    use crate::preview::{screencap, Scrcpy};
    let tv = shared.tv();
    let open = || shared.preview.load(Ordering::Relaxed) && !shared.stopping();
    let send = |m: Msg| tx.send(m).is_ok();
    loop {
        while !open() {
            if shared.stopping() {
                return;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        let mut live: Option<Scrcpy> = None;
        // when scrcpy last failed (not again for a while)
        let mut scrcpy_failed: Option<Instant> = None;
        let mut still_at: Option<Instant> = None;
        // the last picture sent (what, and where in it for moving ones)
        let mut shown_art: Option<(String, i64)> = None;
        // the app the pictures last came from
        let mut pictures_from: Option<String> = None;
        while open() {
            // an app's player has a picture of what's on (YouTube's frames,
            // Kodi's artwork: its video is on a layer scrcpy can't see)
            let art = lock(&shared.art).clone();
            // moved on from an app whose player had work going for pictures
            let now_pkg = art.as_ref().map(|a| a.pkg.clone());
            if pictures_from != now_pkg {
                stop_pictures(pictures_from.take());
                pictures_from = now_pkg;
            }
            if let Some(a) = art {
                live = None;
                let at = a.now();
                // a still picture once; a moving one as the position moves
                let key = (a.art.clone(), if a.moves { at.map_or(0, |t| (t * 4.0) as i64) } else { 0 });
                if shown_art.as_ref() != Some(&key) {
                    let provider = crate::provider::for_package(&a.pkg);
                    let got = provider.and_then(|p| p.player()).map(|p| p.picture(&tv, &a.art, at));
                    match got.map(|g| g.or_fallback()) {
                        Some(Ok(Some(bytes))) => match image::load_from_memory(&bytes) {
                            Ok(img) => {
                                let rgb = img.to_rgb8();
                                let source = provider.map_or("the app", |p| p.name());
                                let p = crate::preview::Picture { width: rgb.width(), height: rgb.height(), rgb: rgb.into_raw(), source };
                                if !send(Msg::Preview(p)) {
                                    return;
                                }
                                let _ = send(Msg::PreviewNote(String::new()));
                            }
                            Err(e) => {
                                let _ = send(Msg::PreviewNote(format!("couldn't read the picture: {e}")));
                            }
                        },
                        Some(Err(e)) => {
                            let _ = send(Msg::PreviewNote(e.to_string()));
                        }
                        // not ready yet (a video being fetched, say)
                        _ => {
                            let _ = send(Msg::PreviewNote("getting a picture of it...".into()));
                        }
                    }
                    shown_art = Some(key);
                }
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
            shown_art = None;
            // anything else: scrcpy, the default
            let ready = crate::preview::scrcpy_ready();
            if live.is_none() && ready && scrcpy_failed.is_none_or(|t| t.elapsed() >= Duration::from_secs(15)) {
                let _ = send(Msg::PreviewNote("starting scrcpy on the TV...".into()));
                match Scrcpy::start(&tv) {
                    Ok(s) => {
                        live = Some(s);
                        let _ = send(Msg::PreviewNote(String::new()));
                    }
                    Err(e) => {
                        scrcpy_failed = Some(Instant::now());
                        let _ = send(Msg::PreviewNote(format!("{e}; screenshots instead")));
                    }
                }
            }
            if let Some(s) = live.as_mut() {
                match s.pictures.recv_timeout(Duration::from_millis(300)) {
                    Ok(mut p) => {
                        // the newest of what's arrived
                        while let Ok(q) = s.pictures.try_recv() {
                            p = q;
                        }
                        let _ = send(Msg::PreviewNote(if p.black() { HIDDEN_VIDEO.into() } else { String::new() }));
                        if !send(Msg::Preview(p)) {
                            return;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => live = None,
                }
                if live.as_mut().is_some_and(|s| !s.running()) {
                    live = None;
                    scrcpy_failed = Some(Instant::now());
                    let _ = send(Msg::PreviewNote("scrcpy stopped; trying again shortly".into()));
                }
                continue;
            }
            // no scrcpy: a screenshot now and then (video comes out black)
            if still_at.is_none_or(|t| t.elapsed() >= Duration::from_secs(4)) {
                still_at = Some(Instant::now());
                if !ready {
                    let _ = send(Msg::PreviewNote("screenshots (install scrcpy and ffmpeg for video too)".into()));
                }
                if let Ok(p) = screencap(&tv) {
                    if p.black() {
                        let _ = send(Msg::PreviewNote(HIDDEN_VIDEO.into()));
                    }
                    if !send(Msg::Preview(p)) {
                        return;
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        // closed: the server stops with it, and any download
        stop_pictures(pictures_from.take());
        drop(live);
    }
}

impl App {
    /// Rows the system panel wants, borders included.
    /// The preview panel: the picture, or what it's waiting for.
    pub(super) fn draw_preview(&mut self, f: &mut Frame, area: ratatui::layout::Rect) {
        let source = if self.preview_source.is_empty() { String::new() } else { format!(" ({})", self.preview_source) };
        let block = Block::bordered().title(format!(" preview{source} - v hides ")).border_style(Style::new().fg(Color::DarkGray));
        let inner = block.inner(area);
        f.render_widget(block, area);
        let note_h = u16::from(!self.preview_note.is_empty());
        let [pic, note] = Layout::vertical([Constraint::Min(1), Constraint::Length(note_h)]).areas(inner);
        match self.preview.as_mut() {
            Some(proto) => f.render_stateful_widget(ratatui_image::StatefulImage::new(), pic, proto),
            None => f.render_widget(Paragraph::new("getting a picture...").italic(), pic),
        }
        if note_h > 0 {
            f.render_widget(Paragraph::new(self.preview_note.clone()).fg(Color::DarkGray), note);
        }
    }
}

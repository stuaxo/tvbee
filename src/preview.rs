//! Pictures of what's on the TV, for the ui's preview panel.
//!
//! - YouTube: a copy of the video at its lowest quality (144p, a few MB),
//!   downloaded here with yt-dlp; each picture is the frame at the TV's
//!   position (read from the app every second), so it keeps in step,
//!   pauses and seeks with it. Its storyboard frames (small, every few
//!   seconds of video, fetched here too) fill in while the copy downloads.
//!
//! Everything else:
//!
//! - scrcpy's server, run on the TV at 320 pixels and one frame a second: the
//!   TV's hardware encoder sends H.264, which `ffmpeg` here turns back into
//!   pictures. It sees video too (`screencap` doesn't, on TVs that play it
//!   on a hardware layer). Needs scrcpy and ffmpeg installed here.
//! - `screencap`, raw, every few seconds: menus and apps only.

use crate::error::{failed, Result};
use crate::tv::Tv;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The size pictures are made at.
pub const WIDTH: u32 = 320;
pub const HEIGHT: u32 = 180;

/// A picture, RGB, `width` x `height`, and where it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
    pub source: &'static str,
}

impl Picture {
    /// Nearly all black: what video on the TV's hardware layer looks like to
    /// scrcpy and screenshots (some apps' video, live TV).
    pub fn black(&self) -> bool {
        let dark = self.rgb.chunks(3).filter(|p| p.iter().map(|c| u32::from(*c)).sum::<u32>() < 30).count();
        dark * 100 >= self.rgb.len() / 3 * 98
    }
}

fn program(name: &str) -> Option<PathBuf> {
    crate::tv::search_path(name, std::env::var_os("PATH").as_deref())
}

// --- scrcpy -----------------------------------------------------------------

/// scrcpy's server (the part that runs on the TV), and the version it must
/// be started with (scrcpy's own, which it's installed with).
pub fn scrcpy_server() -> Option<&'static (PathBuf, String)> {
    static FOUND: OnceLock<Option<(PathBuf, String)>> = OnceLock::new();
    FOUND
        .get_or_init(|| {
            let scrcpy = program("scrcpy")?;
            // scrcpy's own variable, then where packages put it
            let path = std::env::var_os("SCRCPY_SERVER_PATH").map(PathBuf::from).into_iter().chain(
                [
                    "/snap/scrcpy/current/usr/local/share/scrcpy/scrcpy-server",
                    "/usr/local/share/scrcpy/scrcpy-server",
                    "/usr/share/scrcpy/scrcpy-server",
                    "/opt/homebrew/share/scrcpy/scrcpy-server",
                ]
                .map(PathBuf::from),
            );
            let path = path.into_iter().find(|p| p.is_file())?;
            let out = std::process::Command::new(scrcpy).arg("--version").output().ok()?;
            let version = String::from_utf8_lossy(&out.stdout).split_whitespace().nth(1)?.to_string();
            Some((path, version))
        })
        .as_ref()
}

/// Whether the scrcpy way can work here.
pub fn scrcpy_ready() -> bool {
    scrcpy_server().is_some() && program("ffmpeg").is_some()
}

/// scrcpy's server running on the TV, sending pictures. Everything stops
/// (the server, ffmpeg, the port forward) when it's dropped.
pub struct Scrcpy {
    server: std::process::Child,
    ffmpeg: std::process::Child,
    _fwd: crate::tv::Forward,
    pub pictures: Receiver<Picture>,
}

impl Drop for Scrcpy {
    fn drop(&mut self) {
        let _ = self.server.kill();
        let _ = self.ffmpeg.kill();
        let _ = self.server.wait();
        let _ = self.ffmpeg.wait();
    }
}

const SERVER_ON_TV: &str = "/data/local/tmp/tveee-scrcpy.jar";

impl Scrcpy {
    /// Start it; pictures arrive on `pictures` (the first after a few seconds).
    pub fn start(tv: &Tv) -> Result<Scrcpy> {
        let Some((jar, version)) = scrcpy_server() else { return failed("preview: scrcpy isn't installed here") };
        let Some(ffmpeg) = program("ffmpeg") else { return failed("preview: ffmpeg isn't installed here") };
        tv.connect()?;
        let push = || tv.adb(&["push", &jar.to_string_lossy(), SERVER_ON_TV]);
        // a second go: it has failed once for no reason that stayed
        push().or_else(|_| {
            std::thread::sleep(Duration::from_millis(500));
            push()
        })?;
        let scid = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.subsec_nanos()) & 0x7fff_ffff;
        let fwd = tv.forward_abstract(&format!("scrcpy_{scid:08x}"))?;
        let server = tv.spawn_shell(&format!(
            "CLASSPATH={SERVER_ON_TV} app_process / com.genymobile.scrcpy.Server {version} scid={scid:08x} log_level=warn \
             tunnel_forward=true audio=false control=false video_codec=h264 max_size={WIDTH} max_fps=1 raw_stream=true cleanup=false"
        ))?;
        // nothing listening yet: the forward closes at once; listening: it
        // stays open (and quiet until the first picture)
        let deadline = Instant::now() + Duration::from_secs(15);
        let (mut socket, first) = loop {
            if Instant::now() > deadline {
                return failed("preview: scrcpy's server didn't start on the TV");
            }
            let mut s = std::net::TcpStream::connect(("127.0.0.1", fwd.port))?;
            s.set_read_timeout(Some(Duration::from_millis(500)))?;
            let mut b = [0u8; 1];
            match s.read(&mut b) {
                Ok(0) => std::thread::sleep(Duration::from_millis(200)),
                Ok(_) => break (s, b.to_vec()),
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => break (s, Vec::new()),
                Err(e) => return Err(e.into()),
            }
        };
        socket.set_read_timeout(None)?;
        let mut ff = std::process::Command::new(ffmpeg)
            // probe only the first packet (the TV may send nothing more while the
            // screen is still): a bigger probe waits, a tiny one can't find the
            // picture's size, and `-fflags nobuffer` stops pictures coming
            .args(["-loglevel", "error", "-flags", "low_delay", "-probesize", "100", "-analyzeduration", "0"])
            .args(["-f", "h264", "-i", "pipe:0", "-vf", &format!("scale={WIDTH}:{HEIGHT}"), "-f", "rawvideo", "-pix_fmt", "rgb24", "pipe:1"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        let (Some(mut to_ff), Some(mut from_ff)) = (ff.stdin.take(), ff.stdout.take()) else { return failed("preview: couldn't talk to ffmpeg") };
        // the TV's H.264 into ffmpeg
        std::thread::spawn(move || {
            let _ = to_ff.write_all(&first);
            let _ = std::io::copy(&mut socket, &mut to_ff);
        });
        // ffmpeg's pictures out
        let (tx, pictures) = mpsc::channel();
        std::thread::spawn(move || {
            let size = (WIDTH * HEIGHT * 3) as usize;
            loop {
                let mut rgb = vec![0u8; size];
                if from_ff.read_exact(&mut rgb).is_err() {
                    return;
                }
                if tx.send(Picture { width: WIDTH, height: HEIGHT, rgb, source: "scrcpy" }).is_err() {
                    return;
                }
            }
        });
        Ok(Scrcpy { server, ffmpeg: ff, _fwd: fwd, pictures })
    }

    /// Whether the server is still running.
    pub fn running(&mut self) -> bool {
        matches!(self.server.try_wait(), Ok(None)) && matches!(self.ffmpeg.try_wait(), Ok(None))
    }
}

// --- screencap --------------------------------------------------------------

/// A raw `screencap` (a 12-byte header on older Android, 16 on newer: width,
/// height, format[, colour space], then RGBA), shrunk to [`WIDTH`] x [`HEIGHT`].
pub fn from_screencap(raw: &[u8]) -> Option<Picture> {
    let w = u32::from_le_bytes(raw.get(0..4)?.try_into().ok()?) as usize;
    let h = u32::from_le_bytes(raw.get(4..8)?.try_into().ok()?) as usize;
    let pixels = w.checked_mul(h)?.checked_mul(4)?;
    let header = raw.len().checked_sub(pixels)?;
    if w == 0 || h == 0 || !(12..=16).contains(&header) {
        return None;
    }
    let mut rgb = Vec::with_capacity((WIDTH * HEIGHT * 3) as usize);
    for y in 0..HEIGHT as usize {
        let sy = y * h / HEIGHT as usize;
        for x in 0..WIDTH as usize {
            let sx = x * w / WIDTH as usize;
            let p = header + (sy * w + sx) * 4;
            rgb.extend_from_slice(raw.get(p..p + 3)?);
        }
    }
    Some(Picture { width: WIDTH, height: HEIGHT, rgb, source: "screenshot" })
}

pub fn screencap(tv: &Tv) -> Result<Picture> {
    let raw = tv.exec_out(&["screencap"])?;
    from_screencap(&raw).map_or_else(|| failed("preview: the TV's screenshot wasn't what tveee expected"), Ok)
}

// --- YouTube storyboards ------------------------------------------------------

/// A video's storyboard: sheets of `cols` x `rows` frames, each `width` x
/// `height`, `fps` frames a second of video.
#[derive(Debug, Clone, PartialEq)]
pub struct Storyboard {
    pub cols: u32,
    pub rows: u32,
    pub fps: f64,
    pub width: u32,
    pub height: u32,
    pub sheets: Vec<String>,
}

impl Storyboard {
    /// Which sheet, and where on it (x, y), the frame at `secs` is.
    pub fn frame_at(&self, secs: f64) -> Option<(usize, u32, u32)> {
        let per_sheet = (self.cols * self.rows) as usize;
        if per_sheet == 0 || self.sheets.is_empty() {
            return None;
        }
        let i = (secs.max(0.0) * self.fps).floor() as usize;
        // past the end: the last frame there is
        let i = i.min(self.sheets.len() * per_sheet - 1);
        let (sheet, cell) = (i / per_sheet, (i % per_sheet) as u32);
        Some((sheet, (cell % self.cols) * self.width, (cell / self.cols) * self.height))
    }
}

/// The strings in a JSON list of strings (`["a", "b\"c"]`).
pub fn json_strings(list: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = list.chars();
    while let Some(c) = chars.next() {
        if c != '"' {
            continue;
        }
        let mut s = String::new();
        while let Some(c) = chars.next() {
            match c {
                '\\' => {
                    if let Some(e) = chars.next() {
                        s.push(match e {
                            'n' => '\n',
                            't' => '\t',
                            other => other,
                        });
                    }
                }
                '"' => break,
                c => s.push(c),
            }
        }
        out.push(s);
    }
    out
}

/// The storyboard from yt-dlp's two lines (see [`storyboard`]).
pub fn parse_storyboard(out: &str) -> Option<Storyboard> {
    let mut lines = out.lines().filter(|l| !l.trim().is_empty());
    let nums: Vec<&str> = lines.next()?.split_whitespace().collect();
    let [cols, rows, fps, width, height] = nums.as_slice() else { return None };
    let sheets = json_strings(lines.next()?);
    Some(Storyboard {
        cols: cols.parse().ok()?,
        rows: rows.parse().ok()?,
        fps: fps.parse().ok()?,
        width: width.parse().ok()?,
        height: height.parse().ok()?,
        sheets,
    })
    .filter(|s| !s.sheets.is_empty() && s.fps > 0.0)
}

/// Video `id`'s storyboard, from yt-dlp (asked once a run per video).
pub fn storyboard(id: &str) -> Option<Storyboard> {
    static KNOWN: OnceLock<Mutex<HashMap<String, Option<Storyboard>>>> = OnceLock::new();
    let known = KNOWN.get_or_init(Default::default);
    if let Some(s) = known.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(id) {
        return s.clone();
    }
    let ytdlp = program("yt-dlp")?;
    let out = std::process::Command::new(ytdlp)
        .args(["--skip-download", "-f", "sb0", "-O", "%(columns)s %(rows)s %(fps)s %(width)s %(height)s", "-O", "%(fragments.:.url)j"])
        .arg(format!("https://www.youtube.com/watch?v={id}"))
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let board = parse_storyboard(&String::from_utf8_lossy(&out.stdout));
    known.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(id.to_string(), board.clone());
    board
}

/// A storyboard sheet, fetched (with curl) and decoded; kept for the run.
fn sheet(url: &str) -> Option<image::RgbImage> {
    static SHEETS: OnceLock<Mutex<HashMap<String, image::RgbImage>>> = OnceLock::new();
    let sheets = SHEETS.get_or_init(Default::default);
    if let Some(s) = sheets.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(url) {
        return Some(s.clone());
    }
    let out = std::process::Command::new(program("curl")?).args(["-sSfL", "--max-time", "15", url]).output().ok()?;
    let img = image::load_from_memory(&out.stdout).ok()?.to_rgb8();
    sheets.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(url.to_string(), img.clone());
    Some(img)
}

/// The storyboard frame of video `id` at `secs`.
pub fn storyboard_frame(id: &str, secs: f64) -> Option<Picture> {
    let board = storyboard(id)?;
    let (n, x, y) = board.frame_at(secs)?;
    let sheet = sheet(board.sheets.get(n)?)?;
    let cell = image::imageops::crop_imm(&sheet, x, y, board.width.min(sheet.width().saturating_sub(x)), board.height.min(sheet.height().saturating_sub(y))).to_image();
    Some(Picture { width: cell.width(), height: cell.height(), rgb: cell.into_raw(), source: "YouTube storyboard" })
}

// --- YouTube: a small copy of the video, shown in step with the TV ----------

/// Where small copies of videos are kept (a few, newest first).
fn copies_dir() -> Option<PathBuf> {
    Some(crate::plugin::cache_path("x")?.parent()?.join("previews"))
}

/// How many copies to keep.
const KEEP: usize = 4;

/// The smallest video-only download yt-dlp can find (a direct one, not a
/// stream of pieces): 144p, a couple of MB for a few minutes.
const SMALLEST: &[&str] = &["-f", "bv[protocol^=http]/bv/wv", "-S", "+size,+br,+res"];

/// A copy of video `id`, at the lowest quality, made with yt-dlp (or the one
/// made before). Slow-ish the first time: for a thread of its own.
pub fn local_copy(id: &str) -> Result<PathBuf> {
    let Some(dir) = copies_dir() else { return failed("preview: no cache folder") };
    std::fs::create_dir_all(&dir)?;
    let mine = |p: &PathBuf| {
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        name.starts_with(&format!("{id}.")) && !name.ends_with(".part") && !name.ends_with(".ytdl")
    };
    let listed = || -> Vec<PathBuf> { std::fs::read_dir(&dir).map(|d| d.flatten().map(|e| e.path()).collect()).unwrap_or_default() };
    if let Some(p) = listed().into_iter().find(mine) {
        return Ok(p);
    }
    // room for this one: the oldest go
    let mut old: Vec<(std::time::SystemTime, PathBuf)> =
        listed().into_iter().filter_map(|p| Some((std::fs::metadata(&p).ok()?.modified().ok()?, p))).collect();
    old.sort();
    for (_, p) in old.iter().rev().skip(KEEP - 1).collect::<Vec<_>>() {
        let _ = std::fs::remove_file(p);
    }
    let Some(ytdlp) = program("yt-dlp") else { return failed("preview: yt-dlp isn't installed here") };
    let out = std::process::Command::new(ytdlp)
        .args(["-q", "--no-playlist", "--no-progress"])
        .args(SMALLEST)
        .arg("-o")
        .arg(dir.join(format!("{id}.%(ext)s")))
        .arg(format!("https://www.youtube.com/watch?v={id}"))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()?;
    match listed().into_iter().find(mine) {
        Some(p) => Ok(p),
        None => failed(format!("preview: yt-dlp couldn't fetch the video: {}", String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or("").trim())),
    }
}

/// The frame at `secs` into a video file, at [`WIDTH`] x [`HEIGHT`].
pub fn frame_of(file: &std::path::Path, secs: f64) -> Option<Picture> {
    let out = std::process::Command::new(program("ffmpeg")?)
        .args(["-loglevel", "error", "-ss", &format!("{:.2}", secs.max(0.0)), "-i"])
        .arg(file)
        .args(["-frames:v", "1", "-vf", &format!("scale={WIDTH}:{HEIGHT}"), "-f", "rawvideo", "-pix_fmt", "rgb24", "pipe:1"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    (out.stdout.len() == (WIDTH * HEIGHT * 3) as usize).then_some(Picture { width: WIDTH, height: HEIGHT, rgb: out.stdout, source: "YouTube, a small copy" })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storyboard_frames() {
        let board = parse_storyboard("WARNING: ignored\n5 5 0.1 120 90\n[\"https://a/M0.jpg?x=1\", \"https://a/M1.jpg\"]\n");
        // the warning line is the first non-empty: not numbers
        assert_eq!(board, None);
        let b = parse_storyboard("5 5 0.1 120 90\n[\"https://a/M0.jpg?x=1\", \"https://a/M1.jpg\"]\n").unwrap();
        assert_eq!((b.cols, b.width, b.sheets.len()), (5, 120, 2));
        // 0.1 fps: frame 0 at 0s, frame 7 at 70s (row 1, column 2), frame 25 on sheet 1
        assert_eq!(b.frame_at(0.0), Some((0, 0, 0)));
        assert_eq!(b.frame_at(75.0), Some((0, 240, 90)));
        assert_eq!(b.frame_at(250.0), Some((1, 0, 0)));
        // past the end: the last frame
        assert_eq!(b.frame_at(99999.0), Some((1, 480, 360)));
    }

    #[test]
    fn black_pictures() {
        let p = |rgb: Vec<u8>| Picture { width: 10, height: 1, rgb, source: "t" };
        assert!(p(vec![0; 30]).black());
        let mut some = vec![0; 30];
        some[..6].copy_from_slice(&[200, 200, 200, 200, 200, 200]);
        assert!(!p(some).black());
    }

    #[test]
    fn reads_json_strings() {
        assert_eq!(json_strings(r#"["a", "b\"c", "d\\e"]"#), ["a", "b\"c", "d\\e"]);
        assert!(json_strings("[]").is_empty());
    }

    #[test]
    fn shrinks_screenshots() {
        // 640x360, a 12-byte header; red left half, blue right half
        let (w, h) = (640u32, 360u32);
        let mut raw = Vec::new();
        raw.extend(w.to_le_bytes());
        raw.extend(h.to_le_bytes());
        raw.extend(1u32.to_le_bytes());
        for _ in 0..h {
            for x in 0..w {
                raw.extend(if x < w / 2 { [255, 0, 0, 255] } else { [0, 0, 255, 255] });
            }
        }
        let p = from_screencap(&raw).unwrap();
        assert_eq!((p.width, p.height, p.rgb.len()), (WIDTH, HEIGHT, (WIDTH * HEIGHT * 3) as usize));
        assert_eq!(&p.rgb[0..3], &[255, 0, 0]);
        let last = p.rgb.len() - 3;
        assert_eq!(&p.rgb[last..], &[0, 0, 255]);
        // not a screenshot
        assert_eq!(from_screencap(&[1, 2, 3]), None);
    }
}

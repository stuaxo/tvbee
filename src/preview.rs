//! Pictures of what's on the TV, for the ui's preview panel.
//!
//! Apps with a player of their own give their own pictures (see
//! `Player::picture`: YouTube's frames, Kodi's artwork). Everything else:
//!
//! - scrcpy's server, run on the TV at 320 pixels and one frame a second: the
//!   TV's hardware encoder sends H.264, which `ffmpeg` here turns back into
//!   pictures. It sees video too (`screencap` doesn't, on TVs that play it
//!   on a hardware layer). Needs scrcpy and ffmpeg installed here.
//! - `screencap`, raw, every few seconds: menus and apps only.

use crate::error::{failed, Result};
use crate::tv::Tv;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::sync::OnceLock;
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

use crate::frames::program;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn black_pictures() {
        let p = |rgb: Vec<u8>| Picture { width: 10, height: 1, rgb, source: "t" };
        assert!(p(vec![0; 30]).black());
        let mut some = vec![0; 30];
        some[..6].copy_from_slice(&[200, 200, 200, 200, 200, 200]);
        assert!(!p(some).black());
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

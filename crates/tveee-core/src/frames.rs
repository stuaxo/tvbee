//! Single frames of video, as JPEG, with `ffmpeg` (when it's installed):
//! what preview pictures are made from (YouTube's copies and storyboards,
//! Kodi's files when they're reachable from here).

use std::path::{Path, PathBuf};

/// A program on PATH, if it's installed.
pub fn program(name: &str) -> Option<PathBuf> {
    crate::tv::search_path(name, std::env::var_os("PATH").as_deref())
}

/// `ffmpeg` over `input` with filter `vf`, one frame out as JPEG.
pub fn ffmpeg_jpeg(seek: Option<f64>, input: &Path, vf: &str) -> Option<Vec<u8>> {
    let mut cmd = std::process::Command::new(program("ffmpeg")?);
    cmd.args(["-loglevel", "error"]);
    if let Some(s) = seek {
        cmd.args(["-ss", &format!("{:.2}", s.max(0.0))]);
    }
    let out = cmd
        .arg("-i")
        .arg(input)
        .args(["-frames:v", "1", "-vf", vf, "-f", "image2pipe", "-vcodec", "mjpeg", "pipe:1"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    (!out.stdout.is_empty()).then_some(out.stdout)
}

/// The frame at `secs` into a video file, as JPEG.
pub fn frame_of(file: &Path, secs: f64) -> Option<Vec<u8>> {
    ffmpeg_jpeg(Some(secs), file, "scale=320:-2")
}


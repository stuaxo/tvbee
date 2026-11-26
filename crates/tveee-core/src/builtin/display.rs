use crate::error::{failed, Result};
use crate::actions::ActionDescriptor;
use crate::Ctx;
use std::process::Command as Proc;
use std::time::{SystemTime, UNIX_EPOCH};

const CAT: &str = "display";

const PNG_MAGIC: &[u8] = &[0x89, b'P', b'N', b'G'];

/// TVs play video on a hardware "sideband" layer that bypasses GPU composition,
/// so `screencap` can't see it. On this Philips firmware it shows up in
/// `dumpsys SurfaceFlinger --list` as `NativeMediaPlayerSurface` (the full dump
/// names it a "Sideband Layer" but takes ~8s).
pub fn has_sideband_video(layer_list: &str) -> bool {
    layer_list.contains("NativeMediaPlayerSurface") || layer_list.contains("Sideband Layer")
}

/// A 1080p PNG this small is a flat colour: heuristic for "nothing but video on screen".
pub fn is_probably_blank(png: &[u8]) -> bool {
    png.len() < 20_000
}

fn default_name() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    format!("tveee-{secs}.png")
}

/// Menu version: save to the default name, report it on the status line.
fn screenshot_item(tv: &crate::tv::Tv) -> Result<String> {
    let out = default_name();
    let png = tv.exec_out(&["screencap", "-p"])?;
    if !png.starts_with(PNG_MAGIC) {
        return failed("screenshot: the TV didn't return a PNG");
    }
    std::fs::write(&out, &png)?;
    let blank = if is_probably_blank(&png) { " (video areas come out black)" } else { "" };
    Ok(format!("saved {out}{blank}"))
}

pub fn actions() -> Vec<ActionDescriptor> {
    use crate::actions::ActionDescriptor as A;
    let mut v = command_actions();
    v.extend([
        A::new("display.mirror_window", CAT, "mirror screen (scrcpy window)", |cx, _| mirror_item(cx.tv).map(Into::into)).key('M'),
        A::new("display.screenshot_here", CAT, "screenshot (saved here)", |cx, _| screenshot_item(cx.tv).map(Into::into)).key('S'),
    ]);
    v
}

fn screenshot(c: &Ctx, args: &[String]) -> Result<()> {
    let out = args.first().cloned().unwrap_or_else(default_name);
    let tv = &c.tv;
    // stream the PNG straight over adb (no temp file on the TV); look at the
    // compositor and foreground app at the same time for context
    let (png, layers, window) = std::thread::scope(|sc| {
        let png = sc.spawn(|| tv.exec_out(&["screencap", "-p"]));
        let layers = sc.spawn(|| tv.shell(&["dumpsys", "SurfaceFlinger", "--list"]));
        let window = sc.spawn(|| tv.window_dump());
        (png.join(), layers.join(), window.join())
    });
    let died = || crate::error::Error::Failed("screenshot: worker thread panicked".into());
    let png = png.map_err(|_| died())??;
    if !png.starts_with(PNG_MAGIC) {
        return failed("screenshot: the TV didn't return a PNG (screen off or capture blocked?)");
    }
    std::fs::write(&out, &png)?;
    println!("{out}");

    let layers = layers.map_err(|_| died())?.unwrap_or_default();
    let window = window.map_err(|_| died())?.unwrap_or_default();
    if let Some(w) = super::info::focus_window(&window) {
        eprintln!("note: foreground {w}");
    }
    if is_probably_blank(&png) {
        eprintln!("note: the image is almost entirely black - only video (or a protected/DRM surface) is on screen");
    }
    if has_sideband_video(&layers) {
        eprintln!(
            "note: video is on a hardware sideband layer, so this shows the app's overlay only \
             (the picture area will be black)"
        );
    }
    Ok(())
}

/// Settings shared by `mirror` and the ui menu. Lower res/bitrate: the TV has
/// very little RAM and CPU headroom.
fn scrcpy(tv: &crate::tv::Tv) -> Result<Proc> {
    if Proc::new("scrcpy").arg("--version").output().is_err() {
        return failed("mirror: scrcpy not found (install it, e.g. 'sudo apt install scrcpy')");
    }
    let mut cmd = Proc::new("scrcpy");
    cmd.args(["-s", &tv.serial()])
        .args(["--window-title", &format!("TV {}", tv.ip)])
        .args(["--max-size", "1280", "--video-bit-rate", "4M"]);
    Ok(cmd)
}

fn mirror(c: &Ctx, args: &[String]) -> Result<()> {
    super::tools::run_replacing(scrcpy(&c.tv)?.args(args))
}

/// Menu version: open scrcpy in its own window and carry on. Waits briefly
/// so that an immediate failure (no display, TV refused) is reported.
fn mirror_item(tv: &crate::tv::Tv) -> Result<String> {
    use std::process::Stdio;
    // a file, not a pipe: scrcpy keeps logging while it runs
    let log_path = std::env::temp_dir().join("tveee-scrcpy.log");
    let log = std::fs::File::create(&log_path)?;
    let mut child = scrcpy(tv)?.stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log).spawn()?;
    for _ in 0..20 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if let Some(status) = child.try_wait()? {
            let text = std::fs::read_to_string(&log_path).unwrap_or_default();
            let why = text.lines().rev().find(|l| l.contains("ERROR")).or(text.lines().last()).unwrap_or("");
            return failed(format!("scrcpy exited ({status}): {}", why.trim()));
        }
    }
    Ok("mirroring in a new window (close it to stop)".into())
}


pub fn command_actions() -> Vec<ActionDescriptor> {
    vec![
        ActionDescriptor::terminal("display.screenshot", CAT, "save a PNG of the screen", screenshot).cli("screenshot", "[file]", "save a PNG of the screen")
            .long("Default file is ./tveee-<unix-time>.png. The path is printed on stdout; notes go\nto stderr. TVs play video on a hardware layer that screencap can't capture, so the\npicture area comes out black (only app overlays/menus are captured); a note says\nwhen that applies."),
        ActionDescriptor::terminal("display.mirror", CAT, "show/control the screen on this PC with scrcpy", mirror).cli("mirror", "[scrcpy args]", "show/control the screen on this PC with scrcpy")
            .long("Extra arguments are passed straight to scrcpy, e.g. 'tveee mirror --no-control'.")
            .aliases(&["scrcpy"]),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_heuristic() {
        assert!(is_probably_blank(&[0; 10_595]));
        assert!(!is_probably_blank(&[0; 40_857]));
    }

    #[test]
    fn detects_sideband_video() {
        assert!(has_sideband_video("NativeMediaPlayerSurface#0\ncom.android.systemui.ImageWallpaper#0"));
        assert!(has_sideband_video("    3 Layers\n      Sideband Layer  : 26  Z: 0"));
        assert!(!has_sideband_video("com.android.systemui.ImageWallpaper#0\nStatusBar#0"));
    }
}

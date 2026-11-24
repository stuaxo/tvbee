//! Thin wrapper over the `adb` binary for one TV.

use crate::error::{failed, Error, Result};
use std::io;
use std::path::PathBuf;
use std::process::{Command as Proc, Output};

pub const DEFAULT_IP: &str = "192.168.1.19";

/// Where a program was found, and how.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub path: PathBuf,
    /// "PATH", "$ADB", "$ANDROID_HOME/platform-tools", ...
    pub how: String,
}

/// `name` (plus `.exe` on Windows) in the directories of `path_var`.
pub fn search_path(name: &str, path_var: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    let names: Vec<String> = if cfg!(windows) { vec![format!("{name}.exe"), name.to_string()] } else { vec![name.to_string()] };
    std::env::split_paths(path_var?)
        .flat_map(|dir| names.iter().map(move |n| dir.join(n)))
        .find(|p| p.is_file())
}

/// Find adb: `$ADB`, then PATH, then the Android SDK variables
/// (`$ANDROID_HOME` / `$ANDROID_SDK_ROOT` + `platform-tools`).
pub fn find_adb(env: impl Fn(&str) -> Option<std::ffi::OsString>) -> Option<Found> {
    if let Some(p) = env("ADB").map(PathBuf::from).filter(|p| p.is_file()) {
        return Some(Found { path: p, how: "$ADB".into() });
    }
    if let Some(p) = search_path("adb", env("PATH").as_deref()) {
        return Some(Found { path: p, how: "PATH".into() });
    }
    for var in ["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
        let dir = env(var).map(|d| PathBuf::from(d).join("platform-tools"));
        if let Some(p) = dir.and_then(|d| search_path("adb", Some(d.as_os_str()))) {
            return Some(Found { path: p, how: format!("${var}/platform-tools") });
        }
    }
    None
}

/// The adb to run (found once; plain "adb" if nothing was found, so the
/// error says "adb not found").
fn adb_program() -> &'static std::path::Path {
    static ADB: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    ADB.get_or_init(|| find_adb(|k| std::env::var_os(k)).map_or_else(|| PathBuf::from("adb"), |f| f.path))
}

pub struct Tv {
    pub ip: String,
}

fn adb_err(e: io::Error) -> Error {
    if e.kind() == io::ErrorKind::NotFound {
        Error::Failed("adb not found (looked at $ADB, PATH, $ANDROID_HOME and $ANDROID_SDK_ROOT); see 'tveee diagnose'".into())
    } else {
        e.into()
    }
}

fn clean(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).replace('\r', "")
}

/// Turn a failed adb run into an error carrying what adb (or the device) said.
fn check(args: &[&str], out: Output) -> Result<Output> {
    if out.status.success() {
        return Ok(out);
    }
    let said = [clean(&out.stderr), clean(&out.stdout)]
        .into_iter()
        .map(|s| s.trim().to_string())
        .find(|s| !s.is_empty());
    let what = args.iter().take(3).copied().collect::<Vec<_>>().join(" ");
    match said {
        Some(msg) => failed(format!("adb {what}: {msg}")),
        None => failed(format!("adb {what} failed ({})", out.status)),
    }
}

/// `adb connect` reports most failures on stdout with exit status 0.
pub fn connect_ok(stdout: &str) -> bool {
    stdout.contains("connected to")
}

impl Tv {
    pub fn from_env() -> Self {
        Tv {
            ip: std::env::var("TV_IP").unwrap_or_else(|_| DEFAULT_IP.into()),
        }
    }

    pub fn serial(&self) -> String {
        format!("{}:5555", self.ip)
    }

    fn run(&self, args: &[&str]) -> Result<Output> {
        let out = Proc::new(adb_program())
            .arg("-s")
            .arg(self.serial())
            .args(args)
            .output()
            .map_err(adb_err)?;
        check(args, out)
    }

    /// `adb connect`; fails if adb can't run or the TV can't be reached.
    pub fn connect(&self) -> Result<()> {
        let out = Proc::new(adb_program())
            .args(["connect", &self.ip])
            .output()
            .map_err(adb_err)?;
        let stdout = clean(&out.stdout);
        if out.status.success() && connect_ok(&stdout) {
            Ok(())
        } else {
            let why = stdout.trim();
            failed(format!("can't connect to {}{}", self.ip, if why.is_empty() { String::new() } else { format!(": {why}") }))
        }
    }

    pub fn try_connect(&self) -> bool {
        self.connect().is_ok()
    }

    /// Run `adb -s <serial> <args>` and return stdout (CRs stripped).
    /// A non-zero exit (adb's or the device command's) is an error.
    pub fn adb(&self, args: &[&str]) -> Result<String> {
        Ok(clean(&self.run(args)?.stdout))
    }

    pub fn shell(&self, args: &[&str]) -> Result<String> {
        let mut full = vec!["shell"];
        full.extend_from_slice(args);
        self.adb(&full)
    }

    /// `adb exec-out`: raw binary stdout with no CR/LF mangling.
    pub fn exec_out(&self, args: &[&str]) -> Result<Vec<u8>> {
        let mut full = vec!["exec-out"];
        full.extend_from_slice(args);
        Ok(self.run(&full)?.stdout)
    }

    /// Like `shell`, but the device's output goes straight to the terminal.
    pub fn shell_inherit(&self, args: &[&str]) -> Result<()> {
        let status = Proc::new(adb_program())
            .arg("-s")
            .arg(self.serial())
            .arg("shell")
            .args(args)
            .status()
            .map_err(adb_err)?;
        if status.success() {
            Ok(())
        } else {
            failed(format!("adb shell {} failed ({status})", args.first().unwrap_or(&"")))
        }
    }

    /// Press a key: written straight to an input device when tveee can,
    /// which is much quicker than `input keyevent` (see `keys`).
    pub fn keyevent(&self, code: u32) -> Result<()> {
        crate::keys::press(self, &[(code, false)])
    }

    /// Press several keys in turn (as one write when they can go raw).
    pub fn keys(&self, codes: &[u32]) -> Result<()> {
        let presses: Vec<(u32, bool)> = codes.iter().map(|c| (*c, false)).collect();
        crate::keys::press(self, &presses)
    }

    /// `adb shell` reading commands from its stdin, left open (output
    /// discarded): for sending one command after another with no start-up.
    pub fn shell_session(&self) -> Result<std::process::Child> {
        Proc::new(adb_program())
            .arg("-s")
            .arg(self.serial())
            .arg("shell")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(adb_err)
    }

    /// Connect, then send a key.
    pub fn key(&self, code: u32) -> Result<()> {
        self.connect()?;
        self.keyevent(code)
    }

    /// `dumpsys window` output, for foreground-app detection.
    pub fn window_dump(&self) -> Result<String> {
        self.shell(&["dumpsys", "window"])
    }

    /// Forward a free local TCP port to a device abstract socket. The forward
    /// is removed when the returned guard is dropped.
    pub fn forward_abstract(&self, socket: &str) -> Result<Forward> {
        let out = self.adb(&["forward", "tcp:0", &format!("localabstract:{socket}")])?;
        match out.trim().parse() {
            Ok(port) => Ok(Forward { tv: Tv { ip: self.ip.clone() }, port }),
            Err(_) => failed(format!("adb forward: unexpected reply '{}'", out.trim())),
        }
    }
}

/// Owns its own `Tv` handle so it can outlive the borrow that made it.
pub struct Forward {
    tv: Tv,
    pub port: u16,
}

impl Drop for Forward {
    fn drop(&mut self) {
        let _ = self.tv.adb(&["forward", "--remove", &format!("tcp:{}", self.port)]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_adb() {
        let dir = std::env::temp_dir().join(format!("tveee-adb-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let sdk = dir.join("sdk");
        std::fs::create_dir_all(sdk.join("platform-tools")).unwrap();
        let exe = if cfg!(windows) { "adb.exe" } else { "adb" };
        std::fs::write(sdk.join("platform-tools").join(exe), "").unwrap();
        let only_sdk = |k: &str| (k == "ANDROID_SDK_ROOT").then(|| sdk.clone().into_os_string());
        let f = find_adb(only_sdk).unwrap();
        assert_eq!(f.how, "$ANDROID_SDK_ROOT/platform-tools");
        let on_path = |k: &str| (k == "PATH").then(|| sdk.join("platform-tools").into_os_string());
        assert_eq!(find_adb(on_path).unwrap().how, "PATH");
        assert!(find_adb(|_| None).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn connect_reply() {
        assert!(connect_ok("connected to 192.168.1.19:5555\n"));
        assert!(connect_ok("already connected to 192.168.1.19:5555\n"));
        assert!(!connect_ok("failed to connect to '192.0.2.1:5555': Connection refused\n"));
        assert!(!connect_ok("cannot connect to 192.0.2.1:5555: No route to host\n"));
    }
}

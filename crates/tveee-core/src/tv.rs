//! Thin wrapper over the `adb` binary for one TV.

use crate::error::{Error, Result, failed};
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
    std::env::split_paths(path_var?).flat_map(|dir| names.iter().map(move |n| dir.join(n))).find(|p| p.is_file())
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

/// An adb command for the log: shortened, and raw key writes summarised.
fn loggable(args: &[&str]) -> String {
    let all = args.join(" ");
    let all = if all.contains("print -n '\\x") { "shell (raw key events)".to_string() } else { all };
    match all.char_indices().nth(160) {
        Some((i, _)) => format!("{}...", &all[..i]),
        None => all,
    }
}

/// Turn a failed adb run into an error carrying what adb (or the device) said.
fn check(args: &[&str], out: Output) -> Result<Output> {
    if out.status.success() {
        return Ok(out);
    }
    let said = [clean(&out.stderr), clean(&out.stdout)].into_iter().map(|s| s.trim().to_string()).find(|s| !s.is_empty());
    let what = args.iter().take(3).copied().collect::<Vec<_>>().join(" ");
    match said {
        Some(msg) => failed(format!("adb {what}: {msg}")),
        None => failed(format!("adb {what} failed ({})", out.status)),
    }
}

/// adb's complaint (stderr) when it isn't connected to the device: then
/// `adb connect` and try again.
pub fn not_connected(stderr: &str) -> bool {
    (stderr.contains("device '") && stderr.contains("' not found")) || stderr.contains("device offline") || stderr.contains("no devices/emulators found")
}

/// How long `adb connect` gets.
pub const CONNECT_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// Run `cmd` (stdout and stderr captured; it should say little), killing it
/// if it isn't done within `limit`: then None.
pub fn output_within(mut cmd: Proc, limit: std::time::Duration) -> io::Result<Option<Output>> {
    use std::process::Stdio;
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let started = std::time::Instant::now();
    while child.try_wait()?.is_none() {
        if started.elapsed() >= limit {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    child.wait_with_output().map(Some)
}

/// `adb connect` reports most failures on stdout with exit status 0.
pub fn connect_ok(stdout: &str) -> bool {
    stdout.contains("connected to")
}

impl Tv {
    pub fn from_env() -> Self {
        Tv { ip: std::env::var("TV_IP").unwrap_or_else(|_| DEFAULT_IP.into()) }
    }

    pub fn serial(&self) -> String {
        format!("{}:5555", self.ip)
    }

    /// Run adb against the TV's serial; when adb says it isn't connected,
    /// `adb connect` (whose error, e.g. the TV being off, is the one returned)
    /// and run it again.
    fn run(&self, args: &[&str]) -> Result<Output> {
        // answered from a recording (`crate::mock`), or recorded
        let what: Vec<&str> = std::iter::once("adb").chain(args.iter().copied()).collect();
        if let Some(answer) = crate::mock::replay(&what) {
            return answer.map(|stdout| Output { status: Default::default(), stdout, stderr: Vec::new() });
        }
        let result = self.run_adb(args);
        crate::mock::record(&what, result.as_ref().map(|o| o.stdout.as_slice()).map_err(ToString::to_string));
        result
    }

    fn run_adb(&self, args: &[&str]) -> Result<Output> {
        let started = std::time::Instant::now();
        let go = || Proc::new(adb_program()).arg("-s").arg(self.serial()).args(args).output().map_err(adb_err);
        let mut out = go()?;
        if !out.status.success() && not_connected(&clean(&out.stderr)) {
            crate::tlog!("adb {}: not connected, connecting", loggable(args));
            self.reconnect()?;
            out = go()?;
        }
        let result = check(args, out);
        crate::tlog!(
            "adb {}: {}ms{}",
            loggable(args),
            started.elapsed().as_millis(),
            result.as_ref().err().map(|e| format!(" FAILED: {e}")).unwrap_or_default()
        );
        result
    }

    /// `adb connect`; fails if adb can't run or the TV can't be reached.
    /// Other calls connect when they need to, so this is only for checking
    /// the TV can be reached.
    /// Gives up after [`CONNECT_WAIT`]: adb waits minutes for an address
    /// that doesn't answer (a TV that's off).
    pub fn connect(&self) -> Result<()> {
        let mut cmd = Proc::new(adb_program());
        cmd.args(["connect", &self.ip]);
        let Some(out) = output_within(cmd, CONNECT_WAIT).map_err(adb_err)? else {
            crate::tlog!("adb connect {}: FAILED: no answer in {}s", self.ip, CONNECT_WAIT.as_secs());
            return failed(format!("can't connect to {}: no answer in {}s (is the TV on?)", self.ip, CONNECT_WAIT.as_secs()));
        };
        let stdout = clean(&out.stdout);
        if out.status.success() && connect_ok(&stdout) {
            Ok(())
        } else {
            let why = stdout.trim();
            crate::tlog!("adb connect {}: FAILED: {why}", self.ip);
            failed(format!("can't connect to {}{}", self.ip, if why.is_empty() { String::new() } else { format!(": {why}") }))
        }
    }

    /// `adb connect` for a call adb refused, one thread at a time, then wait
    /// for adb to have the device (it's "offline" for a moment after
    /// connecting). A failure is given to calls in the next few seconds
    /// as it is, rather than each of them trying again (a TV that's off can
    /// take a long time to fail).
    fn reconnect(&self) -> Result<()> {
        static LAST_FAILURE: std::sync::Mutex<Option<(String, std::time::Instant, String)>> = std::sync::Mutex::new(None);
        let mut last = LAST_FAILURE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((serial, at, why)) = last.as_ref() {
            if *serial == self.serial() && at.elapsed() < std::time::Duration::from_secs(5) {
                return failed(why.clone());
            }
        }
        match self.connect().and_then(|()| self.wait_online()) {
            Ok(()) => {
                *last = None;
                Ok(())
            }
            Err(e) => {
                *last = Some((self.serial(), std::time::Instant::now(), e.to_string()));
                Err(e)
            }
        }
    }

    /// Wait (up to 3s) for adb to say the device is ready.
    fn wait_online(&self) -> Result<()> {
        for _ in 0..15 {
            let state = Proc::new(adb_program()).arg("-s").arg(self.serial()).arg("get-state").output().map_err(adb_err)?;
            if state.status.success() && clean(&state.stdout).trim() == "device" {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        crate::tlog!("adb {}: still not ready after connecting", self.serial());
        failed(format!("adb: {} is connected but not ready (offline)", self.serial()))
    }

    /// Make sure adb is connected to the TV (connecting if not), for the
    /// calls that start adb and leave it running.
    pub fn ready(&self) -> Result<()> {
        self.run(&["get-state"]).map(drop)
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
        self.ready()?;
        let status = Proc::new(adb_program()).arg("-s").arg(self.serial()).arg("shell").args(args).status().map_err(adb_err)?;
        if status.success() { Ok(()) } else { failed(format!("adb shell {} failed ({status})", args.first().unwrap_or(&""))) }
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

    /// Start `cmd` on the TV and leave it running (output discarded); kill
    /// the child to stop it.
    pub fn spawn_shell(&self, cmd: &str) -> Result<std::process::Child> {
        self.ready()?;
        Proc::new(adb_program())
            .arg("-s")
            .arg(self.serial())
            .arg("shell")
            .arg(cmd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(adb_err)
    }

    /// `adb shell` reading commands from its stdin, left open (output
    /// discarded): for sending one command after another with no start-up.
    pub fn shell_session(&self) -> Result<std::process::Child> {
        self.ready()?;
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

    /// Send a key.
    pub fn key(&self, code: u32) -> Result<()> {
        self.keyevent(code)
    }

    /// `dumpsys window` output, for foreground-app detection.
    /// The lines of `dumpsys window` tveee reads: what has focus, and which
    /// uid owns each app's windows (~0.6 KB of ~23 KB). For a checked
    /// reading, [`crate::readings::Window`].
    pub fn window_dump(&self) -> Result<String> {
        self.shell(&[&<crate::readings::Window as crate::readings::Reading>::cmd().render()])
    }

    /// Forward a free local TCP port to a device abstract socket. The forward
    /// is removed when the returned guard is dropped.
    pub fn forward_abstract(&self, socket: &str) -> Result<Forward> {
        self.forward(&format!("localabstract:{socket}"))
    }

    /// Forward a free local port to a TCP port on the TV itself (reaching
    /// what only listens there).
    pub fn forward_tcp(&self, port: u16) -> Result<Forward> {
        self.forward(&format!("tcp:{port}"))
    }

    fn forward(&self, remote: &str) -> Result<Forward> {
        let out = self.adb(&["forward", "tcp:0", remote])?;
        match out.trim().parse() {
            Ok(port) => Ok(Forward { tv: Tv { ip: self.ip.clone() }, port }),
            Err(_) => failed(format!("adb forward: unexpected reply '{}'", out.trim())),
        }
    }
}

/// Owns its own `Tv` handle so it can outlive the borrow that made it.
pub struct Forward {
    pub(crate) tv: Tv,
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

    #[test]
    fn output_within_a_limit() {
        let cmd = |prog: &str, arg: &str| {
            let mut c = Proc::new(prog);
            c.arg(arg);
            c
        };
        let quick = output_within(cmd("echo", "hi"), std::time::Duration::from_secs(5)).unwrap().unwrap();
        assert_eq!(String::from_utf8_lossy(&quick.stdout), "hi\n");
        let started = std::time::Instant::now();
        let slow = output_within(cmd("sleep", "5"), std::time::Duration::from_millis(200)).unwrap();
        assert!(slow.is_none() && started.elapsed() < std::time::Duration::from_secs(2));
    }

    #[test]
    fn not_connected_replies() {
        assert!(not_connected("adb: device '192.168.1.19:5555' not found\n"));
        assert!(not_connected("error: device '192.168.1.19:5555' not found\n"));
        assert!(not_connected("adb: device offline\n"));
        assert!(not_connected("adb: no devices/emulators found\n"));
        assert!(!not_connected("/system/bin/sh: foo: not found\n"));
        assert!(!not_connected(""));
    }
}

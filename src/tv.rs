//! Thin wrapper over the `adb` binary for one TV.

use crate::error::{failed, Error, Result};
use std::io;
use std::process::{Command as Proc, Output};

pub const DEFAULT_IP: &str = "192.168.1.19";

pub struct Tv {
    pub ip: String,
}

fn adb_err(e: io::Error) -> Error {
    if e.kind() == io::ErrorKind::NotFound {
        Error::Failed("adb not found in PATH".into())
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
        let out = Proc::new("adb")
            .arg("-s")
            .arg(self.serial())
            .args(args)
            .output()
            .map_err(adb_err)?;
        check(args, out)
    }

    /// `adb connect`; fails if adb can't run or the TV can't be reached.
    pub fn connect(&self) -> Result<()> {
        let out = Proc::new("adb")
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
        let status = Proc::new("adb")
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

    pub fn keyevent(&self, code: u32) -> Result<()> {
        self.shell(&["input", "keyevent", &code.to_string()])?;
        Ok(())
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
    fn connect_reply() {
        assert!(connect_ok("connected to 192.168.1.19:5555\n"));
        assert!(connect_ok("already connected to 192.168.1.19:5555\n"));
        assert!(!connect_ok("failed to connect to '192.0.2.1:5555': Connection refused\n"));
        assert!(!connect_ok("cannot connect to 192.0.2.1:5555: No route to host\n"));
    }
}

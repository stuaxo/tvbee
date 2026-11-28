//! Typed requests: everything asked of the TV, and what its answers mean
//! (the plan's part 8).
//!
//! Each thing asked is a small type implementing [`Ask`]: it says what goes
//! on the wire as data (a [`Request`]: a shell command, or an HTTP request)
//! and parses the answer in one place. [`Tv::ask`] sends it the same way for
//! every transport: logged, asked again when the answer isn't a whole one,
//! recorded and replayed in tests (`TVEEE_MOCK_DIR`, below the transports).
//!
//! Commands are built from their arguments ([`Cmd`]) and quoted once when
//! run, so a path with a quote or a space can't break one.

use crate::error::{Result, failed};
use crate::tv::Tv;

/// Something asked of the TV, and what its answer means.
pub trait Ask {
    type Answer;
    /// What it's called, in messages and the log.
    fn name(&self) -> String;
    /// What's sent (credentials looked up here: a plugin's pairing, say).
    fn request(&self, tv: &Tv) -> Result<Request>;
    /// The answer; Ok(None) when the reply isn't a whole one (empty, cut
    /// short: a busy TV), so it's asked for again. An error says the reply
    /// was whole but wrong (an API's own error).
    fn parse(&self, reply: &Reply) -> Result<Option<Self::Answer>>;
}

/// What goes on the wire.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    /// A command in the TV's shell (over adb).
    Shell(Cmd),
    /// HTTP or HTTPS to an API on the TV (through curl: TLS with the TVs'
    /// self-signed certificates, digest or basic authentication).
    Http(Http),
    /// A plain HTTP GET with tveee's own client (a local tunnel, say).
    Get { host: String, port: u16, path: String },
}

/// An HTTP request to the TV.
#[derive(Debug, Clone, PartialEq)]
pub struct Http {
    pub url: String,
    /// JSON to POST; None for a GET.
    pub body: Option<String>,
    pub auth: Auth,
    pub timeout_secs: u32,
}

/// How an HTTP request authenticates.
#[derive(Debug, Clone, PartialEq)]
pub enum Auth {
    None,
    Basic { user: String, password: String },
    Digest { user: String, password: String },
}

/// What came back: the HTTP status (0 for the shell), and the text.
#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    pub status: u16,
    pub body: String,
}

impl Reply {
    /// The body as JSON (None: it isn't).
    pub fn json(&self) -> Option<serde_json::Value> {
        serde_json::from_str(&self.body).ok()
    }
}

/// A shell command: its programs and arguments, piped one into the next,
/// quoted when run ([`Cmd::render`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Cmd {
    stages: Vec<Stage>,
}

#[derive(Debug, Clone, PartialEq)]
enum Stage {
    Argv(Vec<String>),
    /// Shell text as written (a script of its own): not quoted.
    Raw(String),
}

impl Cmd {
    /// `program`, with arguments to come.
    pub fn new(program: &str) -> Cmd {
        Cmd { stages: vec![Stage::Argv(vec![program.to_string()])] }
    }

    /// Shell text as written, for what isn't one command (a loop, a list of
    /// commands): it's run as it is, so build it from no outside text.
    pub fn raw(script: &str) -> Cmd {
        Cmd { stages: vec![Stage::Raw(script.to_string())] }
    }

    /// One more argument.
    pub fn arg(mut self, a: impl Into<String>) -> Cmd {
        match self.stages.last_mut() {
            Some(Stage::Argv(v)) => v.push(a.into()),
            _ => self.stages.push(Stage::Argv(vec![a.into()])),
        }
        self
    }

    /// Several more.
    pub fn args<S: Into<String>>(self, args: impl IntoIterator<Item = S>) -> Cmd {
        args.into_iter().fold(self, Cmd::arg)
    }

    /// Its output piped into `next`.
    pub fn pipe(mut self, next: Cmd) -> Cmd {
        self.stages.extend(next.stages);
        self
    }

    /// Only the lines with `pattern` (`| grep pattern`).
    pub fn grep(self, pattern: &str) -> Cmd {
        self.pipe(Cmd::new("grep").arg(pattern))
    }

    /// Only the lines matching extended regex `pattern` (`| grep -E`).
    pub fn grep_e(self, pattern: &str) -> Cmd {
        self.pipe(Cmd::new("grep").arg("-E").arg(pattern))
    }

    /// The command line, each argument quoted if it needs to be.
    pub fn render(&self) -> String {
        self.stages
            .iter()
            .map(|s| match s {
                Stage::Argv(v) => v.iter().map(|a| quote(a)).collect::<Vec<_>>().join(" "),
                Stage::Raw(t) => t.clone(),
            })
            .collect::<Vec<_>>()
            .join(" | ")
    }
}

/// `a` for a POSIX shell: as it is when that's safe, else in single quotes.
pub fn quote(a: &str) -> String {
    let safe = !a.is_empty() && a.chars().all(|c| c.is_ascii_alphanumeric() || "_-./:=@%+,".contains(c));
    if safe { a.to_string() } else { format!("'{}'", a.replace('\'', r"'\''")) }
}

/// Two answers asked together ([`Tv::ask_both`]), each whole or None.
pub type Both<A, B> = (Option<<A as Ask>::Answer>, Option<<B as Ask>::Answer>);

/// What separates the answers of commands run together ([`Tv::ask_both`]).
const MARK: &str = "@@tveee-next@@";

impl Tv {
    /// Send `r` and return what came back (an adb or connection failure is
    /// an error; an HTTP error status is a reply).
    pub fn send(&self, r: &Request) -> Result<Reply> {
        match r {
            Request::Shell(c) => Ok(Reply { status: 0, body: self.shell(&[&c.render()])? }),
            Request::Http(h) => {
                let pair = |u: &String, p: &String| (u.clone(), p.clone());
                let (basic, digest) = match &h.auth {
                    Auth::None => (None, None),
                    Auth::Basic { user, password } => (Some(pair(user, password)), None),
                    Auth::Digest { user, password } => (None, Some(pair(user, password))),
                };
                let (status, body) = crate::https::request(&crate::https::Request {
                    url: &h.url,
                    body: h.body.as_deref(),
                    digest: digest.as_ref().map(|(u, p)| (u.as_str(), p.as_str())),
                    basic: basic.as_ref().map(|(u, p)| (u.as_str(), p.as_str())),
                    retry: None,
                    timeout_secs: h.timeout_secs,
                })?;
                Ok(Reply { status, body })
            }
            Request::Get { host, port, path } => {
                let (status, body) = crate::http::get(host, *port, path)?;
                Ok(Reply { status, body })
            }
        }
    }

    /// Ask `a`, again once if the answer isn't a whole one; Ok(None) if it
    /// still isn't.
    pub fn ask_whole<A: Ask>(&self, a: &A) -> Result<Option<A::Answer>> {
        let request = a.request(self)?;
        for tries in 0..2 {
            let reply = self.send(&request)?;
            if let Some(answer) = a.parse(&reply)? {
                return Ok(Some(answer));
            }
            crate::tlog!("{}: not a whole answer ({} bytes){}", a.name(), reply.body.len(), if tries == 0 { "; asking again" } else { "" });
        }
        Ok(None)
    }

    /// Ask `a`; an error if the TV doesn't give a whole answer.
    pub fn ask<A: Ask>(&self, a: &A) -> Result<A::Answer> {
        match self.ask_whole(a)? {
            Some(answer) => Ok(answer),
            None => failed(format!("the TV didn't answer {} (twice)", a.name())),
        }
    }

    /// Ask `a` and `b` together: two shell commands go in one adb call (one
    /// round trip), anything else one after the other. Each answer whole or
    /// None, as [`Tv::ask_whole`] (one not whole is asked again alone).
    pub fn ask_both<A: Ask, B: Ask>(&self, a: &A, b: &B) -> Result<Both<A, B>> {
        let (ra, rb) = (a.request(self)?, b.request(self)?);
        let (Request::Shell(ca), Request::Shell(cb)) = (&ra, &rb) else { return Ok((self.ask_whole(a)?, self.ask_whole(b)?)) };
        let both = self.shell(&[&format!("{}; echo {MARK}; {}", ca.render(), cb.render())])?;
        let (out_a, out_b) = both.split_once(MARK).map_or((both.as_str(), ""), |(x, y)| (x, y.strip_prefix('\n').unwrap_or(y)));
        let reply = |body: &str| Reply { status: 0, body: body.to_string() };
        let first = match a.parse(&reply(out_a))? {
            Some(x) => Some(x),
            None => self.ask_whole(a)?,
        };
        let second = match b.parse(&reply(out_b))? {
            Some(x) => Some(x),
            None => self.ask_whole(b)?,
        };
        Ok((first, second))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_are_quoted_once() {
        let c = Cmd::new("dumpsys").arg("window").grep_e("mCurrentFocus=|mOwnerUid=");
        assert_eq!(c.render(), "dumpsys window | grep -E 'mCurrentFocus=|mOwnerUid='");
        assert_eq!(Cmd::new("dumpsys").arg("power").grep("mWakefulness=").render(), "dumpsys power | grep mWakefulness=");
        // a path with a quote and a space can't break it
        assert_eq!(Cmd::new("ls").arg("/sdcard/it's here").render(), r"ls '/sdcard/it'\''s here'");
        assert_eq!(Cmd::new("echo").arg("").render(), "echo ''");
        assert_eq!(Cmd::new("cat").args(["/proc/meminfo", "/proc/loadavg"]).render(), "cat /proc/meminfo /proc/loadavg");
        // raw text as written
        assert_eq!(Cmd::raw("for f in a b; do echo $f; done").render(), "for f in a b; do echo $f; done");
        assert_eq!(quote("$HOME"), "'$HOME'");
    }

    #[test]
    fn replies_as_json() {
        let r = Reply { status: 200, body: r#"{"a":1}"#.into() };
        assert_eq!(r.json().and_then(|v| v["a"].as_u64()), Some(1));
        assert!(Reply { status: 200, body: "nope".into() }.json().is_none());
    }
}

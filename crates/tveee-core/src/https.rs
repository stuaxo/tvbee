//! HTTPS requests through `curl`, for TV APIs that use TLS with a
//! self-signed certificate and digest authentication (Philips JointSpace).
//! tveee has no TLS library; curl is on Linux and Windows 10+ already.

use crate::error::{Error, Result, failed};
use std::io::Write;
use std::process::{Command, Stdio};

/// What a request needs.
pub struct Request<'a> {
    pub url: &'a str,
    /// JSON to POST; None for a GET.
    pub body: Option<&'a str>,
    /// Digest authentication (user, password).
    pub digest: Option<(&'a str, &'a str)>,
    /// Basic authentication (user, password), e.g. Kodi's.
    pub basic: Option<(&'a str, &'a str)>,
    /// Keep trying a refused connection for a while (see [`RETRY_FOR`]) if
    /// this says it's worth it: asked once, at the first refusal (a TV in
    /// standby refuses too, and won't come back by itself).
    pub retry: Option<&'a dyn Fn() -> bool>,
    pub timeout_secs: u32,
}

/// The arguments curl gets (credentials go on stdin, not here).
pub fn curl_args(r: &Request) -> Vec<String> {
    let mut a: Vec<String> = vec![
        "--silent".into(),
        "--show-error".into(),
        // the TV's certificate is self-signed
        "--insecure".into(),
        "--max-time".into(),
        r.timeout_secs.to_string(),
        "--write-out".into(),
        "\n%{http_code}".into(),
        // the rest of the options (credentials, body) come from stdin
        "--config".into(),
        "-".into(),
    ];
    a.push(r.url.to_string());
    a
}

/// curl config lines for stdin: credentials and the body stay out of the
/// process list.
pub fn curl_config(r: &Request) -> String {
    let quote = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    let mut c = String::new();
    if let Some((user, pass)) = r.digest {
        c.push_str("digest\n");
        c.push_str(&format!("user = {}\n", quote(&format!("{user}:{pass}"))));
    } else if let Some((user, pass)) = r.basic {
        c.push_str(&format!("user = {}\n", quote(&format!("{user}:{pass}"))));
    }
    if let Some(body) = r.body {
        c.push_str("header = \"Content-Type: application/json\"\n");
        c.push_str(&format!("data-binary = {}\n", quote(body)));
    }
    c
}

/// Split curl's output into (HTTP status, body), using the `%{http_code}`
/// it writes last.
pub fn split_status(out: &str) -> Option<(u16, String)> {
    let (body, code) = out.rsplit_once('\n')?;
    Some((code.trim().parse().ok()?, body.to_string()))
}

/// How long to keep trying when the TV refuses the connection: Philips TVs
/// restart their API for a few seconds when live TV comes up.
const RETRY_FOR: std::time::Duration = std::time::Duration::from_secs(10);

/// Make the request; returns (status, body). A refused connection is retried
/// for a while (see [`RETRY_FOR`]).
pub fn request(r: &Request) -> Result<(u16, String)> {
    // answered from a recording (`crate::mock`), or recorded
    let what = ["https", crate::mock::url_key(r.url), r.body.unwrap_or("")];
    if let Some(answer) = crate::mock::replay(&what) {
        return crate::mock::http_answer(&answer?);
    }
    let result = request_live(r);
    crate::mock::record(&what, result.as_ref().map(|(s, b)| crate::mock::http_bytes(*s, b)).as_deref().map_err(ToString::to_string));
    result
}

fn request_live(r: &Request) -> Result<(u16, String)> {
    let start = std::time::Instant::now();
    let mut worth_it: Option<bool> = None;
    loop {
        let reply = request_once(r);
        crate::tlog!(
            "{} {}: {}ms, {}",
            if r.body.is_some() { "POST" } else { "GET" },
            r.url,
            start.elapsed().as_millis(),
            match &reply {
                Ok((status, _)) => format!("HTTP {status}"),
                Err(Refused(e)) => format!("refused: {e}"),
                Err(Other(e)) => format!("FAILED: {e}"),
            }
        );
        match reply {
            Err(Refused(_)) if start.elapsed() < RETRY_FOR && *worth_it.get_or_insert_with(|| r.retry.is_some_and(|f| f())) => {
                std::thread::sleep(std::time::Duration::from_millis(700))
            }
            Err(Refused(e)) | Err(Other(e)) => return Err(e),
            Ok(reply) => return Ok(reply),
        }
    }
}

enum Fail {
    Refused(Error),
    Other(Error),
}
use Fail::{Other, Refused};

fn request_once(r: &Request) -> std::result::Result<(u16, String), Fail> {
    let fail = |e: Error| Other(e);
    let mut child = Command::new("curl")
        .args(curl_args(r))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                Error::Failed("curl not found: it's needed for the TV's HTTPS API (install curl)".into())
            } else {
                e.into()
            }
        })
        .map_err(fail)?;
    child.stdin.take().expect("piped").write_all(curl_config(r).as_bytes()).map_err(|e| fail(e.into()))?;
    let out = child.wait_with_output().map_err(|e| fail(e.into()))?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    match split_status(&text) {
        // curl writes 000 when it couldn't connect at all
        Some((0, _)) | None => {
            let err = String::from_utf8_lossy(&out.stderr);
            let e = failed::<()>(format!("https {}: {}", r.url, err.trim().lines().last().unwrap_or("no reply"))).unwrap_err();
            // curl exit 7: couldn't connect (refused)
            Err(if out.status.code() == Some(7) { Refused(e) } else { Other(e) })
        }
        Some(r) => Ok(r),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_curl_calls() {
        let r = Request {
            url: "https://tv:1926/6/pair/grant",
            body: Some(r#"{"a":"b\"c"}"#),
            digest: Some(("dev", "k\"ey")),
            basic: None,
            retry: None,
            timeout_secs: 10,
        };
        let args = curl_args(&r);
        assert!(args.contains(&"--insecure".to_string()) && args.last().unwrap() == "https://tv:1926/6/pair/grant");
        // the password and body never appear as arguments
        assert!(args.iter().all(|a| !a.contains("k\"ey") && !a.contains("b\\\"c")));
        let c = curl_config(&r);
        assert!(c.contains("digest\n"));
        assert!(c.contains(r#"user = "dev:k\"ey""#));
        assert!(c.contains(r#"data-binary = "{\"a\":\"b\\\"c\"}""#));
        let get = Request { url: "https://tv/x", body: None, digest: None, basic: None, retry: None, timeout_secs: 5 };
        assert_eq!(curl_config(&get), "");
    }

    // Windows doesn't refuse a closed local port straight away (it retries
    // the connection, so curl times out instead)
    #[cfg(not(windows))]
    #[test]
    fn refused_is_retried_only_when_worth_it() {
        // needs curl (it isn't everywhere tveee is built)
        if Command::new("curl").arg("--version").output().is_err() {
            eprintln!("no curl: skipped");
            return;
        }
        // a closed local port: curl says "refused" straight away
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let url = format!("https://127.0.0.1:{port}/x");
        let asked = std::cell::Cell::new(0);
        let check = || {
            asked.set(asked.get() + 1);
            false
        };
        let started = std::time::Instant::now();
        let r = Request { url: &url, body: None, digest: None, basic: None, retry: Some(&check), timeout_secs: 2 };
        assert!(request(&r).is_err());
        assert_eq!(asked.get(), 1);
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }

    #[test]
    fn reads_status() {
        assert_eq!(split_status("{\"x\":1}\n200"), Some((200, "{\"x\":1}".into())));
        assert_eq!(split_status("\n401"), Some((401, String::new())));
        assert_eq!(split_status("garbage"), None);
    }
}

//! HTTPS requests through `curl`, for TV APIs that use TLS with a
//! self-signed certificate and digest authentication (Philips JointSpace).
//! tveee has no TLS library; curl is on Linux and Windows 10+ already.

use crate::error::{failed, Error, Result};
use std::io::Write;
use std::process::{Command, Stdio};

/// What a request needs.
pub struct Request<'a> {
    pub url: &'a str,
    /// JSON to POST; None for a GET.
    pub body: Option<&'a str>,
    /// Digest authentication (user, password).
    pub digest: Option<(&'a str, &'a str)>,
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
    let start = std::time::Instant::now();
    loop {
        match request_once(r) {
            Err(Refused(_)) if start.elapsed() < RETRY_FOR => std::thread::sleep(std::time::Duration::from_millis(700)),
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
        let r = Request { url: "https://tv:1926/6/pair/grant", body: Some(r#"{"a":"b\"c"}"#), digest: Some(("dev", "k\"ey")), timeout_secs: 10 };
        let args = curl_args(&r);
        assert!(args.contains(&"--insecure".to_string()) && args.last().unwrap() == "https://tv:1926/6/pair/grant");
        // the password and body never appear as arguments
        assert!(args.iter().all(|a| !a.contains("k\"ey") && !a.contains("b\\\"c")));
        let c = curl_config(&r);
        assert!(c.contains("digest\n"));
        assert!(c.contains(r#"user = "dev:k\"ey""#));
        assert!(c.contains(r#"data-binary = "{\"a\":\"b\\\"c\"}""#));
        let get = Request { url: "https://tv/x", body: None, digest: None, timeout_secs: 5 };
        assert_eq!(curl_config(&get), "");
    }

    #[test]
    fn reads_status() {
        assert_eq!(split_status("{\"x\":1}\n200"), Some((200, "{\"x\":1}".into())));
        assert_eq!(split_status("\n401"), Some((401, String::new())));
        assert_eq!(split_status("garbage"), None);
    }
}

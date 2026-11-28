//! Minimal plain-HTTP GET and a flat-JSON field reader (no crates).

use crate::error::{Error, Result, failed};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

/// Split a raw HTTP response into (status, body).
pub fn parse_response(raw: &str) -> Option<(u16, &str)> {
    let (head, body) = raw.split_once("\r\n\r\n")?;
    let status = head.lines().next()?.split_whitespace().nth(1)?.parse().ok()?;
    Some((status, body))
}

fn content_length(head: &str) -> Option<usize> {
    head.lines().filter_map(|l| l.split_once(':')).find(|(k, _)| k.trim().eq_ignore_ascii_case("content-length")).and_then(|(_, v)| v.trim().parse().ok())
}

/// True once `raw` holds the headers and, if given, Content-Length bytes of body.
fn complete(raw: &[u8]) -> bool {
    let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n") else { return false };
    let head = String::from_utf8_lossy(&raw[..end]);
    content_length(&head).is_some_and(|n| raw.len() >= end + 4 + n)
}

/// GET over HTTP/1.1 (Chromium's devtools server ignores 1.0 requests). Stops at
/// Content-Length if the server keeps the socket open; decodes chunked bodies.
pub fn get(host: &str, port: u16, path: &str) -> Result<(u16, String)> {
    // answered from a recording (`crate::mock`), or recorded
    let port_s = port.to_string();
    let what = ["http", &port_s, path];
    if let Some(answer) = crate::mock::replay(&what) {
        return crate::mock::http_answer(&answer?);
    }
    let result = get_live(host, port, path);
    crate::mock::record(&what, result.as_ref().map(|(s, b)| crate::mock::http_bytes(*s, b)).as_deref().map_err(ToString::to_string));
    result
}

fn get_live(host: &str, port: u16, path: &str) -> Result<(u16, String)> {
    let addr = (host, port).to_socket_addrs()?.next().ok_or_else(|| Error::Failed(format!("can't resolve {host}")))?;
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(5)).map_err(|e| Error::io(format!("connecting to {host}:{port}"), e))?;
    s.set_read_timeout(Some(Duration::from_secs(10)))?;
    write!(s, "GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n")?;
    let mut raw = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match s.read(&mut buf)? {
            0 => break,
            n => raw.extend_from_slice(&buf[..n]),
        }
        if complete(&raw) {
            break;
        }
    }
    let raw = String::from_utf8_lossy(&raw);
    let chunked = raw.split_once("\r\n\r\n").is_some_and(|(head, _)| head.to_lowercase().contains("transfer-encoding: chunked"));
    match parse_response(&raw) {
        Some((status, body)) if chunked => Ok((status, dechunk(body))),
        Some((status, body)) => Ok((status, body.to_string())),
        None => failed(format!("http {host}:{port}{path}: malformed response")),
    }
}

/// Undo `Transfer-Encoding: chunked`.
pub fn dechunk(body: &str) -> String {
    let (mut out, mut rest) = (String::new(), body);
    while let Some((size, after)) = rest.split_once("\r\n") {
        let Ok(n) = usize::from_str_radix(size.split(';').next().unwrap_or("").trim(), 16) else { break };
        if n == 0 || after.len() < n {
            out.push_str(&after[..n.min(after.len())]);
            break;
        }
        out.push_str(&after[..n]);
        rest = after[n..].strip_prefix("\r\n").unwrap_or(&after[n..]);
    }
    out
}

/// A JSON reply, read.
pub fn json(text: &str) -> Option<serde_json::Value> {
    serde_json::from_str(text).ok()
}

/// The first value at `key`, at any depth (depth-first): TV APIs nest the
/// same things differently from one firmware to the next.
pub fn find<'a>(v: &'a serde_json::Value, key: &str) -> Option<&'a serde_json::Value> {
    match v {
        serde_json::Value::Object(m) => m.get(key).or_else(|| m.values().find_map(|x| find(x, key))),
        serde_json::Value::Array(a) => a.iter().find_map(|x| find(x, key)),
        _ => None,
    }
}

/// The string at `key` (in this object).
pub fn text(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key)?.as_str().map(str::to_string)
}

/// The number at `key` (in this object): a JSON number, or a string of one.
pub fn number(v: &serde_json::Value, key: &str) -> Option<f64> {
    match v.get(key)? {
        serde_json::Value::String(s) => s.trim().parse().ok(),
        n => n.as_f64(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_response() {
        let raw = "HTTP/1.1 200 OK\r\nServer: x\r\n\r\n{\"a\":1}";
        assert_eq!(parse_response(raw), Some((200, "{\"a\":1}")));
        assert_eq!(parse_response("garbage"), None);
    }

    #[test]
    fn knows_when_body_is_complete() {
        assert!(!complete(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nab"));
        assert!(complete(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nabcd"));
        assert!(!complete(b"HTTP/1.1 200 OK\r\n\r\nabcd"));
    }

    #[test]
    fn decodes_chunks() {
        assert_eq!(dechunk("4\r\nWiki\r\n5;x=1\r\npedia\r\n0\r\n\r\n"), "Wikipedia");
    }

    #[test]
    fn reads_json() {
        let v = json(r#"{"a":{"b":[{"ccid":"12","name":"x \"y\""}]},"n":3}"#).unwrap();
        let ch = &find(&v, "b").unwrap()[0];
        // numbers as numbers, or as strings
        assert_eq!((number(ch, "ccid"), number(&v, "n")), (Some(12.0), Some(3.0)));
        assert_eq!(text(ch, "name").as_deref(), Some("x \"y\""));
        assert_eq!((text(&v, "nope"), find(&v, "nope")), (None, None));
        assert_eq!(json("not json"), None);
    }
}

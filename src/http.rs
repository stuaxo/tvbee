//! Minimal plain-HTTP GET and a flat-JSON field reader (no crates).

use crate::error::{failed, Error, Result};
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
    head.lines()
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse().ok())
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
    let addr = (host, port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| Error::Failed(format!("can't resolve {host}")))?;
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(5))?;
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
    let chunked = raw
        .split_once("\r\n\r\n")
        .is_some_and(|(head, _)| head.to_lowercase().contains("transfer-encoding: chunked"));
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

/// Top-level objects of a JSON array (or the single object), as slices.
/// Enough for flat API replies; doesn't validate.
pub fn objects(json: &str) -> Vec<&str> {
    let (mut out, mut depth, mut start, mut in_str, mut esc) = (Vec::new(), 0, 0, false, false);
    for (i, ch) in json.char_indices() {
        if in_str {
            match (esc, ch) {
                (true, _) => esc = false,
                (false, '\\') => esc = true,
                (false, '"') => in_str = false,
                _ => {}
            }
            continue;
        }
        match ch {
            '"' => in_str = true,
            '{' => {
                if depth == 0 {
                    start = i;
                }
                depth += 1;
            }
            '}' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    out.push(&json[start..=i]);
                }
            }
            _ => {}
        }
    }
    out
}

/// The string value of `"key": "..."` in a flat JSON object, unescaped.
pub fn string_field(obj: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\"");
    let mut from = 0;
    while let Some(i) = obj[from..].find(&pat) {
        let after = obj[from + i + pat.len()..].trim_start();
        from += i + pat.len();
        let Some(val) = after.strip_prefix(':') else { continue };
        let val = val.trim_start().strip_prefix('"')?;
        return unescape(val);
    }
    None
}

/// A JSON string literal (with its quotes) -> the string.
pub fn json_string(lit: &str) -> Option<String> {
    unescape(lit.trim().strip_prefix('"')?)
}

/// The number value of `"key": 12.5` in a flat JSON object.
pub fn number_field(obj: &str, key: &str) -> Option<f64> {
    let pat = format!("\"{key}\"");
    let i = obj.find(&pat)?;
    let rest = obj[i + pat.len()..].trim_start().strip_prefix(':')?.trim_start();
    let end = rest.find(|c: char| !(c.is_ascii_digit() || matches!(c, '.' | '-' | 'e' | 'E' | '+'))).unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// Read a JSON string body (after the opening quote) up to its closing quote.
fn unescape(s: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                'b' | 'f' => {}
                'u' => {
                    let hex: String = chars.by_ref().take(4).collect();
                    out.push(u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32).unwrap_or('\u{fffd}'));
                }
                other => out.push(other),
            },
            c => out.push(c),
        }
    }
    None
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
    fn reads_values() {
        assert_eq!(json_string("\"Everyone\\u2019s\"").as_deref(), Some("Everyone’s"));
        assert_eq!(json_string("NA"), None);
        let o = r#"{"start_time": 54.0, "title": "x", "end_time": 69}"#;
        assert_eq!(number_field(o, "start_time"), Some(54.0));
        assert_eq!(number_field(o, "end_time"), Some(69.0));
        assert_eq!(number_field(o, "nope"), None);
    }

    #[test]
    fn reads_flat_json() {
        let j = r#"[ { "title": "A \"quoted\" {title}", "url": "https://x/tv#/watch?v=1" },
                     { "title": "café", "type": "page" } ]"#;
        let objs = objects(j);
        assert_eq!(objs.len(), 2);
        assert_eq!(string_field(objs[0], "title").unwrap(), "A \"quoted\" {title}");
        assert_eq!(string_field(objs[0], "url").unwrap(), "https://x/tv#/watch?v=1");
        assert_eq!(string_field(objs[1], "title").unwrap(), "café");
        assert_eq!(string_field(objs[1], "url"), None);
        assert_eq!(objects(r#"{"a":"b"}"#).len(), 1);
    }
}

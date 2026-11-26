//! A small WebSocket client (RFC 6455), text messages only, over a plain
//! `TcpStream`: devtools pages, and anything else that pushes JSON.
//! Also a reader for a raw TCP stream of JSON objects (Kodi's port 9090).

use crate::error::{failed, Result};
use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

pub struct WsClient {
    s: TcpStream,
}

/// A client-to-server frame (clients must mask).
pub fn frame(op: u8, payload: &[u8], mask: [u8; 4]) -> Vec<u8> {
    let mut f = vec![0x80 | op];
    match payload.len() {
        n if n < 126 => f.push(0x80 | n as u8),
        n if n <= 0xffff => {
            f.push(0x80 | 126);
            f.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            f.push(0x80 | 127);
            f.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    f.extend_from_slice(&mask);
    f.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
    f
}

/// A client-to-server text frame.
pub fn text_frame(payload: &[u8], mask: [u8; 4]) -> Vec<u8> {
    frame(0x1, payload, mask)
}

fn mask() -> [u8; 4] {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    (t ^ std::process::id().rotate_left(16)).to_be_bytes()
}

fn read_exact(s: &mut impl Read, n: usize) -> Result<Vec<u8>> {
    let mut buf = vec![0; n];
    s.read_exact(&mut buf)?;
    Ok(buf)
}

/// Read one server frame (servers don't mask): (opcode, payload, final).
pub fn read_frame(s: &mut impl Read) -> Result<(u8, Vec<u8>, bool)> {
    let h = read_exact(s, 2)?;
    let len = match h[1] & 0x7f {
        126 => {
            let b = read_exact(s, 2)?;
            u16::from_be_bytes([b[0], b[1]]) as usize
        }
        127 => u64::from_be_bytes(read_exact(s, 8)?.try_into().unwrap_or_default()) as usize,
        n => n as usize,
    };
    if len > 1 << 22 {
        return failed("websocket: frame too large");
    }
    Ok((h[0] & 0x0f, read_exact(s, len)?, h[0] & 0x80 != 0))
}

/// What [`WsClient::recv`] got.
#[derive(Debug, PartialEq)]
pub enum Recv {
    Text(String),
    /// Nothing within the read timeout.
    Quiet,
    /// The server closed the connection.
    Closed,
}

/// Wait up to the stream's read timeout for something to read: false if
/// nothing came (so a timeout never lands in the middle of a frame).
pub fn ready(s: &TcpStream) -> Result<bool> {
    match s.peek(&mut [0u8; 1]) {
        Ok(0) => Ok(true), // closed: the read says so
        Ok(_) => Ok(true),
        Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => Ok(false),
        Err(e) => Err(e.into()),
    }
}

impl WsClient {
    /// Open `ws://host:port/path`. Reads time out after 5s (see
    /// [`WsClient::set_timeout`]).
    pub fn connect(host: &str, port: u16, path: &str) -> Result<WsClient> {
        let mut s = TcpStream::connect((host, port))?;
        s.set_read_timeout(Some(Duration::from_secs(5)))?;
        // a fixed key is fine: the server's accept hash isn't checked
        write!(
            s,
            "GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: dHZiZWUtZGV2dG9vbHMhIQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
        )?;
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            head.extend(read_exact(&mut s, 1)?);
            if head.len() > 8192 {
                return failed("websocket: bad handshake");
            }
        }
        if !head.starts_with(b"HTTP/1.1 101") {
            return failed("websocket refused");
        }
        Ok(WsClient { s })
    }

    pub fn set_timeout(&self, t: Duration) -> Result<()> {
        Ok(self.s.set_read_timeout(Some(t))?)
    }

    pub fn send_text(&mut self, msg: &str) -> Result<()> {
        Ok(self.s.write_all(&text_frame(msg.as_bytes(), mask()))?)
    }

    /// The next text message; None when the server closes. Pings are
    /// answered on the way. Waits up to the read timeout, then fails.
    pub fn recv_text(&mut self) -> Result<Option<String>> {
        match self.recv()? {
            Recv::Text(t) => Ok(Some(t)),
            Recv::Closed => Ok(None),
            Recv::Quiet => failed("websocket: no reply"),
        }
    }

    /// The next text message, or that none came within the read timeout,
    /// or that the server closed. Pings are answered on the way.
    pub fn recv(&mut self) -> Result<Recv> {
        if !ready(&self.s)? {
            return Ok(Recv::Quiet);
        }
        let mut msg: Vec<u8> = Vec::new();
        loop {
            let (op, data, fin) = read_frame(&mut self.s)?;
            match op {
                // text, binary, or more of one
                0x0..=0x2 => {
                    msg.extend(data);
                    if fin {
                        return Ok(Recv::Text(String::from_utf8_lossy(&msg).into_owned()));
                    }
                }
                0x8 => {
                    let _ = self.s.write_all(&frame(0x8, &[], mask()));
                    return Ok(Recv::Closed);
                }
                0x9 => self.s.write_all(&frame(0xA, &data, mask()))?,
                _ => {}
            }
        }
    }
}

/// Splits a byte stream of JSON objects with nothing between them (Kodi's
/// TCP API) into whole objects.
#[derive(Default)]
pub struct JsonSplitter {
    buf: Vec<u8>,
}

impl JsonSplitter {
    /// Add bytes read; returns the objects completed.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        let (mut depth, mut in_str, mut esc, mut start) = (0usize, false, false, None);
        let mut used = 0;
        for (i, &b) in self.buf.iter().enumerate() {
            if in_str {
                match (esc, b) {
                    (true, _) => esc = false,
                    (false, b'\\') => esc = true,
                    (false, b'"') => in_str = false,
                    _ => {}
                }
                continue;
            }
            match b {
                b'"' => in_str = true,
                b'{' | b'[' => {
                    if depth == 0 {
                        start = Some(i);
                    }
                    depth += 1;
                }
                b'}' | b']' if depth > 0 => {
                    depth -= 1;
                    if let (0, Some(s)) = (depth, start) {
                        out.push(String::from_utf8_lossy(&self.buf[s..=i]).into_owned());
                        used = i + 1;
                        start = None;
                    }
                }
                // stray bytes between objects (newlines)
                _ if depth == 0 => used = i + 1,
                _ => {}
            }
        }
        self.buf.drain(..used);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_are_masked() {
        let f = text_frame(b"hi", [1, 2, 3, 4]);
        assert_eq!(f, vec![0x81, 0x82, 1, 2, 3, 4, b'h' ^ 1, b'i' ^ 2]);
        let long = text_frame(&[0; 300], [0; 4]);
        assert_eq!(&long[1..4], &[0x80 | 126, 1, 44]);
        assert_eq!(frame(0xA, b"", [0; 4])[..2], [0x8A, 0x80]);
    }

    #[test]
    fn reads_frames() {
        let mut short: &[u8] = &[0x81, 2, b'o', b'k'];
        assert_eq!(read_frame(&mut short).unwrap(), (1, b"ok".to_vec(), true));
        let mut long = vec![0x01, 126, 1, 44];
        long.extend([b'x'; 300]);
        let (op, data, fin) = read_frame(&mut &long[..]).unwrap();
        assert_eq!((op, data.len(), fin), (1, 300, false));
    }

    #[test]
    fn splits_json_stream() {
        let mut j = JsonSplitter::default();
        assert_eq!(j.push(br#"{"a":"}{"}{"b":"#), [r#"{"a":"}{"}"#]);
        assert_eq!(j.push(b"[1,{\"c\":\"\\\"\"}]}\n{\"d\":1}"), [r#"{"b":[1,{"c":"\""}]}"#, r#"{"d":1}"#]);
        assert!(j.push(b"\n").is_empty());
        assert!(j.buf.is_empty());
    }
}

//! Kodi's JSON-RPC methods as types (the plan's part 8): each says its
//! name, its parameters (built as JSON, never by hand) and what its result
//! means. [`Rpc`] makes one an [`Ask`]: a JSON-RPC 2.0 request with Kodi's
//! login, sent the way everything asked of the TV is ([`ask`]).

use super::{Error, Result, Tv, failed, login, off, ping};
use serde_json::{Value, json};
use tveee_core::ask::{Ask, Auth, Http, Reply, Request, Retry};

/// A Kodi JSON-RPC method.
pub trait Method {
    /// What its result means, as a type.
    type Out;
    /// Its name: "Files.GetDirectory".
    fn name(&self) -> &str;
    /// Its parameters, as JSON.
    fn params(&self) -> Value;
    /// What its `result` means.
    fn parse(&self, result: Value) -> Result<Self::Out>;
}

/// A method as something to ask the TV.
pub struct Rpc<'a, M>(pub &'a M, pub u32);

impl<M: Method> Ask for Rpc<'_, M> {
    type Answer = M::Out;
    fn name(&self) -> String {
        format!("Kodi {}", self.0.name())
    }
    fn request(&self, tv: &Tv) -> Result<Request> {
        let (port, user, password) = login(tv);
        let body = json!({ "jsonrpc": "2.0", "id": 1, "method": self.0.name(), "params": self.0.params() }).to_string();
        let auth = if password.is_empty() { Auth::None } else { Auth::Basic { user, password } };
        Ok(Request::Http(Http { url: format!("http://{}:{port}/jsonrpc", tv.ip), body: Some(body), auth, timeout_secs: self.1, retry: Retry::Never }))
    }
    fn parse(&self, reply: &Reply) -> Result<Option<M::Out>> {
        match reply.status {
            200 => self.0.parse(result_of(&reply.body, self.0.name())?).map(Some),
            401 => failed("Kodi wants a username and password: run 'tveee pair kodi'"),
            code => failed(format!("Kodi answered HTTP {code}")),
        }
    }
}

/// The `result` of a JSON-RPC reply, or its error.
pub fn result_of(reply: &str, method: &str) -> Result<Value> {
    let Ok(mut v) = serde_json::from_str::<Value>(reply) else { return failed(format!("Kodi: {method}: not JSON: {}", reply.trim())) };
    if let Some(e) = v.get("error") {
        let msg = e.get("message").and_then(Value::as_str).map_or_else(|| e.to_string(), str::to_string);
        return failed(format!("Kodi: {method}: {msg}"));
    }
    match v.get_mut("result") {
        Some(r) => Ok(r.take()),
        None => failed(format!("Kodi: {method}: no result")),
    }
}

/// Ask Kodi `m`, waiting up to 4s.
pub fn ask<M: Method>(tv: &Tv, m: &M) -> Result<M::Out> {
    ask_within(tv, m, 4)
}

/// Ask Kodi `m`, waiting up to `secs`. Not reached: Kodi is off, or (it
/// answers a ping) just slow, which is said as such.
pub fn ask_within<M: Method>(tv: &Tv, m: &M, secs: u32) -> Result<M::Out> {
    let rpc = Rpc(m, secs);
    let slow = || failed(format!("Kodi didn't answer within {secs}s (it's running: a slow share or a big library?)"));
    let reply = match tv.send(&rpc.request(tv)?) {
        Ok(r) => r,
        // what the failure was says it: nothing listening is Kodi off,
        // no answer in time is Kodi busy
        Err(e) if e.refused() => return Err(off()),
        Err(e) if e.timed_out() => return slow(),
        // else ask whether it's there at all
        Err(_) if m.name() != "JSONRPC.Ping" && ping(tv) => return slow(),
        Err(_) => return Err(off()),
    };
    match rpc.parse(&reply)? {
        Some(out) => Ok(out),
        None => Err(Error::Failed(format!("Kodi: {}: no answer", m.name()))),
    }
}

/// Any method by name, its params as JSON, its result as it comes (the
/// `kodi-rpc` command, and calls not typed yet).
pub struct Raw<'a> {
    /// Its name: "Player.GetItem".
    pub method: &'a str,
    /// Its parameters, as JSON.
    pub params: Value,
}

impl Method for Raw<'_> {
    type Out = Value;
    fn name(&self) -> &str {
        self.method
    }
    fn params(&self) -> Value {
        self.params.clone()
    }
    fn parse(&self, result: Value) -> Result<Value> {
        Ok(result)
    }
}

// --- the library browser's methods ----------------------------------------------

/// What's in a folder Kodi knows (`Files.GetDirectory`), with the song and
/// video details the list shows.
pub struct GetDirectory<'a> {
    /// The folder: a path or a library address (musicdb://albums/).
    pub directory: &'a str,
    /// "music", "video", "files".
    pub media: &'a str,
}

impl Method for GetDirectory<'_> {
    type Out = Value;
    fn name(&self) -> &str {
        "Files.GetDirectory"
    }
    fn params(&self) -> Value {
        json!({ "directory": self.directory, "media": self.media, "properties": ["title", "artist", "duration", "track"] })
    }
    fn parse(&self, result: Value) -> Result<Value> {
        Ok(result)
    }
}

/// The shares and folders added to Kodi for `media` (`Files.GetSources`).
pub struct GetSources<'a> {
    /// "music" or "video".
    pub media: &'a str,
}

impl Method for GetSources<'_> {
    type Out = Value;
    fn name(&self) -> &str {
        "Files.GetSources"
    }
    fn params(&self) -> Value {
        json!({ "media": self.media })
    }
    fn parse(&self, result: Value) -> Result<Value> {
        Ok(result)
    }
}

/// Empty playlist `0` (`Playlist.Clear`).
pub struct PlaylistClear(pub u32);

impl Method for PlaylistClear {
    type Out = ();
    fn name(&self) -> &str {
        "Playlist.Clear"
    }
    fn params(&self) -> Value {
        json!({ "playlistid": self.0 })
    }
    fn parse(&self, _: Value) -> Result<()> {
        Ok(())
    }
}

/// Add a whole folder to a playlist (`Playlist.Add`).
pub struct PlaylistAddDirectory<'a> {
    /// Kodi's playlist (0 music, 1 video).
    pub playlist: u32,
    /// The folder whose items are added.
    pub directory: &'a str,
    /// "music" or "video".
    pub media: &'a str,
}

impl Method for PlaylistAddDirectory<'_> {
    type Out = ();
    fn name(&self) -> &str {
        "Playlist.Add"
    }
    fn params(&self) -> Value {
        json!({ "playlistid": self.playlist, "item": { "directory": self.directory, "media": self.media } })
    }
    fn parse(&self, _: Value) -> Result<()> {
        Ok(())
    }
}

/// The files in a playlist, in order (`Playlist.GetItems`).
pub struct PlaylistFiles(pub u32);

impl Method for PlaylistFiles {
    type Out = Vec<String>;
    fn name(&self) -> &str {
        "Playlist.GetItems"
    }
    fn params(&self) -> Value {
        json!({ "playlistid": self.0, "properties": ["file"] })
    }
    fn parse(&self, result: Value) -> Result<Vec<String>> {
        let items = result.get("items").and_then(Value::as_array).cloned().unwrap_or_default();
        Ok(items.iter().map(|i| i.get("file").and_then(Value::as_str).unwrap_or("").to_string()).collect())
    }
}

/// Play something (`Player.Open`).
pub enum Open<'a> {
    /// A file, by its path.
    File(&'a str),
    /// A playlist, from one of its items.
    Playlist {
        /// Kodi's playlist (0 music, 1 video).
        playlist: u32,
        /// The item to start from, from 0.
        position: usize,
    },
}

impl Method for Open<'_> {
    type Out = ();
    fn name(&self) -> &str {
        "Player.Open"
    }
    fn params(&self) -> Value {
        match self {
            Open::File(f) => json!({ "item": { "file": f } }),
            Open::Playlist { playlist, position } => json!({ "item": { "playlistid": playlist, "position": position } }),
        }
    }
    fn parse(&self, _: Value) -> Result<()> {
        Ok(())
    }
}

/// The file a player is playing (`Player.GetItem`); None: none.
pub struct PlayingFile(pub u32);

impl Method for PlayingFile {
    type Out = Option<String>;
    fn name(&self) -> &str {
        "Player.GetItem"
    }
    fn params(&self) -> Value {
        json!({ "playerid": self.0, "properties": ["file"] })
    }
    fn parse(&self, result: Value) -> Result<Option<String>> {
        Ok(result.pointer("/item/file").and_then(Value::as_str).filter(|f| !f.is_empty()).map(str::to_string))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn methods_build_their_params() {
        // a path with quotes and backslashes: JSON, not hand-built
        let d = GetDirectory { directory: r#"smb://nas/it's "here"\x/"#, media: "music" };
        assert_eq!(d.params()["directory"], r#"smb://nas/it's "here"\x/"#);
        assert_eq!(d.params()["properties"][3], "track");
        assert_eq!(Open::Playlist { playlist: 0, position: 2 }.params(), json!({ "item": { "playlistid": 0, "position": 2 } }));
        assert_eq!(Open::File("a.mp3").params(), json!({ "item": { "file": "a.mp3" } }));
        assert_eq!(PlaylistAddDirectory { playlist: 1, directory: "videodb://tvshows/", media: "video" }.params()["item"]["media"], "video");
    }

    #[test]
    fn results_parsed_in_one_place() {
        let files = PlaylistFiles(0).parse(json!({ "items": [{ "file": "a" }, { "file": "b" }] })).unwrap();
        assert_eq!(files, ["a", "b"]);
        assert_eq!(PlaylistFiles(0).parse(json!({})).unwrap(), Vec::<String>::new());
        assert_eq!(PlayingFile(0).parse(json!({ "item": { "file": "x.mp3" } })).unwrap().as_deref(), Some("x.mp3"));
        assert_eq!(PlayingFile(0).parse(json!({ "item": { "file": "" } })).unwrap(), None);
    }

    #[test]
    fn replies_and_errors() {
        let m = Raw { method: "JSONRPC.Ping", params: json!({}) };
        let rpc = Rpc(&m, 4);
        let ok = Reply { status: 200, body: r#"{"jsonrpc":"2.0","id":1,"result":"pong"}"#.into() };
        assert_eq!(rpc.parse(&ok).unwrap(), Some(json!("pong")));
        let err = Reply { status: 200, body: r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"Method not found."}}"#.into() };
        assert_eq!(rpc.parse(&err).unwrap_err().to_string(), "Kodi: JSONRPC.Ping: Method not found.");
        assert!(rpc.parse(&Reply { status: 401, body: String::new() }).unwrap_err().to_string().contains("tveee pair kodi"));
    }
}

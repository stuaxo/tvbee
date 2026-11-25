//! Providers: sources of content an app gives access to. Live TV over the
//! air (hosted by Philips' TV app), YouTube, later Kodi or the NetTV apps.
//!
//! An app is something you launch; a provider is what tveee can show and do
//! with it. Each capability is its own small trait ([`Guide`], [`Channels`],
//! [`Details`]); a provider opts in by implementing the trait and returning
//! itself from the matching accessor on [`Provider`]. Plugins register
//! providers (`Plugin::with_providers`) the way they register commands and
//! apps, and the core works with whichever provider is in front.

use crate::error::Result;
use crate::more::Place;
use crate::tv::Tv;

/// What a provider gets to work with.
pub struct Cx<'a> {
    pub tv: &'a Tv,
}

/// A channel. `id` is the provider's own id for it (what guide events refer
/// to); `number` is what you'd type on the remote.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Channel {
    pub id: u32,
    pub number: Option<String>,
    pub name: Option<String>,
    pub radio: bool,
    /// In one of the user's favourite lists.
    pub favourite: bool,
    /// How it's broadcast, if the provider says.
    pub delivery: Option<Delivery>,
    /// Its DVB service: (original network, transport stream, service). The
    /// same after a retune, unlike `id`, and the same in any guide.
    pub dvb: Option<(u16, u16, u16)>,
}

/// How a channel reaches the TV.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Delivery {
    DvbT,
    DvbT2,
    DvbC,
    DvbS,
    Analog,
    Other,
}

impl Delivery {
    /// From the names TVs use: "DVB_T", "DVB-T2", "analog", ...
    pub fn parse(s: &str) -> Delivery {
        match s.to_ascii_uppercase().replace('-', "_").as_str() {
            "DVB_T" => Delivery::DvbT,
            "DVB_T2" => Delivery::DvbT2,
            "DVB_C" | "DVB_C2" => Delivery::DvbC,
            "DVB_S" | "DVB_S2" => Delivery::DvbS,
            "ANALOG" | "ANALOGUE" => Delivery::Analog,
            _ => Delivery::Other,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Delivery::DvbT => "DVB-T",
            Delivery::DvbT2 => "DVB-T2",
            Delivery::DvbC => "DVB-C",
            Delivery::DvbS => "DVB-S",
            Delivery::Analog => "analogue",
            Delivery::Other => "other",
        }
    }
}

/// Where a provider's channels come from: the country the TV was set up
/// for, the DVB network, and how they're broadcast.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Network {
    pub country: Option<String>,
    /// The DVB original network id most of the channels share.
    pub onid: Option<u16>,
    pub deliveries: Vec<Delivery>,
}

impl Network {
    /// From the channels and the country: the commonest network id, and
    /// every kind of broadcast in use (analogue inputs left out).
    pub fn of(channels: &[Channel], country: Option<String>) -> Network {
        let mut count: std::collections::BTreeMap<u16, usize> = std::collections::BTreeMap::new();
        for (onid, _, _) in channels.iter().filter_map(|c| c.dvb) {
            *count.entry(onid).or_default() += 1;
        }
        let onid = count.into_iter().max_by_key(|(_, n)| *n).map(|(o, _)| o);
        let mut deliveries: Vec<Delivery> = channels.iter().filter_map(|c| c.delivery).filter(|d| *d != Delivery::Analog).collect();
        deliveries.sort();
        deliveries.dedup();
        Network { country, onid, deliveries }
    }
}

impl Channel {
    /// "4 Channel 4", or the id when that's all there is.
    pub fn label(&self) -> String {
        match (&self.number, &self.name) {
            (Some(n), Some(name)) => format!("{n} {name}"),
            (None, Some(name)) => name.clone(),
            (Some(n), None) => n.clone(),
            (None, None) => format!("#{}", self.id),
        }
    }

    /// Sort key: by channel number, then id.
    pub fn order(&self) -> (u32, u32) {
        let n = self.number.as_deref().and_then(|n| n.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok());
        (n.unwrap_or(u32::MAX), self.id)
    }
}

/// Find a channel by what someone typed: its number, its name, or the start
/// (then any part) of its name, ignoring case.
pub fn find_channel<'a>(list: &'a [Channel], query: &str) -> Option<&'a Channel> {
    let q = query.trim().to_lowercase();
    let name = |c: &Channel| c.name.as_deref().unwrap_or("").to_lowercase();
    list.iter()
        .find(|c| c.number.as_deref() == Some(q.as_str()))
        .or_else(|| list.iter().find(|c| name(c) == q))
        .or_else(|| list.iter().filter(|c| name(c).starts_with(&q)).min_by_key(|c| c.order()))
        .or_else(|| list.iter().filter(|c| name(c).contains(&q)).min_by_key(|c| c.order()))
}

/// A programme in a guide. Times are Unix seconds.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    /// The [`Channel::id`] it's on.
    pub channel: u32,
    pub title: String,
    pub start: i64,
    pub end: i64,
    pub genre: Option<u32>,
    /// What it's about, from the broadcast guide.
    pub info: Option<String>,
}

impl Event {
    /// The genre's name, where the provider's number is known.
    pub fn genre_name(&self) -> Option<&'static str> {
        // Philips' own numbering, worked out from the titles it labels; only
        // the clear ones (1, 2, 9 and 17 are mixed bags)
        Some(match self.genre? {
            0 => "film",
            3 => "sport",
            4 => "children's",
            5 => "music",
            6 => "documentary",
            7 => "news",
            11 => "drama",
            _ => return None,
        })
    }
}

/// What's on a channel now, and what's next.
#[derive(Debug, Clone, PartialEq)]
pub struct NowNext {
    pub channel: u32,
    pub now: Option<Event>,
    pub next: Option<Event>,
}

/// A programme guide.
pub trait Guide {
    /// Now and next on every channel that has guide data, at time `at`.
    fn now_next(&self, cx: &Cx, at: i64) -> Result<Vec<NowNext>>;
    /// One channel's (by [`Channel::id`]) programmes between `from` and `to`.
    fn schedule(&self, cx: &Cx, channel: u32, from: i64, to: i64) -> Result<Vec<Event>>;
    /// What the programme starting at `start` on `channel` is about, if the
    /// guide says. May be slow the first time (the schedule doesn't carry
    /// descriptions when they're costly to read).
    fn info(&self, _cx: &Cx, _channel: u32, _start: i64) -> Result<Option<String>> {
        Ok(None)
    }
}

/// Channels you can switch between.
pub trait Channels {
    fn list(&self, cx: &Cx) -> Result<Vec<Channel>>;
    /// The channel on now, if the provider can tell.
    fn current(&self, _cx: &Cx) -> Result<Option<Channel>> {
        Ok(None)
    }
    /// Switch to a channel; returns status-line feedback.
    fn tune(&self, cx: &Cx, channel: &Channel) -> Result<String>;
    /// Where the channels come from (country, network, broadcast kind).
    fn network(&self, _cx: &Cx) -> Option<Network> {
        None
    }
    /// The favourite channels (by id) as the TV has them; None if this
    /// provider has no favourites. tveee syncs with these (`favourites`).
    fn favourites(&self, _cx: &Cx) -> Option<Result<std::collections::BTreeSet<u32>>> {
        None
    }
    /// Make channel `id` a favourite or not on the TV. Ok(false): it can't
    /// right now (try again later).
    fn write_favourite(&self, _cx: &Cx, _id: u32, _on: bool) -> Result<bool> {
        crate::error::failed("favourites can't be changed here")
    }
}

/// More about what's playing (description, chapters, ...).
pub trait Details {
    /// Details for the link [`Provider::watch_link`] made for what's on.
    fn details(&self, tv: &crate::tv::Tv, link: &str) -> Result<crate::more::Details>;
}

/// A place in an app to come back to: opening `link` in the app gets there.
#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    pub link: String,
    /// What it is, for messages: "at 12:34 in the same video".
    pub said: String,
    /// Where to start `link`, for apps the link alone can't say it to.
    pub resume: Option<f64>,
}

/// What a provider hook did: it handled it; it can't here (the app's API
/// isn't there, nothing's playing, ...), so do it the generic way (the media
/// session, keys, an intent); or it tried and something broke.
#[derive(Debug)]
pub enum Outcome<T> {
    Handled(T),
    Fallback,
    Failed(crate::error::Error),
}

impl<T> Outcome<T> {
    /// The result if handled.
    pub fn handled(self) -> Option<T> {
        match self {
            Outcome::Handled(t) => Some(t),
            _ => None,
        }
    }

    /// Handled: Some; fall back: None; failed: the error.
    pub fn or_fallback(self) -> crate::error::Result<Option<T>> {
        match self {
            Outcome::Handled(t) => Ok(Some(t)),
            Outcome::Fallback => Ok(None),
            Outcome::Failed(e) => Err(e),
        }
    }
}

/// An API that isn't there (`Error::Unavailable`) is a fallback; other
/// errors are failures.
impl<T> From<crate::error::Result<T>> for Outcome<T> {
    fn from(r: crate::error::Result<T>) -> Self {
        match r {
            Ok(t) => Outcome::Handled(t),
            Err(crate::error::Error::Unavailable(_)) => Outcome::Fallback,
            Err(e) => Outcome::Failed(e),
        }
    }
}

/// What a player app says is on: more than Android's media session gives.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Playing {
    pub title: Option<String>,
    /// The show and episode ("Taskmaster S03E04"), or the artist.
    pub subtitle: Option<String>,
    pub progress: Option<crate::devtools::Progress>,
    /// A link to it (`tveee goto`, `details`).
    pub link: Option<String>,
    /// A picture of it (poster, fanart, a frame), for [`Player::picture`].
    pub art: Option<String>,
    /// The picture changes as it plays (a frame of the video): ask again
    /// with the position, rather than once.
    pub art_moves: bool,
    /// More to say: ("video", "1080p H.264"), ("audio", "English AC3 5.1"), ...
    pub extra: Vec<(&'static str, String)>,
}

/// An app tveee can ask what it's playing and tell what to do, through an
/// API of its own (Kodi's JSON-RPC).
pub trait Player: Sync {
    /// What's playing (fall back: nothing, or the app can't say).
    fn playing(&self, tv: &crate::tv::Tv) -> Outcome<Playing>;
    /// Move what's playing; returns the new position in seconds.
    fn seek(&self, tv: &crate::tv::Tv, to: crate::devtools::SeekTo) -> Outcome<f64>;
    fn play_pause(&self, _tv: &crate::tv::Tv) -> Outcome<()> {
        Outcome::Fallback
    }
    /// Picture `art` (from [`Playing::art`]), as image file bytes, for the
    /// ui's preview; `at`: how far in it is now, for pictures that move.
    #[cfg_attr(not(feature = "ui"), allow(dead_code))]
    fn picture(&self, _tv: &crate::tv::Tv, _art: &str, _at: Option<f64>) -> Outcome<Vec<u8>> {
        Outcome::Fallback
    }
    /// Push: call `changed` (with what happened: "pause", "Player.OnSeek")
    /// as soon as the app says playback changed, rather than waiting to be
    /// asked; until `go` says to stop (it's asked at least every second).
    /// Fall back: the app can't say (it's asked every second instead).
    #[cfg_attr(not(feature = "ui"), allow(dead_code))]
    fn watch(&self, _tv: &crate::tv::Tv, _changed: &dyn Fn(&str), _go: &dyn Fn() -> bool) -> Outcome<()> {
        Outcome::Fallback
    }
    /// Stop anything started in the background for pictures (a download):
    /// the preview has moved on.
    #[cfg_attr(not(feature = "ui"), allow(dead_code))]
    fn stop(&self) {}
}

/// A source of content. Everything but the identity is optional.
pub trait Provider: Sync {
    /// Short id: `tveee guide --provider <id>`.
    fn id(&self) -> &'static str;
    fn name(&self) -> &'static str;
    /// The packages it lives in.
    fn hosts(&self) -> &'static [&'static str];

    /// A search-results link with `{q}` for the query.
    fn search_link(&self) -> Option<&'static str> {
        None
    }
    /// Close the app before sending it a link (see the YouTube provider).
    fn fresh_for_links(&self) -> bool {
        false
    }
    /// Where the user is in the app (a video, and how far in), so it can be
    /// brought back there: after restarting the app, say. None: nothing
    /// worth keeping, or the app can't say.
    fn save_session(&self, _tv: &crate::tv::Tv) -> Outcome<Session> {
        Outcome::Fallback
    }
    /// Whether [`Provider::save_session`] does anything (for listing).
    fn has_sessions(&self) -> bool {
        false
    }
    /// Start the app (closed: `target` is what to start) and bring it back
    /// to a saved session. Fall back: open the session's link the usual way.
    fn restore_session(&self, _tv: &crate::tv::Tv, _target: &str, _s: &Session) -> Outcome<()> {
        Outcome::Fallback
    }
    /// The app's own API for what's playing and controlling it.
    fn player(&self) -> Option<&dyn Player> {
        None
    }
    /// Hand `link` to the app where it is, when it's already in front,
    /// without starting an activity (which, with `fresh_for_links`, means
    /// closing it first). Fall back: send the link as usual.
    fn open_here(&self, _tv: &crate::tv::Tv, _link: &str) -> Outcome<()> {
        Outcome::Fallback
    }
    /// Fixed places in the app (`tveee goto <id>`).
    fn places(&self) -> &'static [Place] {
        &[]
    }

    fn details(&self) -> Option<&dyn Details> {
        None
    }
    fn guide(&self) -> Option<&dyn Guide> {
        None
    }
    fn channels(&self) -> Option<&dyn Channels> {
        None
    }
}

/// "Freeview, United Kingdom (DVB-T, DVB-T2)": the lineup's name where one
/// is known, else the country; then how it's broadcast.
pub fn describe(net: &Network) -> String {
    let place = match (crate::lineups::find(net), &net.country) {
        (Some(l), _) => l.title(),
        (None, Some(c)) => c.clone(),
        (None, None) => "unknown network".into(),
    };
    let how: Vec<&str> = net.deliveries.iter().map(|d| d.as_str()).collect();
    if how.is_empty() { place } else { format!("{place} ({})", how.join(", ")) }
}

/// The capabilities a provider has, by name (for `tveee providers`).
pub fn capabilities(p: &dyn Provider) -> Vec<&'static str> {
    let mut v = Vec::new();
    if p.guide().is_some() {
        v.push("guide");
    }
    if p.channels().is_some() {
        v.push("channels");
    }
    if p.details().is_some() {
        v.push("details");
    }
    if !p.places().is_empty() {
        v.push("places");
    }
    if p.search_link().is_some() {
        v.push("search");
    }
    if p.has_sessions() {
        v.push("sessions");
    }
    if p.player().is_some() {
        v.push("player");
    }
    v
}

static REGISTRY: std::sync::OnceLock<Vec<&'static dyn Provider>> = std::sync::OnceLock::new();

/// Set the providers of the enabled plugins. Call once, early (main does).
pub fn install(providers: Vec<&'static dyn Provider>) {
    let _ = REGISTRY.set(providers);
}

/// Every provider. Without [`install`] (tests) that's every plugin's.
pub fn all() -> &'static [&'static dyn Provider] {
    REGISTRY.get_or_init(|| crate::plugins::all().iter().flat_map(|p| (p.providers)()).collect())
}

/// The provider hosted by `pkg`, if any.
pub fn for_package(pkg: &str) -> Option<&'static dyn Provider> {
    all().iter().copied().find(|p| p.hosts().contains(&pkg))
}

/// A provider by id, or the first one with a guide.
pub fn with_guide(id: Option<&str>) -> Option<&'static dyn Provider> {
    all().iter().copied().find(|p| p.guide().is_some() && id.is_none_or(|i| p.id() == i))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcomes() {
        use crate::error::Error;
        let unavailable: Outcome<u8> = Err(Error::Unavailable("off".into())).into();
        assert!(matches!(unavailable, Outcome::Fallback));
        let broke: Outcome<u8> = Err(Error::Failed("bad".into())).into();
        assert!(broke.or_fallback().is_err());
        let done: Outcome<u8> = Ok(3).into();
        assert_eq!(done.handled(), Some(3));
        assert_eq!(Outcome::<u8>::Fallback.or_fallback().unwrap(), None);
    }

    #[test]
    fn registry_has_the_built_in_providers() {
        let ids: Vec<_> = all().iter().map(|p| p.id()).collect();
        assert!(ids.contains(&"youtube") && ids.contains(&"freeview"), "{ids:?}");
        let yt = for_package("com.google.android.youtube.tv").unwrap();
        assert_eq!(capabilities(yt), vec!["details", "places", "search", "sessions", "player"]);
        let tv = with_guide(None).unwrap();
        assert_eq!(tv.id(), "freeview");
        assert_eq!(capabilities(tv), vec!["guide", "channels"]);
        assert!(with_guide(Some("youtube")).is_none());
    }

    #[test]
    fn finds_channels() {
        let ch = |id, n: &str, name: &str| Channel { id, number: Some(n.into()), name: Some(name.into()), radio: false, favourite: false, delivery: None, dvb: None };
        let list = vec![ch(159, "1", "BBC ONE Lon"), ch(158, "2", "BBC TWO"), ch(156, "4", "Channel 4"), ch(143, "18", "More 4"), ch(9, "101", "BBC ONE Lon HD")];
        let id = |q| find_channel(&list, q).map(|c| c.id);
        assert_eq!(id("4"), Some(156));
        assert_eq!(id("more 4"), Some(143));
        assert_eq!(id("bbc one"), Some(159)); // the lower number wins
        assert_eq!(id("two"), Some(158));
        assert_eq!(id("nope"), None);
        assert_eq!(list[3].label(), "18 More 4");
        assert!(list[0].order() < list[4].order());
    }
}

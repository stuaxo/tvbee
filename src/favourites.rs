//! Favourite channels, kept in sync between tvbee and the TV.
//!
//! The TV is the real list, but it can't always take changes (Philips TVs
//! only accept them with live TV in front), and it can be changed from its
//! own menus. So tvbee keeps, per provider and TV:
//!
//! - what it last read from the TV, and when (`seen`), and
//! - changes made in tvbee that the TV hasn't taken yet (`want`), each with
//!   when it was made.
//!
//! Every read of the TV's favourites is a sync. A channel that differs from
//! the last read was changed on the TV itself, some time after that read.
//! A pending change of ours goes to the TV, unless the TV changed that
//! channel after we made it (the TV's change is newer, so it wins).

use crate::error::Result;
use crate::provider::{Channel, Cx, Provider};
use std::collections::{BTreeMap, BTreeSet};

/// What's kept between runs.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Store {
    /// When the TV's favourites were last read (Unix seconds; 0: never).
    pub seen_at: i64,
    /// The favourites as the TV had them then.
    pub seen: BTreeSet<u32>,
    /// Changes made in tvbee not on the TV yet: channel -> (favourite?, when).
    pub want: BTreeMap<u32, (bool, i64)>,
}

impl Store {
    /// `seen <time>`, then a `tv <id>` line per favourite, a `want <id> <0|1> <time>` per pending change.
    pub fn parse(text: &str) -> Store {
        let mut s = Store::default();
        for l in text.lines() {
            let f: Vec<&str> = l.split_whitespace().collect();
            match f.as_slice() {
                ["seen", t] => s.seen_at = t.parse().unwrap_or(0),
                ["tv", id] => s.seen.extend(id.parse::<u32>().ok()),
                ["want", id, on, t] => {
                    if let (Ok(id), Ok(t)) = (id.parse(), t.parse()) {
                        s.want.insert(id, (*on == "1", t));
                    }
                }
                _ => {}
            }
        }
        s
    }

    pub fn render(&self, header: &str) -> String {
        let mut out = format!("# {header}\nseen {}\n", self.seen_at);
        for id in &self.seen {
            out.push_str(&format!("tv {id}\n"));
        }
        for (id, (on, t)) in &self.want {
            out.push_str(&format!("want {id} {} {t}\n", u8::from(*on)));
        }
        out
    }
}

/// What to do with a pending change, given the TV's favourites now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decide {
    /// The TV already has it that way.
    Done,
    /// The TV changed that channel after our change was made: keep the TV's.
    TvWins,
    /// Send ours.
    Send,
}

pub fn decide(store: &Store, tv: &BTreeSet<u32>, id: u32, on: bool, at: i64) -> Decide {
    let changed_on_tv = store.seen_at > 0 && store.seen.contains(&id) != tv.contains(&id);
    if tv.contains(&id) == on {
        Decide::Done
    } else if changed_on_tv && at <= store.seen_at {
        // ours was already known at the last read, and the TV changed since
        Decide::TvWins
    } else {
        Decide::Send
    }
}

/// The outcome of a sync.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Sync {
    /// The favourites as tvbee shows them: the TV's, with pending changes.
    pub favourites: BTreeSet<u32>,
    /// Changes not on the TV yet.
    pub pending: BTreeMap<u32, bool>,
    /// Changed on the TV itself since tvbee last looked.
    pub on_tv: Vec<(u32, bool)>,
    /// Pending changes that just went to the TV.
    pub sent: Vec<(u32, bool)>,
    /// Pending changes dropped: the TV changed the channel later.
    pub overruled: Vec<(u32, bool)>,
    /// Why the TV couldn't be read or written, if it couldn't.
    pub problem: Option<String>,
}

impl Sync {
    /// What's worth telling the user, in one line (labels from `channels`).
    pub fn news(&self, channels: &[Channel]) -> Option<String> {
        let name = |id: &u32| channels.iter().find(|c| c.id == *id).map_or_else(|| format!("#{id}"), Channel::label);
        let list = |v: &[(u32, bool)]| v.iter().map(|(id, on)| format!("{}{}", if *on { "+" } else { "-" }, name(id))).collect::<Vec<_>>().join(", ");
        let mut parts = Vec::new();
        if !self.on_tv.is_empty() {
            parts.push(format!("changed on the TV: {}", list(&self.on_tv)));
        }
        if !self.sent.is_empty() {
            parts.push(format!("sent to the TV: {}", list(&self.sent)));
        }
        if !self.overruled.is_empty() {
            parts.push(format!("kept the TV's newer change for {}", self.overruled.iter().map(|(id, _)| name(id)).collect::<Vec<_>>().join(", ")));
        }
        (!parts.is_empty()).then(|| parts.join("; "))
    }
}

fn file(provider: &str, ip: &str) -> Option<std::path::PathBuf> {
    Some(crate::dotenv::config_dir()?.join("favourites").join(format!("{provider}-{ip}.txt")))
}

pub fn load(provider: &str, ip: &str) -> Store {
    file(provider, ip).and_then(|f| std::fs::read_to_string(f).ok()).map(|t| Store::parse(&t)).unwrap_or_default()
}

fn save(provider: &str, ip: &str, store: &Store) -> Result<()> {
    let Some(f) = file(provider, ip) else { return Ok(()) };
    if let Some(dir) = f.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(f, store.render(&format!("favourites tvbee last read from {provider} on {ip}, and changes waiting to go to it")))?;
    Ok(())
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// Read the TV's favourites and settle pending changes both ways. None if
/// the provider has no favourites.
pub fn sync(p: &dyn Provider, cx: &Cx) -> Option<Sync> {
    let chans = p.channels()?;
    let read = chans.favourites(cx)?;
    let ip = &cx.tv.ip;
    let mut store = load(p.id(), ip);
    let mut out = Sync::default();
    let mut tv = match read {
        Ok(tv) => tv,
        // can't reach it: show what we know
        Err(e) => {
            out.problem = Some(e.to_string());
            out.favourites = store.seen.clone();
            for (id, (on, _)) in &store.want {
                apply(&mut out.favourites, *id, *on);
                out.pending.insert(*id, *on);
            }
            return Some(out);
        }
    };
    if store.seen_at > 0 {
        out.on_tv = store.seen.symmetric_difference(&tv).map(|id| (*id, tv.contains(id))).collect();
    }
    for (id, (on, at)) in store.want.clone() {
        match decide(&store, &tv, id, on, at) {
            Decide::Done => {
                store.want.remove(&id);
            }
            Decide::TvWins => {
                store.want.remove(&id);
                out.overruled.push((id, on));
            }
            Decide::Send => match chans.write_favourite(cx, id, on) {
                Ok(true) => {
                    store.want.remove(&id);
                    apply(&mut tv, id, on);
                    out.sent.push((id, on));
                }
                // not now (live TV isn't in front): next time
                Ok(false) => {}
                Err(e) => out.problem = Some(e.to_string()),
            },
        }
    }
    store.seen = tv.clone();
    store.seen_at = now();
    out.favourites = tv;
    for (id, (on, _)) in &store.want {
        apply(&mut out.favourites, *id, *on);
        out.pending.insert(*id, *on);
    }
    let _ = save(p.id(), ip, &store);
    Some(out)
}

fn apply(set: &mut BTreeSet<u32>, id: u32, on: bool) {
    if on {
        set.insert(id);
    } else {
        set.remove(&id);
    }
}

/// Make channel `id` a favourite or not: noted now, sent to the TV now if
/// it'll take it, else on a later sync.
pub fn set(p: &dyn Provider, cx: &Cx, id: u32, on: bool) -> Result<Sync> {
    let mut store = load(p.id(), &cx.tv.ip);
    store.want.insert(id, (on, now()));
    save(p.id(), &cx.tv.ip, &store)?;
    sync(p, cx).map_or_else(|| crate::error::failed(format!("{} has no favourites", p.name())), Ok)
}

/// Mark `list`'s favourites from a sync.
pub fn mark(list: &mut [Channel], s: &Sync) {
    for ch in list {
        ch.favourite = s.favourites.contains(&ch.id);
    }
}

/// The channel list with favourites marked, and the sync's news. Syncing
/// problems don't stop the list.
pub fn channels(p: &dyn Provider, cx: &Cx) -> Result<(Vec<Channel>, Option<Sync>)> {
    let Some(chans) = p.channels() else { return crate::error::failed(format!("{} has no channels", p.name())) };
    let mut list = chans.list(cx)?;
    let s = sync(p, cx);
    if let Some(s) = &s {
        mark(&mut list, s);
    }
    Ok((list, s))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set_of(ids: &[u32]) -> BTreeSet<u32> {
        ids.iter().copied().collect()
    }

    #[test]
    fn store_round_trips() {
        let s = Store { seen_at: 100, seen: set_of(&[143, 159]), want: BTreeMap::from([(158, (true, 120)), (143, (false, 130))]) };
        assert_eq!(Store::parse(&s.render("test")), s);
        assert_eq!(Store::parse(""), Store::default());
    }

    #[test]
    fn decides_which_way() {
        // last read at 100: 143 was a favourite
        let store = Store { seen_at: 100, seen: set_of(&[143]), want: BTreeMap::new() };
        // the TV already agrees
        assert_eq!(decide(&store, &set_of(&[143]), 143, true, 50), Decide::Done);
        // ours (made after the last read) and the TV unchanged: send it
        assert_eq!(decide(&store, &set_of(&[143]), 158, true, 150), Decide::Send);
        // someone removed 143 on the TV after the last read; our "add 143"
        // dates from before that read, so the TV's change is newer
        let tv = set_of(&[]);
        assert_eq!(decide(&store, &tv, 143, true, 90), Decide::TvWins);
        // ... but a change of ours made since the last read wins
        assert_eq!(decide(&store, &tv, 143, true, 150), Decide::Send);
        // never read before: nothing to compare, send
        assert_eq!(decide(&Store::default(), &tv, 143, true, 90), Decide::Send);
    }

    #[test]
    fn tells_what_happened() {
        let ch = |id, n: &str| Channel { id, number: Some(n.into()), name: None, radio: false, favourite: false, delivery: None, dvb: None };
        let s = Sync { on_tv: vec![(158, true)], sent: vec![(143, false)], ..Sync::default() };
        assert_eq!(s.news(&[ch(158, "2"), ch(143, "18")]).as_deref(), Some("changed on the TV: +2; sent to the TV: -18"));
        assert_eq!(Sync::default().news(&[]), None);
    }
}

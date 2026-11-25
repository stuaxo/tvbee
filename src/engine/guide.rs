//! The live TV guide's data: its channels and their programmes.

use super::*;

/// A channel in the guide pop-up, with its programmes for the next day.
#[derive(Debug, Clone, PartialEq)]
pub struct GuideRow {
    pub channel: crate::provider::Channel,
    /// Sorted by start.
    pub events: Vec<crate::provider::Event>,
}

impl GuideRow {
    /// What's on at time `t`, and what comes after it.
    pub fn at(&self, t: i64) -> (Option<&crate::provider::Event>, Option<&crate::provider::Event>) {
        let on = self.events.iter().find(|e| e.start <= t && t < e.end);
        let after = self.events.iter().find(|e| e.start >= on.map_or(t, |o| o.end));
        (on, after)
    }
}

/// How far ahead the guide goes (← → move through it in GUIDE_STEP steps).
pub(crate) const GUIDE_AHEAD: i64 = 24 * 3600;
pub(crate) const GUIDE_STEP: i64 = 30 * 60;

#[derive(Debug, Clone, PartialEq)]
pub struct Guide {
    pub provider: &'static str,
    /// "Freeview, United Kingdom (DVB-T, DVB-T2)", if the provider knows its network.
    pub place: Option<String>,
    /// How the network groups its channel numbers (for sub-headers).
    pub lineup: Option<&'static crate::lineups::Lineup>,
    pub rows: Vec<GuideRow>,
    /// The channel the TV is tuned to.
    pub current: Option<u32>,
    /// Favourite changes not on the TV yet.
    pub pending: std::collections::BTreeMap<u32, bool>,
    /// What syncing favourites found, for the status line.
    pub news: Option<String>,
}

/// The first provider with both channels and a guide.
pub(crate) fn live_provider() -> Option<&'static dyn crate::provider::Provider> {
    crate::provider::all().iter().copied().find(|p| p.channels().is_some() && p.guide().is_some())
}

pub(crate) fn load_guide(tv: &Tv) -> Result<Guide> {
    let Some(p) = live_provider() else { return Err(crate::error::Error::Failed("no live TV guide on this TV".into())) };
    let cx = crate::provider::Cx { tv };
    let (chans, guide) = (p.channels().expect("checked"), p.guide().expect("checked"));
    let (list, synced) = crate::favourites::channels(p, &cx)?;
    let now = crate::plugins::guide::now();
    let current = chans.current(&cx).ok().flatten().map(|c| c.id);
    let news = synced.as_ref().and_then(|s| s.news(&list));
    let pending = synced.map(|s| s.pending).unwrap_or_default();
    // (the guide is cached, so a schedule per channel is cheap)
    let rows = list
        .into_iter()
        .map(|channel| {
            let events = guide.schedule(&cx, channel.id, now, now + GUIDE_AHEAD + 3 * 3600).unwrap_or_default();
            GuideRow { channel, events }
        })
        .collect();
    let net = chans.network(&cx);
    let place = net.as_ref().map(crate::provider::describe);
    let lineup = net.as_ref().and_then(crate::lineups::find);
    Ok(Guide { provider: p.name(), place, lineup, rows, current, pending, news })
}


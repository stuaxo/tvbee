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


/// The time `later` steps ahead of `now`: now itself, then on the hour and
/// half hour (19:30, 20:00, ...).
pub fn guide_time_at(now: i64, later: i64) -> i64 {
    if later == 0 { now } else { (now.div_euclid(GUIDE_STEP) + later) * GUIDE_STEP }
}

/// A line of the guide's channel list: a group's sub-header, or channel row N.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuideItem<'a> {
    Header(&'a str),
    Row(usize),
}

/// The lines to show of a list whose rows are in `groups` (each row's group,
/// None if ungrouped): a header where the group changes, `room` lines around
/// row `sel`. Scrolled into a group, its header stays at the top.
pub fn guide_items<'a>(groups: &[Option<&'a str>], sel: usize, room: usize) -> Vec<GuideItem<'a>> {
    let mut all = Vec::new();
    let mut last = None;
    for (i, g) in groups.iter().enumerate() {
        if g.is_some() && *g != last {
            all.push(GuideItem::Header(g.unwrap_or_default()));
        }
        last = *g;
        all.push(GuideItem::Row(i));
    }
    let at = all.iter().position(|it| *it == GuideItem::Row(sel)).unwrap_or(0);
    let start = at.saturating_sub(room / 2).min(all.len().saturating_sub(room));
    let mut out: Vec<GuideItem> = all.iter().copied().skip(start).take(room).collect();
    // the top line mid-group: show the group instead (not over the selected
    // row, and only if the group carries on below it)
    if let (Some(GuideItem::Row(i)), Some(GuideItem::Row(j)), true) = (out.first().copied(), out.get(1).copied(), start > 0) {
        if let (Some(g), true) = (groups[i], i != sel && groups[j] == groups[i]) {
            out[0] = GuideItem::Header(g);
        }
    }
    out
}

/// The guide pop-up's tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuideTab {
    All,
    Favourites,
    Radio,
}

impl GuideTab {
    pub const ALL: [GuideTab; 3] = [GuideTab::All, GuideTab::Favourites, GuideTab::Radio];

    /// The tab after this one (Tab cycles through them).
    pub fn next(self) -> GuideTab {
        match self {
            GuideTab::All => GuideTab::Favourites,
            GuideTab::Favourites => GuideTab::Radio,
            GuideTab::Radio => GuideTab::All,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            GuideTab::All => "All channels",
            GuideTab::Favourites => "Favourites",
            GuideTab::Radio => "Radio",
        }
    }

    pub fn holds(self, ch: &crate::provider::Channel) -> bool {
        match self {
            GuideTab::All => true,
            GuideTab::Favourites => ch.favourite,
            GuideTab::Radio => ch.radio,
        }
    }
}

/// Whether a guide row matches the filter: every word is the start of the
/// channel number or part of its name or of the programmes shown (on at
/// `t` and after), in any case.
pub fn guide_matches(r: &GuideRow, filter: &str, t: i64) -> bool {
    let num = r.channel.number.as_deref().unwrap_or("");
    let (on, after) = r.at(t);
    let text = [r.channel.name.as_deref(), on.map(|e| e.title.as_str()), after.map(|e| e.title.as_str())]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase();
    filter.to_lowercase().split_whitespace().all(|w| num.starts_with(w) || text.contains(w))
}

/// The rows a tab shows with this filter.
pub fn guide_shown<'a>(rows: &'a [GuideRow], tab: GuideTab, filter: &str, t: i64) -> Vec<&'a GuideRow> {
    rows.iter().filter(|r| tab.holds(&r.channel) && guide_matches(r, filter, t)).collect()
}


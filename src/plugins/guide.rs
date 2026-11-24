//! `guide` and `providers`: programme guides from whichever provider has one
//! (over-the-air TV on Philips TVs), and what each provider can do.

use crate::error::{failed, usage, Result};
use crate::provider::{self, Cx, Event};
use crate::registry::Command;
use crate::Ctx;

const CAT: &str = "guide";

pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// This PC's UTC offset in seconds, from `date +%z` (0 where there's no
/// `date`). Asked once per run.
pub fn utc_offset() -> i64 {
    static OFFSET: std::sync::OnceLock<i64> = std::sync::OnceLock::new();
    *OFFSET.get_or_init(|| {
        let Ok(out) = std::process::Command::new("date").arg("+%z").output() else { return 0 };
        parse_offset(String::from_utf8_lossy(&out.stdout).trim())
    })
}

/// "+0100" -> 3600, "-0530" -> -19800.
pub fn parse_offset(z: &str) -> i64 {
    let (sign, digits) = match z.as_bytes().first() {
        Some(b'-') => (-1, &z[1..]),
        Some(b'+') => (1, &z[1..]),
        _ => (1, z),
    };
    let (h, m) = (digits.get(..2).and_then(|h| h.parse::<i64>().ok()), digits.get(2..4).and_then(|m| m.parse::<i64>().ok()));
    sign * (h.unwrap_or(0) * 3600 + m.unwrap_or(0) * 60)
}

/// Local "21:00" for a Unix time.
pub fn hhmm(t: i64, offset: i64) -> String {
    let s = (t + offset).rem_euclid(86_400);
    format!("{:02}:{:02}", s / 3600, s / 60 % 60)
}

/// Local "Sat 21:00" for a Unix time.
pub fn day_hhmm(t: i64, offset: i64) -> String {
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"]; // 1970-01-01 was a Thursday
    let day = (t + offset).div_euclid(86_400).rem_euclid(7) as usize;
    format!("{} {}", DAYS[day], hhmm(t, offset))
}

fn fit(s: &str, w: usize) -> String {
    if s.chars().count() <= w {
        format!("{s:<w$}")
    } else {
        format!("{}…", s.chars().take(w - 1).collect::<String>())
    }
}

/// `guide [<channel>] [--hours N] [--provider ID]`.
fn guide(c: &Ctx, args: &[String]) -> Result<()> {
    let (mut id, mut hours, mut words) = (None, 24i64, Vec::new());
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--provider" => id = it.next().cloned(),
            "--hours" => hours = it.next().and_then(|h| h.parse().ok()).filter(|h| *h > 0).map_or_else(|| usage("guide: --hours needs a number"), Ok)?,
            x if x.starts_with("--") => return usage(format!("guide: unknown option '{x}' (usage: tveee guide [<channel>] [--hours N] [--provider ID])")),
            x => words.push(x.to_string()),
        }
    }
    let Some(p) = provider::with_guide(id.as_deref()) else {
        return failed("guide: no provider with a programme guide here (see 'tveee providers')");
    };
    let g = p.guide().expect("with_guide checked");
    let cx = Cx { tv: &c.tv };
    let (at, off) = (now(), utc_offset());
    // the channel list gives numbers and names; without it, ids only
    let channels = p.channels().map(|ch| ch.list(&cx));
    if words.is_empty() {
        let mut rows = g.now_next(&cx, at)?;
        if rows.is_empty() {
            return failed(format!("guide: the {} guide is empty (the TV collects it while tuned to live TV)", p.name()));
        }
        let list = match channels {
            Some(Ok(list)) => list,
            Some(Err(e)) => {
                eprintln!("(no channel names or numbers: {e})");
                Vec::new()
            }
            None => Vec::new(),
        };
        let find = |id| list.iter().find(|ch| ch.id == id);
        rows.sort_by_key(|r| find(r.channel).map_or((u32::MAX, r.channel), provider::Channel::order));
        println!("{:<24}  {:<44}  next", "channel", format!("now ({})", p.name()));
        for r in rows {
            let label = find(r.channel).map_or_else(|| format!("#{}", r.channel), provider::Channel::label);
            let now = r.now.as_ref().map_or_else(|| "-".to_string(), |e: &Event| format!("{} {}", hhmm(e.start, off), e.title));
            let next = r.next.as_ref().map_or_else(String::new, |e| format!("{} {}", hhmm(e.start, off), e.title));
            println!("{}  {}  {}", fit(&label, 24), fit(&now, 44), fit(&next, 40));
        }
        return Ok(());
    }
    // one channel's schedule: needs the channel list to find it
    let query = words.join(" ");
    let list = channels.unwrap_or_else(|| failed(format!("guide: {} has no channel list to find '{query}' in", p.name())))?;
    let Some(ch) = provider::find_channel(&list, &query) else {
        return failed(format!("guide: no channel '{query}' (see 'tveee channels')"));
    };
    let events = g.schedule(&cx, ch.id, at, at + hours * 3600)?;
    if events.is_empty() {
        return failed(format!("guide: nothing in the guide for {} in the next {hours}h", ch.label()));
    }
    println!("{}:", ch.label());
    for e in events {
        let on = if e.start <= at { "  <- on now" } else { "" };
        println!("  {}-{}  {}{on}", day_hhmm(e.start, off), hhmm(e.end, off), e.title);
    }
    Ok(())
}

/// The provider with channels: `--provider ID`, else the first one.
fn channel_provider(id: Option<&str>) -> Result<&'static dyn provider::Provider> {
    provider::all()
        .iter()
        .copied()
        .find(|p| p.channels().is_some() && id.is_none_or(|i| p.id() == i))
        .map_or_else(|| failed("no provider with channels here (see 'tveee providers')"), Ok)
}

/// Split off a trailing `--provider ID`.
fn provider_flag(args: &[String]) -> (Vec<String>, Option<String>) {
    match args.iter().position(|a| a == "--provider") {
        Some(i) => (args[..i].iter().chain(args.get(i + 2..).unwrap_or(&[])).cloned().collect(), args.get(i + 1).cloned()),
        None => (args.to_vec(), None),
    }
}

/// `channels [--tv | --radio] [--provider ID]`.
fn channels(c: &Ctx, args: &[String]) -> Result<()> {
    let (rest, id) = provider_flag(args);
    let kind = match rest.as_slice() {
        [] => None,
        [k] if k == "--tv" => Some(false),
        [k] if k == "--radio" => Some(true),
        _ => return usage("usage: tveee channels [--tv | --radio] [--provider ID]"),
    };
    let p = channel_provider(id.as_deref())?;
    let cx = Cx { tv: &c.tv };
    let chans = p.channels().expect("checked");
    let (list, _) = crate::favourites::channels(p, &cx)?;
    let current = chans.current(&cx).ok().flatten().map(|ch| ch.id);
    // what's on each, if there's a guide too
    let (at, off) = (now(), utc_offset());
    let on: Vec<provider::NowNext> = p.guide().and_then(|g| g.now_next(&cx, at).ok()).unwrap_or_default();
    // grouped the way the country's lineup numbers them (Entertainment, HD, ...)
    let net = chans.network(&cx);
    let lineup = net.as_ref().and_then(crate::lineups::find);
    if let Some(net) = &net {
        println!("{}", provider::describe(net));
    }
    let mut group = None;
    for ch in list.iter().filter(|ch| kind.is_none_or(|r| ch.radio == r)) {
        let g = lineup.map(|l| l.group(ch.order().0));
        if g.is_some() && g != group {
            println!("\n{}:", g.unwrap_or_default());
            group = g;
        }
        let title = on.iter().find(|n| n.channel == ch.id).and_then(|n| n.now.as_ref()).map_or_else(String::new, |e| format!("{} {}", hhmm(e.start, off), e.title));
        let radio = if ch.radio { " (radio)" } else { "" };
        let mark = if current == Some(ch.id) { "  <- on now" } else { "" };
        let star = if ch.favourite { "★" } else { " " };
        println!("{star} {}  {}{mark}", fit(&format!("{}{radio}", ch.label()), 30), title);
    }
    Ok(())
}

/// `watch <number|name> [--provider ID]`.
fn watch(c: &Ctx, args: &[String]) -> Result<()> {
    let (words, id) = provider_flag(args);
    if words.is_empty() {
        return usage("usage: tveee watch <channel number or name> [--provider ID]");
    }
    let query = words.join(" ");
    let p = channel_provider(id.as_deref())?;
    let cx = Cx { tv: &c.tv };
    let chans = p.channels().expect("checked");
    let list = chans.list(&cx)?;
    let Some(ch) = provider::find_channel(&list, &query) else {
        return failed(format!("watch: no channel '{query}' (see 'tveee channels')"));
    };
    println!("{}", chans.tune(&cx, ch)?);
    Ok(())
}

/// `channel now`: what live TV is tuned to, and what's on it.
fn on_now(c: &Ctx, args: &[String]) -> Result<()> {
    let (words, id) = provider_flag(args);
    let info = match words.as_slice() {
        [] => false,
        [w] if w == "--info" => true,
        _ => return usage("usage: tveee onnow [--info] [--provider ID]"),
    };
    let p = channel_provider(id.as_deref())?;
    let cx = Cx { tv: &c.tv };
    let Some(ch) = p.channels().expect("checked").current(&cx)? else {
        return failed(format!("{} can't tell which channel is on", p.name()));
    };
    let (at, off) = (now(), utc_offset());
    let events = p.guide().map(|g| g.schedule(&cx, ch.id, at, at + 6 * 3600)).transpose()?.unwrap_or_default();
    println!("{}", ch.label());
    for (e, when) in events.iter().zip(["now ", "next"]) {
        let genre = e.genre_name().map_or(String::new(), |g| format!("  ({g})"));
        println!("  {when}  {}-{}  {}{genre}", hhmm(e.start, off), hhmm(e.end, off), e.title);
        if !info {
            continue;
        }
        // the first time, the TV takes a while to hand over the descriptions
        let about = p.guide().map(|g| g.info(&cx, ch.id, e.start)).transpose()?.flatten();
        for line in about.as_deref().unwrap_or("(no description in the broadcast guide)").lines() {
            println!("               {line}");
        }
    }
    Ok(())
}

/// `fav [add|remove <channel>] [--provider ID]`.
fn fav(c: &Ctx, args: &[String]) -> Result<()> {
    let (words, id) = provider_flag(args);
    let p = channel_provider(id.as_deref())?;
    let cx = Cx { tv: &c.tv };
    let (list, synced) = crate::favourites::channels(p, &cx)?;
    let Some(synced) = synced else { return failed(format!("{} has no favourites", p.name())) };
    let (on, query) = match words.split_first() {
        None => {
            if let Some(news) = synced.news(&list) {
                println!("({news})");
            }
            if let Some(why) = &synced.problem {
                println!("(couldn't sync with the TV: {why})");
            }
            let favs: Vec<_> = list.iter().filter(|ch| ch.favourite).collect();
            if favs.is_empty() {
                println!("no favourites yet ('tveee fav add <channel>')");
            }
            for ch in favs {
                let wait = if synced.pending.contains_key(&ch.id) { "  (not on the TV yet)" } else { "" };
                println!("★ {}{wait}", ch.label());
            }
            for (gone, _) in synced.pending.iter().filter(|(_, on)| !**on) {
                if let Some(ch) = list.iter().find(|c| c.id == *gone) {
                    println!("  {}  (being removed: still on the TV)", ch.label());
                }
            }
            return Ok(());
        }
        Some((cmd, rest)) if !rest.is_empty() && (cmd == "add" || cmd == "remove") => (cmd == "add", rest.join(" ")),
        _ => return usage("usage: tveee fav [add|remove <channel>] [--provider ID]"),
    };
    let Some(ch) = provider::find_channel(&list, &query) else {
        return failed(format!("fav: no channel '{query}' (see 'tveee channels')"));
    };
    let s = crate::favourites::set(p, &cx, ch.id, on)?;
    println!("{}", fav_message(ch, on, &s));
    if let Some(news) = s.news(&list).filter(|_| s.sent.iter().any(|(id, _)| *id != ch.id) || !s.on_tv.is_empty()) {
        println!("({news})");
    }
    Ok(())
}

/// What happened to a favourite change, for the status line.
pub fn fav_message(ch: &provider::Channel, on: bool, s: &crate::favourites::Sync) -> String {
    let what = if on { "added" } else { "removed" };
    match (s.pending.contains_key(&ch.id), &s.problem) {
        (false, _) => format!("{what} {}", ch.label()),
        (true, Some(why)) => format!("saved: {} will be {what} on the TV later (now: {why})", ch.label()),
        // Philips TVs only take changes with live TV in front
        (true, None) => format!("saved: {} will be {what} on the TV next time live TV is in front", ch.label()),
    }
}

fn providers(c: &Ctx, _: &[String]) -> Result<()> {
    let all = provider::all();
    if all.is_empty() {
        println!("no providers enabled (see 'tveee plugins')");
    }
    for p in all {
        println!("{:<10} {:<24} {:<28} {}", p.id(), p.name(), p.hosts().join(","), provider::capabilities(*p).join(", "));
        // where its channels come from, if it can say (Philips: needs pairing)
        if let Some(net) = p.channels().and_then(|ch| ch.network(&Cx { tv: &c.tv })) {
            println!("{:<10} {}", "", provider::describe(&net));
        }
    }
    Ok(())
}

pub fn commands() -> Vec<Command> {
    vec![
        Command::new(CAT, "guide", "[<channel>] [--hours N] [--provider ID]", "programme guide: now/next, or one channel's schedule", guide).long(
            "From the first provider with a guide (live TV over the air on Philips TVs,\n\
             from the broadcast EPG the TV collects).\n\
             \x20 tveee guide                now and next on every channel\n\
             \x20 tveee guide 4              channel 4's programmes for the next 24 hours\n\
             \x20 tveee guide bbc two --hours 6  ... by name, for the next 6\n\
             Channel numbers and names come from the TV's channel list, which on\n\
             Philips TVs needs pairing ('tveee pair'); without it the guide shows the\n\
             TV's internal channel ids.",
        ),
        Command::new(CAT, "channels", "[--tv | --radio] [--provider ID]", "channels you can watch, with what's on", channels)
            .long("The live TV channel list with what's on each now, marking the one the TV is\ntuned to. On Philips TVs it needs pairing ('tveee pair')."),
        Command::new(CAT, "watch", "<channel> [--provider ID]", "switch live TV to a channel, by number or name", watch)
            .long("  tveee watch 4\n  tveee watch more 4\n  tveee watch bbc two\nNames match from the start, then anywhere; the lowest channel number wins."),
        Command::new(CAT, "onnow", "[--info] [--provider ID]", "the channel live TV is on, and what's on it now and next", on_now)
            .long("  --info   with each programme's description, from the broadcast guide (the first\n           time takes the TV about half a minute; then it's cached)"),
        Command::new(CAT, "fav", "[add|remove <channel>] [--provider ID]", "favourite channels: list, add, remove", fav).long(
            "  tveee fav                  list them\n\
             \x20 tveee fav add more 4       add a channel (by number or name)\n\
             \x20 tveee fav remove 18        remove one\n\
             Changes are kept by tveee and synced with the TV: they go to it as soon\n\
             as it takes them (Philips TVs: only with live TV in front), and changes\n\
             made on the TV itself are noticed. If both sides changed a channel, the\n\
             later change wins. On Philips TVs new favourites go in Favourites 1, and\n\
             removing takes a channel out of every list.",
        ),
        Command::new(CAT, "providers", "", "content providers and what each can do", providers).long(
            "Providers are the sources of content tveee knows (live TV, YouTube, ...),\n\
             registered by plugins, with their capabilities: guide, channels, details,\n\
             places, search, now-playing.",
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times() {
        assert_eq!(parse_offset("+0100"), 3600);
        assert_eq!(parse_offset("-0530"), -19_800);
        assert_eq!(parse_offset(""), 0);
        // day 20000 after 1970-01-01 (a Thursday) starts at midnight UTC on a Friday
        let midnight = 20_000 * 86_400;
        assert_eq!(hhmm(midnight, 0), "00:00");
        assert_eq!(hhmm(midnight, 3600), "01:00");
        assert_eq!(day_hhmm(midnight, 0), "Fri 00:00");
        assert_eq!(day_hhmm(midnight, -60), "Thu 23:59");
        assert_eq!(fit("abcdef", 4), "abc…");
        assert_eq!(fit("ab", 4), "ab  ");
    }
}

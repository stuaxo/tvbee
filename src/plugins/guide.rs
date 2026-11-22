//! `guide` and `providers`: programme guides from whichever provider has one
//! (over-the-air TV on Philips TVs), and what each provider can do.

use crate::error::{failed, usage, Result};
use crate::provider::{self, Cx, Event};
use crate::registry::Command;
use crate::Ctx;

const CAT: &str = "guide";

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// This PC's UTC offset in seconds, from `date +%z` (0 where there's no `date`).
fn utc_offset() -> i64 {
    let Ok(out) = std::process::Command::new("date").arg("+%z").output() else { return 0 };
    parse_offset(String::from_utf8_lossy(&out.stdout).trim())
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

/// `guide [--provider ID] [<channel> [--hours N]]`.
fn guide(c: &Ctx, args: &[String]) -> Result<()> {
    let (mut id, mut hours, mut channel) = (None, 24i64, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--provider" => id = it.next().cloned(),
            "--hours" => hours = it.next().and_then(|h| h.parse().ok()).filter(|h| *h > 0).map_or_else(|| usage("guide: --hours needs a number"), Ok)?,
            n if n.parse::<u32>().is_ok() => channel = n.parse().ok(),
            x => return usage(format!("guide: unexpected '{x}' (usage: tvbee guide [<channel>] [--hours N] [--provider ID])")),
        }
    }
    let Some(p) = provider::with_guide(id.as_deref()) else {
        return failed("guide: no provider with a programme guide here (see 'tvbee providers')");
    };
    let g = p.guide().expect("with_guide checked");
    let cx = Cx { tv: &c.tv };
    let (at, off) = (now(), utc_offset());
    match channel {
        // one channel's schedule
        Some(preset) => {
            let events = g.schedule(&cx, preset, at, at + hours * 3600)?;
            if events.is_empty() {
                return failed(format!(
                    "guide: nothing in the {} guide for preset {preset} in the next {hours}h (presets are the TV's internal numbers; 'tvbee guide' lists them)",
                    p.name()
                ));
            }
            println!("preset {preset} ({}):", p.name());
            for e in events {
                let on = if e.start <= at { "  <- on now" } else { "" };
                println!("  {}-{}  {}{on}", day_hhmm(e.start, off), hhmm(e.end, off), e.title);
            }
        }
        // now and next on every channel
        None => {
            let rows = g.now_next(&cx, at)?;
            if rows.is_empty() {
                return failed(format!("guide: the {} guide is empty (the TV collects it while tuned to live TV)", p.name()));
            }
            // presets are the TV's internal service numbers, not channel numbers
            println!("{:>6}  {:<44}  next", "preset", format!("now ({})", p.name()));
            for r in rows {
                let now = r.now.as_ref().map_or_else(|| "-".to_string(), |e: &Event| format!("{} {}", hhmm(e.start, off), e.title));
                let next = r.next.as_ref().map_or_else(String::new, |e| format!("{} {}", hhmm(e.start, off), e.title));
                println!("{:>6}  {}  {}", r.preset, fit(&now, 44), fit(&next, 40));
            }
        }
    }
    Ok(())
}

/// The provider with channels: `--provider ID`, else the first one.
fn channel_provider(id: Option<&str>) -> Result<&'static dyn provider::Provider> {
    provider::all()
        .iter()
        .copied()
        .find(|p| p.channels().is_some() && id.is_none_or(|i| p.id() == i))
        .map_or_else(|| failed("no provider with channels here (see 'tvbee providers')"), Ok)
}

fn channels(c: &Ctx, args: &[String]) -> Result<()> {
    let id = match args {
        [] => None,
        [flag, id] if flag == "--provider" => Some(id.as_str()),
        _ => return usage("usage: tvbee channels [--provider ID]"),
    };
    let p = channel_provider(id)?;
    let cx = Cx { tv: &c.tv };
    let list = p.channels().expect("checked").list(&cx)?;
    let current = p.channels().expect("checked").current(&cx).ok().flatten().map(|ch| ch.preset);
    // what's on each, if there's a guide too
    let (at, off) = (now(), utc_offset());
    let on: Vec<provider::NowNext> = p.guide().map(|g| g.now_next(&cx, at)).transpose()?.unwrap_or_default();
    for ch in list {
        let title = on.iter().find(|n| n.preset == ch.preset).and_then(|n| n.now.as_ref()).map_or_else(String::new, |e| format!("{} {}", hhmm(e.start, off), e.title));
        let mark = if current == Some(ch.preset) { "  <- watching" } else { "" };
        println!("{:>4}  {}{mark}", ch.preset, ch.name.as_deref().map_or(title.clone(), |n| format!("{n}  {title}")));
    }
    Ok(())
}

fn watch(c: &Ctx, args: &[String]) -> Result<()> {
    let (preset, id) = match args {
        [n] => (n, None),
        [n, flag, id] if flag == "--provider" => (n, Some(id.as_str())),
        _ => return usage("usage: tvbee watch <channel> [--provider ID]"),
    };
    let Ok(preset) = preset.parse::<u32>() else { return usage(format!("watch: '{preset}' isn't a channel number")) };
    let p = channel_provider(id)?;
    println!("{}", p.channels().expect("checked").tune(&Cx { tv: &c.tv }, preset)?);
    Ok(())
}

fn providers(_: &Ctx, _: &[String]) -> Result<()> {
    let all = provider::all();
    if all.is_empty() {
        println!("no providers enabled (see 'tvbee plugins')");
    }
    for p in all {
        println!("{:<10} {:<24} {:<28} {}", p.id(), p.name(), p.hosts().join(","), provider::capabilities(*p).join(", "));
    }
    Ok(())
}

pub fn commands() -> Vec<Command> {
    vec![
        Command::new(CAT, "guide", "[<preset>] [--hours N] [--provider ID]", "programme guide: now/next, or one channel's schedule", guide).long(
            "From the first provider with a guide (live TV over the air on Philips TVs,\n\
             from the broadcast EPG the TV collects).\n\
             \x20 tvbee guide              now and next on every channel\n\
             \x20 tvbee guide 101          preset 101's programmes for the next 24 hours\n\
             \x20 tvbee guide 101 --hours 6  ... for the next 6\n\
             Presets are the TV's internal service numbers (as listed by 'tvbee guide'),\n\
             not the channel numbers typed on the remote; mapping them needs the TV's\n\
             channel list (JointSpace pairing).",
        ),
        Command::new(CAT, "channels", "[--provider ID]", "channels you can watch, with what's on", channels)
            .long("The channels of the first provider that has them (live TV over the air on\nPhilips TVs: those with something in the guide), with the programme on now."),
        Command::new(CAT, "watch", "<preset> [--provider ID]", "switch live TV to a channel (not yet for over-the-air TV)", watch)
            .long("Switches to a channel from the guide. Over-the-air TV can't do this yet: the\nguide's presets aren't channel numbers. 'tvbee channel <number>' types a channel\nnumber on the remote instead."),
        Command::new(CAT, "providers", "", "content providers and what each can do", providers).long(
            "Providers are the sources of content tvbee knows (live TV, YouTube, ...),\n\
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

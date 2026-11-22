//! Lineups: how a country's broadcast TV numbers its channels, so lists can
//! be grouped (Entertainment, HD, Children's, News, Radio, ...). Each lineup
//! is a small data file in `src/lineups/`, compiled in; a provider's
//! [`Network`] (country, DVB network) picks the one that applies.

use crate::provider::Network;
use std::ops::RangeInclusive;

/// The lineup files, parsed on first use.
const FILES: &[(&str, &str)] = &[("gb-freeview.txt", include_str!("lineups/gb-freeview.txt"))];

#[derive(Debug, Clone, PartialEq)]
pub struct Lineup {
    pub name: String,
    pub country: Option<String>,
    /// The DVB original network id it's for.
    pub onid: Option<u16>,
    pub groups: Vec<(RangeInclusive<u32>, String)>,
}

impl Lineup {
    /// The group channel `number` is in ("Other" if none).
    pub fn group(&self, number: u32) -> &str {
        self.groups.iter().find(|(r, _)| r.contains(&number)).map_or("Other", |(_, g)| g.as_str())
    }

    /// "Freeview, United Kingdom".
    pub fn title(&self) -> String {
        match &self.country {
            Some(c) => format!("{}, {c}", self.name),
            None => self.name.clone(),
        }
    }
}

/// Parse a lineup file; errors name the line.
pub fn parse(text: &str) -> Result<Lineup, String> {
    let mut l = Lineup { name: String::new(), country: None, onid: None, groups: Vec::new() };
    for (n, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let bad = |why: &str| format!("line {}: {why}: {raw}", n + 1);
        let (key, value) = line.split_once(char::is_whitespace).map(|(k, v)| (k, v.trim())).ok_or_else(|| bad("expected '<key> <value>'"))?;
        match key {
            "name" => l.name = value.to_string(),
            "country" => l.country = Some(value.to_string()),
            "onid" => l.onid = Some(value.parse().map_err(|_| bad("onid must be a number"))?),
            range => {
                let (from, to) = range.split_once('-').unwrap_or((range, range));
                let (Ok(from), Ok(to)) = (from.parse::<u32>(), to.parse::<u32>()) else { return Err(bad("expected a channel number or range")) };
                if from > to {
                    return Err(bad("range runs backwards"));
                }
                if l.groups.iter().any(|(r, _)| r.contains(&from) || r.contains(&to)) {
                    return Err(bad("overlaps an earlier range"));
                }
                l.groups.push((from..=to, value.to_string()));
            }
        }
    }
    if l.name.is_empty() {
        return Err("no 'name' line".into());
    }
    Ok(l)
}

fn all() -> &'static [Lineup] {
    static ALL: std::sync::OnceLock<Vec<Lineup>> = std::sync::OnceLock::new();
    ALL.get_or_init(|| FILES.iter().map(|(name, text)| parse(text).unwrap_or_else(|e| panic!("src/lineups/{name}: {e}"))).collect())
}

/// The lineup for `network`: by its DVB network id, else by country.
pub fn find(network: &Network) -> Option<&'static Lineup> {
    let by_onid = network.onid.and_then(|o| all().iter().find(|l| l.onid == Some(o)));
    by_onid.or_else(|| network.country.as_ref().and_then(|c| all().iter().find(|l| l.country.as_ref() == Some(c) && l.onid.is_none())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freeview_groups() {
        let net = Network { country: Some("United Kingdom".into()), onid: Some(9018), deliveries: Vec::new() };
        let f = find(&net).expect("freeview");
        assert_eq!(f.title(), "Freeview, United Kingdom");
        for (n, g) in [(1, "Entertainment"), (100, "Entertainment"), (101, "HD"), (203, "Children's"), (231, "News"), (250, "Text"), (601, "Interactive"), (722, "Radio"), (348, "Other")] {
            assert_eq!(f.group(n), g, "channel {n}");
        }
        // another network: no lineup
        assert!(find(&Network { country: Some("Germany".into()), onid: Some(8468), deliveries: Vec::new() }).is_none());
    }

    #[test]
    fn checks_lineup_files() {
        assert!(parse("name X\n1-10 A\n5-20 B").unwrap_err().contains("overlaps"));
        assert!(parse("name X\n10-1 A").unwrap_err().contains("backwards"));
        assert!(parse("name X\nseven A").unwrap_err().contains("channel number"));
        assert!(parse("1-10 A").unwrap_err().contains("name"));
        let l = parse("# c\nname X  # trailing\n7 Seven\n").unwrap();
        assert_eq!((l.name.as_str(), l.group(7), l.group(8)), ("X", "Seven", "Other"));
        // every built-in file parses
        assert!(!all().is_empty());
    }
}

//! "More" about an app: details of what's on (description, chapters, ...),
//! fixed places in the app (history, subscriptions, ...), and links from
//! what's on (the channel, chapters, links in the description).
//!
//! These are shapes the core shows (`tveee details`, `tveee goto`) and
//! plugins fill in through their apps' hooks ([`crate::plugins::apps::App`]).

/// How to get somewhere.
#[derive(Debug, Clone, PartialEq)]
pub enum Go {
    /// Open this link in the app (a VIEW intent aimed at its package).
    Link(String),
    /// Seek the playing video to this many seconds.
    Seek(f64),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    pub label: String,
    pub go: Go,
}

/// A fixed destination in an app, reached by a link.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Place {
    /// What to type: `tveee goto <id>`.
    pub id: &'static str,
    pub label: &'static str,
    pub link: &'static str,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Chapter {
    pub start: f64,
    pub title: String,
}

/// What an app knows about what's on.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Details {
    /// e.g. ("channel", "RelfWolf (@RelfWolf)"), ("views", "1.05M").
    pub fields: Vec<(&'static str, String)>,
    pub description: Option<String>,
    pub chapters: Vec<Chapter>,
    /// Places to go from here: the channel, chapters, links in the description.
    pub links: Vec<Link>,
}

/// Links in a description, labelled by the text before them on the line:
/// "• Claude Code: https://..." -> ("Claude Code", the link).
pub fn description_links(text: &str) -> Vec<Link> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut rest = line;
        while let Some(i) = rest.find("http://").or_else(|| rest.find("https://")) {
            let url: String = rest[i..].chars().take_while(|c| !c.is_whitespace() && !matches!(c, ')' | ']' | '>' | '"')).collect();
            let before = rest[..i].trim().trim_start_matches(['•', '-', '*', ' ']).trim().trim_end_matches([':', '-', '–']).trim();
            let label = if before.is_empty() { url.clone() } else { before.to_string() };
            out.push(Link { label, go: Go::Link(url.trim_end_matches(['.', ',']).to_string()) });
            rest = &rest[i + url.len().max(1)..];
        }
    }
    out
}

/// 1051296 -> "1.05M", 13384 -> "13.4K".
pub fn count(n: f64) -> String {
    match n {
        n if n >= 1e9 => format!("{:.2}B", n / 1e9),
        n if n >= 1e6 => format!("{:.2}M", n / 1e6),
        n if n >= 1e3 => format!("{:.1}K", n / 1e3),
        n => format!("{n}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_description_links() {
        let d = "WHAT I USED :\n• Claude Code: https://code.claude.com/docs/en/setup\nSee https://a.example/x, and (https://b.example/y).\nnothing here";
        let l = description_links(d);
        assert_eq!(l.len(), 3);
        assert_eq!(l[0], Link { label: "Claude Code".into(), go: Go::Link("https://code.claude.com/docs/en/setup".into()) });
        assert_eq!(l[1].go, Go::Link("https://a.example/x".into()));
        assert_eq!(l[1].label, "See");
        assert_eq!(l[2].go, Go::Link("https://b.example/y".into()));
    }

    #[test]
    fn counts() {
        assert_eq!(count(1051296.0), "1.05M");
        assert_eq!(count(13384.0), "13.4K");
        assert_eq!(count(999.0), "999");
    }
}

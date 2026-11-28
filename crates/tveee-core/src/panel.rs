//! Panels: a plugin's live view of something on the TV, which front ends
//! show and change without knowing what it is (the Ambilight is one). A
//! panel says what it shows now ([`View`]): a few lines, colour swatches,
//! and rows of options, each running an action (`crate::actions`) when
//! picked. Plugins register them with `Plugin::with_panels`.
//!
//! Plain data, so plugins describe them whether or not a front end is built.


use crate::tv::Tv;

/// A colour: red, green, blue.
pub type Rgb = (u8, u8, u8);

/// A panel a plugin offers.
#[derive(Clone, Copy)]
pub struct Spec {
    pub id: &'static str,
    /// Its heading ("ambilight").
    pub title: &'static str,
    /// Whether this TV has it (asked once, in the background).
    pub available: fn(&Tv) -> bool,
    /// What it shows now (read every refresh while it's open).
    pub read: fn(&Tv) -> View,
}

/// What a panel shows.
#[derive(Debug, Clone, PartialEq)]
pub enum View {
    /// Not here after all: why.
    Missing(String),
    /// It needs this pairing first (`crate::pairing`): why.
    NeedsPairing { pairing: &'static str, why: String },
    Failed(String),
    Shown(Shown),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Shown {
    /// (what, value) lines: "power", "on".
    pub info: Vec<(String, String)>,
    /// Rows of colour swatches, each with a label ("left", its lights).
    pub swatches: Vec<(String, Vec<Rgb>)>,
    /// What can be changed, as rows of options.
    pub rows: Vec<Row>,
}

/// A row of options; `current` is the one in effect now.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub name: &'static str,
    pub options: Vec<Choice>,
    pub current: Option<usize>,
    /// What it does, when the name doesn't say ("sync Hue bulbs with the TV").
    pub help: Option<&'static str>,
}

/// An option: its label, a colour it shows (a light's), and what picking it does.
#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    pub label: String,
    pub colour: Option<Rgb>,
    pub run: Run,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Run {
    /// Run action `id` with `args`.
    Act { id: &'static str, args: Vec<String> },
    /// Ask for a line of text (`prompt`, `hint`), then run action `id` with
    /// `args` and the text after them.
    Ask { prompt: String, hint: &'static str, id: &'static str, args: Vec<String> },
}

impl Choice {
    pub fn act(label: impl Into<String>, id: &'static str, args: &[&str]) -> Choice {
        Choice { label: label.into(), colour: None, run: Run::Act { id, args: args.iter().map(|a| a.to_string()).collect() } }
    }
}

static REGISTRY: std::sync::OnceLock<Vec<Spec>> = std::sync::OnceLock::new();

/// Set the enabled plugins' panels (main does).
pub fn install(panels: Vec<Spec>) {
    let _ = REGISTRY.set(panels);
}

pub fn all() -> &'static [Spec] {
    REGISTRY.get_or_init(Vec::new)
}

pub fn by_id(id: &str) -> Option<&'static Spec> {
    all().iter().find(|p| p.id == id)
}

/// A colour: `#ff8800`, `ff8800`, `255,136,0`, or a name.
pub fn parse_colour(s: &str) -> Option<Rgb> {
    let s = s.trim().to_lowercase();
    let named = match s.as_str() {
        "red" => Some((255, 0, 0)),
        "green" => Some((0, 255, 0)),
        "blue" => Some((0, 0, 255)),
        "white" => Some((255, 255, 255)),
        "warm" => Some((255, 147, 41)),
        "yellow" => Some((255, 200, 0)),
        "orange" => Some((255, 100, 0)),
        "cyan" => Some((0, 255, 255)),
        "magenta" | "pink" => Some((255, 0, 160)),
        "purple" => Some((128, 0, 255)),
        "black" | "off" => Some((0, 0, 0)),
        _ => None,
    };
    if named.is_some() {
        return named;
    }
    let hex = s.trim_start_matches('#');
    if hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        return Some((byte(0)?, byte(2)?, byte(4)?));
    }
    let parts: Vec<u8> = s.split([',', ' ']).filter(|p| !p.is_empty()).filter_map(|p| p.parse().ok()).collect();
    match parts.as_slice() {
        [r, g, b] => Some((*r, *g, *b)),
        _ => None,
    }
}

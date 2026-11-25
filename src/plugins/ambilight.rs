//! Ambilight on Philips TVs, through the JointSpace API (so it needs
//! pairing: `tveee pair jointspace`). Part of the `philips` plugin.
//!
//! The TV drives the lights itself in one of its styles (following the
//! video, the sound, a colour; lounge), or hands them to something outside
//! it (`mode manual` / `expert`: tveee, or a program such as Hyperion),
//! which then sets the colours.

use crate::error::{failed, usage, Result};
use crate::plugins::philips::{js_get_paired, js_post};
use crate::actions::ActionDescriptor;
use crate::tv::Tv;
use crate::Ctx;

const CAT: &str = "philips";
const WHAT: &str = "ambilight";

fn get(tv: &Tv, path: &str) -> Result<String> {
    match js_get_paired(tv, path, WHAT)? {
        (200, body) => Ok(body),
        (code, _) => failed(format!("ambilight: the TV answered HTTP {code} for {path}")),
    }
}

fn post(tv: &Tv, path: &str, body: &str) -> Result<()> {
    match js_post(tv, path, body, WHAT)? {
        (200, _) => Ok(()),
        (code, said) => failed(format!("ambilight: the TV answered HTTP {code} for {path}{}", if said.trim().is_empty() { String::new() } else { format!(": {}", said.trim()) })),
    }
}

/// The video styles' settings, as tveee names them and as the TV does.
pub const VIDEO: &[(&str, &str)] =
    &[("standard", "STANDARD"), ("natural", "NATURAL"), ("vivid", "VIVID"), ("game", "GAME"), ("comfort", "COMFORT"), ("relax", "RELAX")];

/// Effects that follow the sound.
pub const AUDIO: &[(&str, &str)] = &[
    ("brightness", "ENERGY_ADAPTIVE_BRIGHTNESS"),
    ("colours", "ENERGY_ADAPTIVE_COLORS"),
    ("vu-meter", "VU_METER"),
    ("spectrum", "SPECTRUM_ANALYZER"),
    ("knight-rider", "KNIGHT_RIDER_CLOCKWISE"),
    ("knight-rider-alternating", "KNIGHT_RIDER_ALTERNATING"),
    ("flash", "RANDOM_PIXEL_FLASH"),
    ("strobe", "STROBO"),
    ("party", "PARTY"),
    ("random", "MODE_RANDOM"),
];

/// The TV's own colours: (tveee's name, the TV's setting, the TV's label).
pub const COLOURS: &[(&str, &str, &str)] = &[
    ("hot-lava", "HOT_LAVA", "Hot Lava"),
    ("warm-white", "ISF", "Warm White"),
    ("cool-white", "PTA_LOUNGE", "Cool White"),
    ("fresh-nature", "FRESH_NATURE", "Fresh Nature"),
    ("deep-water", "DEEP_WATER", "Deep Water"),
];

/// The modes `mode` can set. The TV reports others too, for its own styles
/// (lounge for its colours, expert for following the sound); only manual
/// means something outside the TV is setting the colours.
pub const MODES: &[(&str, &str)] = &[
    ("internal", "the TV drives the lights, in its current style"),
    ("manual", "something outside the TV (tveee's 'set', or another program) sets the colours"),
    ("expert", "outside control with the TV's expert settings (the TV also uses it for following the sound)"),
];

fn lookup<'a>(table: &'a [(&'a str, &'a str)], name: &str) -> Option<&'a str> {
    table.iter().find(|(n, tv)| *n == name || tv.eq_ignore_ascii_case(name)).map(|(_, tv)| *tv)
}

fn names(table: &[(&str, &str)]) -> String {
    table.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(", ")
}

/// The body that sets a style ("FOLLOW_VIDEO" with "STANDARD", ...).
pub fn style_body(style: &str, setting: Option<&str>, label: Option<&str>) -> String {
    let mut body = format!(r#"{{"styleName":"{style}","isExpert":false"#);
    if let Some(s) = setting {
        body.push_str(&format!(r#","menuSetting":"{s}""#));
    }
    if let Some(l) = label {
        body.push_str(&format!(r#","stringValue":"{l}""#));
    }
    body.push('}');
    body
}

/// A colour: `#ff8800`, `ff8800`, `255,136,0`, or a name.
pub fn parse_colour(s: &str) -> Option<(u8, u8, u8)> {
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

/// One light: its layer (1 on most TVs), side, number along the side, colour.
#[derive(Debug, Clone, PartialEq)]
pub struct Light {
    pub layer: u32,
    pub side: String,
    pub n: u32,
    pub rgb: (u8, u8, u8),
}

/// The lights in a `/6/ambilight/processed` (or `measured`, `cached`) reply:
/// `{"layer1":{"left":{"0":{"r":35,"g":40,"b":44},...},...}}`.
pub fn parse_lights(json: &str) -> Vec<Light> {
    const SIDES: [&str; 4] = ["left", "top", "right", "bottom"];
    let Some(v) = crate::http::json(json) else { return Vec::new() };
    let Some(layers) = v.as_object() else { return Vec::new() };
    let mut out = Vec::new();
    for (name, sides) in layers {
        let Some(layer) = name.strip_prefix("layer").and_then(|n| n.parse().ok()) else { continue };
        for (side, lights) in sides.as_object().into_iter().flatten() {
            if !SIDES.contains(&side.as_str()) {
                continue;
            }
            for (n, rgb) in lights.as_object().into_iter().flatten() {
                let c = |k: &str| crate::http::number(rgb, k).map(|v| v.clamp(0.0, 255.0) as u8);
                if let (Ok(n), Some(r), Some(g), Some(b)) = (n.parse(), c("r"), c("g"), c("b")) {
                    out.push(Light { layer, side: side.clone(), n, rgb: (r, g, b) });
                }
            }
        }
    }
    // the TV's order: layer, then round the screen, then along each side
    out.sort_by_key(|l| (l.layer, SIDES.iter().position(|s| *s == l.side), l.n));
    out
}

/// The lights and the colours they're showing now, as lines; with a block of
/// the colour itself when `swatch` (a terminal).
pub fn lights(tv: &Tv, swatch: bool) -> Result<Vec<String>> {
    let list = parse_lights(&get(tv, "/6/ambilight/processed")?);
    if list.is_empty() {
        return failed("ambilight: the TV didn't list any lights");
    }
    let layers = list.iter().map(|l| l.layer).max().unwrap_or(1);
    Ok(list
        .iter()
        .map(|l| {
            let (r, g, b) = l.rgb;
            let block = if swatch { format!("  \x1b[48;2;{r};{g};{b}m      \x1b[0m") } else { String::new() };
            let layer = if layers > 1 { format!("layer {} ", l.layer) } else { String::new() };
            format!("{layer}{:<6} {}  #{r:02x}{g:02x}{b:02x}{block}", l.side, l.n)
        })
        .collect())
}

/// Whether `system` (a `/6/system` reply) lists Ambilight among the TV's features.
pub fn lists_ambilight(system: &str) -> bool {
    system.find("\"ambilight\"").is_some_and(|i| system[i..].split(']').next().is_some_and(|l| l.contains("\"Ambilight\"")))
}

/// Whether the TV has Ambilight tveee can control: Philips support on
/// (the plugin), and the TV says so in its public system info (no pairing
/// needed). Asked once a run (when the TV answers).
pub fn has_ambilight(tv: &Tv) -> bool {
    static KNOWN: std::sync::Mutex<Option<bool>> = std::sync::Mutex::new(None);
    if crate::pairing::by_id("jointspace").is_none() {
        return false;
    }
    let mut known = KNOWN.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(k) = *known {
        return k;
    }
    let Ok((200, system)) = crate::http::get(&tv.ip, 1925, "/6/system") else { return false };
    let has = lists_ambilight(&system);
    *known = Some(has);
    has
}

/// What the ui's Ambilight panel shows.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(not(feature = "ui"), allow(dead_code))]
pub enum Panel {
    /// No Ambilight here (or no way to control it).
    Missing,
    /// It has one, but tveee isn't paired with the TV yet.
    NeedsPairing,
    /// The state and each light's colour now.
    Shown { state: State, lights: Vec<Light> },
    Failed(String),
}

/// Everything the panel shows, read in one go.
#[cfg_attr(not(feature = "ui"), allow(dead_code))]
pub fn panel(tv: &Tv) -> Panel {
    if !has_ambilight(tv) {
        return Panel::Missing;
    }
    if crate::pairing::load("jointspace", &tv.ip).is_none() {
        return Panel::NeedsPairing;
    }
    let (state, lights) = std::thread::scope(|sc| {
        let l = sc.spawn(|| get(tv, "/6/ambilight/processed").map(|j| parse_lights(&j)));
        (state(tv), l.join().unwrap_or_else(|_| failed("ambilight: a read failed")))
    });
    match state {
        Ok(state) => Panel::Shown { state, lights: lights.unwrap_or_default() },
        Err(e) => Panel::Failed(e.to_string()),
    }
}

/// What the lights are doing.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct State {
    pub power: bool,
    /// The TV's mode: internal, manual (outside control), lounge, expert, ...
    pub mode: Option<String>,
    /// FOLLOW_VIDEO, FOLLOW_AUDIO, FOLLOW_COLOR, OFF (none under outside control).
    pub style: Option<String>,
    /// The style's setting (STANDARD, VU_METER, HOT_LAVA, ...).
    pub setting: Option<String>,
    /// Ambilight+Hue, if the TV has it.
    pub hue: Option<bool>,
    /// How many lights on each side that has any.
    pub sides: Vec<(String, u32)>,
}

impl State {
    /// Something outside the TV is setting the colours.
    pub fn outside(&self) -> bool {
        self.mode.as_deref() == Some("manual")
    }

    /// As `tveee ambilight` prints it.
    pub fn lines(&self) -> Vec<String> {
        let mut out = vec![format!("power:   {}", if self.power { "on" } else { "off" })];
        if self.mode.is_some() {
            let who = if self.outside() { "outside the TV (tveee's 'set', or another program)" } else { "the TV" };
            out.push(format!("control: {who}"));
        }
        if let Some(style) = &self.style {
            let setting = self.setting.as_ref().map(|s| format!(", {}", s.to_lowercase().replace('_', " "))).unwrap_or_default();
            out.push(format!("style:   {}{setting}", style.to_lowercase().replace('_', " ")));
        }
        if let Some(h) = self.hue {
            out.push(format!("hue:     {} (Ambilight+Hue)", if h { "on" } else { "off" }));
        }
        if !self.sides.is_empty() {
            out.push(format!("lights:  {}", self.sides.iter().map(|(s, n)| format!("{n} {s}")).collect::<Vec<_>>().join(", ")));
        }
        out
    }
}

/// What the lights are doing, read from the TV.
pub fn state(tv: &Tv) -> Result<State> {
    let (power, mode, config, hue, layout) = std::thread::scope(|sc| {
        let p = sc.spawn(|| get(tv, "/6/ambilight/power"));
        let m = sc.spawn(|| get(tv, "/6/ambilight/mode"));
        let c = sc.spawn(|| get(tv, "/6/ambilight/currentconfiguration"));
        let h = sc.spawn(|| get(tv, "/6/HueLamp/power"));
        let l = get(tv, "/6/ambilight/topology");
        let j = |h: std::thread::ScopedJoinHandle<'_, Result<String>>| h.join().unwrap_or_else(|_| failed("ambilight: a read failed"));
        (j(p), j(m), j(c), j(h), l)
    });
    // power is the one every Ambilight TV has: its error is the one to show
    let power = power?;
    let field = |json: &Result<String>, k: &str| json.as_ref().ok().and_then(|j| crate::http::text(&crate::http::json(j)?, k));
    let on = |v: Option<String>| v.map(|p| p.eq_ignore_ascii_case("on"));
    let layout = layout.ok().and_then(|l| crate::http::json(&l));
    let sides = layout
        .as_ref()
        .map(|l| {
            ["left", "top", "right", "bottom"]
                .iter()
                .map(|k| (k.to_string(), crate::http::number(l, k).unwrap_or(0.0) as u32))
                .filter(|(_, n)| *n > 0)
                .collect()
        })
        .unwrap_or_default();
    Ok(State {
        power: on(crate::http::json(&power).and_then(|v| crate::http::text(&v, "power"))).unwrap_or(false),
        mode: field(&mode, "current"),
        style: field(&config, "styleName"),
        setting: field(&config, "menuSetting"),
        hue: on(field(&hue, "power")),
        sides,
    })
}

/// What the lights are doing, as lines.
pub fn status(tv: &Tv) -> Result<Vec<String>> {
    Ok(state(tv)?.lines())
}

/// A change the ui can make.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(not(feature = "ui"), allow(dead_code))]
pub enum Change {
    Power(bool),
    /// One of the TV's styles, with its setting (and the label colours need).
    Style { style: &'static str, setting: &'static str, label: Option<&'static str> },
    Hue(bool),
    /// Every light one colour (outside control).
    All((u8, u8, u8)),
    /// One light (side, number) one colour (outside control).
    One(String, u32, (u8, u8, u8)),
}

/// Make `change`; returns what happened.
#[cfg_attr(not(feature = "ui"), allow(dead_code))]
pub fn apply(tv: &Tv, change: &Change) -> Result<String> {
    match change {
        Change::Power(on) => set_power(tv, *on),
        Change::Style { style, setting, label } => {
            set_style(tv, style, Some(setting), *label)?;
            Ok(format!("ambilight: {}", setting.to_lowercase().replace('_', " ")))
        }
        Change::Hue(on) => {
            post(tv, "/6/HueLamp/power", &format!(r#"{{"power":"{}"}}"#, if *on { "On" } else { "Off" }))?;
            Ok(format!("Ambilight+Hue {}", if *on { "on" } else { "off" }))
        }
        Change::All(rgb) => set_colour(tv, *rgb),
        Change::One(side, n, rgb) => set_light(tv, side, *n, *rgb),
    }
}

/// A row of the ui's Ambilight dialog: its options (with the change each
/// makes; None on the lights row, where a colour is asked for), and which
/// one is the TV's now.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(not(feature = "ui"), allow(dead_code))]
pub struct Row {
    pub name: &'static str,
    pub options: Vec<(String, Option<Change>)>,
    pub current: Option<usize>,
}

/// The dialog's rows for `state` and `lights`: power, style, the style's
/// settings, Ambilight+Hue (if there), and the lights (all, or each one).
#[cfg_attr(not(feature = "ui"), allow(dead_code))]
pub fn rows(state: &State, lights: &[Light]) -> Vec<Row> {
    let mut out = vec![Row {
        name: "power",
        options: vec![("on".into(), Some(Change::Power(true))), ("off".into(), Some(Change::Power(false)))],
        current: Some(if state.power { 0 } else { 1 }),
    }];
    let style = if state.outside() { None } else { state.style.as_deref() };
    let setting = state.setting.as_deref();
    // the setting to keep when going back to a style: the one it had, if this was it
    let keep = |s: &str, table: &[(&'static str, &'static str)], default: &'static str| -> &'static str {
        if style == Some(s) {
            if let Some((_, tv)) = table.iter().find(|(_, tv)| Some(*tv) == setting) {
                return tv;
            }
        }
        default
    };
    let colours: Vec<(&'static str, &'static str)> = COLOURS.iter().map(|(n, tv, _)| (*n, *tv)).collect();
    let label_of = |tv: &str| COLOURS.iter().find(|c| c.1 == tv).map(|c| c.2);
    out.push(Row {
        name: "style",
        options: vec![
            ("video".into(), Some(Change::Style { style: "FOLLOW_VIDEO", setting: keep("FOLLOW_VIDEO", VIDEO, "STANDARD"), label: None })),
            ("sound".into(), Some(Change::Style { style: "FOLLOW_AUDIO", setting: keep("FOLLOW_AUDIO", AUDIO, "ENERGY_ADAPTIVE_BRIGHTNESS"), label: None })),
            ("colour".into(), {
                let s = keep("FOLLOW_COLOR", &colours, "ISF");
                Some(Change::Style { style: "FOLLOW_COLOR", setting: s, label: label_of(s) })
            }),
        ],
        current: match style {
            Some("FOLLOW_VIDEO") => Some(0),
            Some("FOLLOW_AUDIO") => Some(1),
            Some("FOLLOW_COLOR") => Some(2),
            _ => None,
        },
    });
    let settings = |table: &[(&'static str, &'static str)], style: &'static str, labels: bool| Row {
        name: "setting",
        options: table
            .iter()
            .map(|(n, tv)| (n.to_string(), Some(Change::Style { style, setting: tv, label: if labels { label_of(tv) } else { None } })))
            .collect(),
        current: table.iter().position(|(_, tv)| Some(*tv) == setting),
    };
    match style {
        Some("FOLLOW_VIDEO") => out.push(settings(VIDEO, "FOLLOW_VIDEO", false)),
        Some("FOLLOW_AUDIO") => out.push(settings(AUDIO, "FOLLOW_AUDIO", false)),
        Some("FOLLOW_COLOR") => out.push(settings(&colours, "FOLLOW_COLOR", true)),
        _ => {}
    }
    if let Some(h) = state.hue {
        out.push(Row {
            name: "hue",
            options: vec![("on".into(), Some(Change::Hue(true))), ("off".into(), Some(Change::Hue(false)))],
            current: Some(if h { 0 } else { 1 }),
        });
    }
    let mut options = vec![("all".to_string(), None)];
    options.extend(lights.iter().map(|l| (format!("{} {}", l.side, l.n), None)));
    out.push(Row { name: "lights", options, current: None });
    out
}

pub fn set_power(tv: &Tv, on: bool) -> Result<String> {
    post(tv, "/6/ambilight/power", &format!(r#"{{"power":"{}"}}"#, if on { "On" } else { "Off" }))?;
    Ok(format!("ambilight {}", if on { "on" } else { "off" }))
}

pub fn set_mode(tv: &Tv, mode: &str) -> Result<()> {
    post(tv, "/6/ambilight/mode", &format!(r#"{{"current":"{mode}"}}"#))
}

/// A style of the TV's own: the TV takes the lights back first.
pub fn set_style(tv: &Tv, style: &str, setting: Option<&str>, label: Option<&str>) -> Result<()> {
    set_mode(tv, "internal")?;
    post(tv, "/6/ambilight/currentconfiguration", &style_body(style, setting, label))
}

/// One light one colour (`side` left/top/right/bottom, `n` along it), the
/// others as they are: the lights go to outside control (manual).
pub fn set_light(tv: &Tv, side: &str, n: u32, (r, g, b): (u8, u8, u8)) -> Result<String> {
    set_mode(tv, "manual")?;
    post(tv, "/6/ambilight/cached", &format!(r#"{{"layer1":{{"{side}":{{"{n}":{{"r":{r},"g":{g},"b":{b}}}}}}}}}"#))?;
    Ok(format!("ambilight: {side} {n} #{r:02x}{g:02x}{b:02x} (outside control)"))
}

/// Every light one colour: the lights go to outside control (manual).
pub fn set_colour(tv: &Tv, (r, g, b): (u8, u8, u8)) -> Result<String> {
    set_mode(tv, "manual")?;
    post(tv, "/6/ambilight/cached", &format!(r#"{{"r":{r},"g":{g},"b":{b}}}"#))?;
    Ok(format!("ambilight: every light #{r:02x}{g:02x}{b:02x} (outside control; 'tveee ambilight video' gives it back to the TV)"))
}

fn ambilight(c: &Ctx, args: &[String]) -> Result<()> {
    let tv = &c.tv;
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let said = match words.as_slice() {
        [] => {
            status(tv)?.iter().for_each(|l| println!("{l}"));
            return Ok(());
        }
        ["on"] => set_power(tv, true)?,
        ["off"] => set_power(tv, false)?,
        ["video", rest @ ..] => {
            let name = rest.first().copied().unwrap_or("standard");
            let Some(s) = lookup(VIDEO, name) else { return usage(format!("ambilight video: one of {}", names(VIDEO))) };
            set_style(tv, "FOLLOW_VIDEO", Some(s), None)?;
            format!("ambilight follows the video ({name})")
        }
        ["audio", rest @ ..] => {
            let name = rest.first().copied().unwrap_or("brightness");
            let Some(s) = lookup(AUDIO, name) else { return usage(format!("ambilight audio: one of {}", names(AUDIO))) };
            set_style(tv, "FOLLOW_AUDIO", Some(s), None)?;
            format!("ambilight follows the sound ({name})")
        }
        ["colour" | "color", name] => {
            let Some((_, s, label)) = COLOURS.iter().find(|(n, _, _)| n == name) else {
                return usage(format!("ambilight colour: one of {} (or 'tveee ambilight set <colour>' for any colour)", COLOURS.iter().map(|c| c.0).collect::<Vec<_>>().join(", ")));
            };
            set_style(tv, "FOLLOW_COLOR", Some(s), Some(label))?;
            format!("ambilight: {label}")
        }
        ["mode"] => crate::http::json(&get(tv, "/6/ambilight/mode")?).and_then(|v| crate::http::text(&v, "current")).unwrap_or_default(),
        ["mode", m] => {
            if !MODES.iter().any(|(n, _)| n == m) {
                return usage(format!("ambilight mode: one of {}", MODES.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(", ")));
            }
            set_mode(tv, m)?;
            format!("ambilight mode: {m}")
        }
        ["lights" | "lamps"] => {
            use std::io::IsTerminal;
            lights(tv, std::io::stdout().is_terminal())?.iter().for_each(|l| println!("{l}"));
            return Ok(());
        }
        ["set", side @ ("left" | "top" | "right" | "bottom"), n, colour @ ..] if !colour.is_empty() => {
            let Ok(n) = n.parse() else { return usage("ambilight set: set <side> <number> <colour>, e.g. set left 0 red") };
            let Some(rgb) = parse_colour(&colour.join(" ")) else { return usage("ambilight set: a colour such as red, #ff8800 or 255,136,0") };
            set_light(tv, side, n, rgb)?
        }
        ["set", colour @ ..] if !colour.is_empty() => {
            let Some(rgb) = parse_colour(&colour.join(" ")) else { return usage("ambilight set: a colour such as red, #ff8800 or 255,136,0") };
            set_colour(tv, rgb)?
        }
        ["hue", on @ ("on" | "off")] => {
            post(tv, "/6/HueLamp/power", &format!(r#"{{"power":"{}"}}"#, if *on == "on" { "On" } else { "Off" }))?;
            format!("Ambilight+Hue {on}")
        }
        _ => return usage("usage: tveee ambilight [on|off | lights | video [setting] | audio [effect] | colour <name> | set [<side> <n>] <colour> | mode [internal|manual|expert] | hue on|off]"),
    };
    println!("{said}");
    Ok(())
}

pub fn command_actions() -> Vec<ActionDescriptor> {
    vec![ActionDescriptor::terminal("ambilight.ambilight", CAT, "Ambilight: what it's doing, its styles, and handing it to outside control", ambilight).cli("ambilight", "[on|off | lights | video | audio | colour | set | mode | hue]", "Ambilight: what it's doing, its styles, and handing it to outside control")
    .long(
        "With nothing: what the lights are doing (power, who's in control, style,\n\
         Ambilight+Hue, how many lights). Needs pairing ('tveee pair jointspace').\n\
         \n\
         \x20 on | off                 the lights on or off\n\
         \x20 lights                   each light and the colour it's showing now\n\
         \x20 video [setting]          follow the picture: standard (the default),\n\
         \x20                          natural, vivid, game, comfort, relax\n\
         \x20 audio [effect]           follow the sound: brightness (the default),\n\
         \x20                          colours, vu-meter, spectrum, knight-rider,\n\
         \x20                          knight-rider-alternating, flash, strobe, party,\n\
         \x20                          random\n\
         \x20 colour <name>            one of the TV's colours (its lounge light):\n\
         \x20                          hot-lava, warm-white, cool-white, fresh-nature,\n\
         \x20                          deep-water\n\
         \x20 set <colour>             every light one colour (red, warm, #ff8800,\n\
         \x20                          255,136,0); hands the lights to outside control\n\
         \x20 set <side> <n> <colour>  one light (see 'lights'), e.g. set left 0 red\n\
         \x20 mode [internal|manual|expert]\n\
         \x20                          who drives the lights: the TV (internal), or\n\
         \x20                          something outside it, such as tveee's 'set' or\n\
         \x20                          a program like Hyperion (manual, expert)\n\
         \x20 hue on|off               Ambilight+Hue (Philips Hue lamps joining in)\n\
         \n\
         Picking a style gives the lights back to the TV.",
    )]
}

pub fn actions() -> Vec<ActionDescriptor> {
    use crate::actions::ActionDescriptor as A;
    let mut v = command_actions();
    v.extend([
        A::new("ambilight.power", WHAT, "ambilight on/off", |cx, _| {
            let on = crate::http::json(&get(cx.tv, "/6/ambilight/power")?).and_then(|v| crate::http::text(&v, "power")).is_some_and(|p| p.eq_ignore_ascii_case("on"));
            set_power(cx.tv, !on).map(Into::into)
        })
        .key('A'),
        A::new("ambilight.video", WHAT, "ambilight: follow the video", |cx, _| {
            set_style(cx.tv, "FOLLOW_VIDEO", Some("STANDARD"), None)?;
            Ok("ambilight follows the video".into())
        })
        .key('v'),
        A::new("ambilight.sound", WHAT, "ambilight: follow the sound", |cx, _| {
            set_style(cx.tv, "FOLLOW_AUDIO", Some("ENERGY_ADAPTIVE_BRIGHTNESS"), None)?;
            Ok("ambilight follows the sound".into())
        })
        .key('u'),
        A::new("ambilight.warm", WHAT, "ambilight: warm white (lounge light)", |cx, _| {
            set_style(cx.tv, "FOLLOW_COLOR", Some("ISF"), Some("Warm White"))?;
            Ok("ambilight: warm white".into())
        })
        .key('l'),
        A::new("ambilight.colour", WHAT, "ambilight: one colour (outside control)", |cx, a| {
            let text = a.join(" ");
            match parse_colour(&text) {
                Some(rgb) => set_colour(cx.tv, rgb).map(Into::into),
                None => failed(format!("'{text}' isn't a colour tveee knows (try red, #ff8800 or 255,136,0)")),
            }
        })
        .key('c')
        .text("Colour:", "red, warm, #ff8800 or 255,136,0"),
    ]);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours() {
        assert_eq!(parse_colour("red"), Some((255, 0, 0)));
        assert_eq!(parse_colour("#FF8800"), Some((255, 136, 0)));
        assert_eq!(parse_colour("ff8800"), Some((255, 136, 0)));
        assert_eq!(parse_colour("255, 136, 0"), Some((255, 136, 0)));
        assert_eq!(parse_colour("255 136 0"), Some((255, 136, 0)));
        assert_eq!(parse_colour("256,0,0"), None);
        assert_eq!(parse_colour("chartreuse"), None);
    }

    #[test]
    fn reads_lights() {
        let json = r#"{"layer1":{"left":{"0":{"r":35,"g":40,"b":44},"1":{"r":37,"g":25,"b":25}},"right":{"0":{"r":29,"g":29,"b":54}}}}"#;
        let l = parse_lights(json);
        assert_eq!(l.len(), 3);
        assert_eq!(l[0], Light { layer: 1, side: "left".into(), n: 0, rgb: (35, 40, 44) });
        assert_eq!((l[2].side.as_str(), l[2].n, l[2].rgb), ("right", 0, (29, 29, 54)));
        assert!(parse_lights("{}").is_empty());
    }

    #[test]
    fn finds_ambilight_in_the_features() {
        assert!(lists_ambilight(r#"{"featuring":{"jsonfeatures":{"ambilight":["LoungeLight","Hue","Ambilight"],"menuitems":["Setup_Menu"]}}}"#));
        assert!(!lists_ambilight(r#"{"featuring":{"jsonfeatures":{"ambilight":[],"menuitems":["Ambilight"]}}}"#));
        assert!(!lists_ambilight(r#"{"featuring":{"jsonfeatures":{"menuitems":["Setup_Menu"]}}}"#));
    }

    #[test]
    fn dialog_rows() {
        let st = State { power: true, mode: Some("internal".into()), style: Some("FOLLOW_VIDEO".into()), setting: Some("VIVID".into()), hue: Some(true), sides: vec![] };
        let lights = vec![Light { layer: 1, side: "left".into(), n: 0, rgb: (1, 2, 3) }];
        let r = rows(&st, &lights);
        assert_eq!(r.iter().map(|r| r.name).collect::<Vec<_>>(), ["power", "style", "setting", "hue", "lights"]);
        assert_eq!((r[1].current, r[2].current), (Some(0), Some(2)));
        // going back to video keeps vivid; sound starts at its default
        assert_eq!(r[1].options[0].1, Some(Change::Style { style: "FOLLOW_VIDEO", setting: "VIVID", label: None }));
        assert_eq!(r[1].options[1].1, Some(Change::Style { style: "FOLLOW_AUDIO", setting: "ENERGY_ADAPTIVE_BRIGHTNESS", label: None }));
        assert_eq!(r[4].options.iter().map(|o| o.0.as_str()).collect::<Vec<_>>(), ["all", "left 0"]);
        // outside control: no style marked, no settings row; no Hue: no hue row
        let st = State { mode: Some("manual".into()), style: None, hue: None, ..st };
        let r = rows(&st, &[]);
        assert_eq!(r.iter().map(|r| r.name).collect::<Vec<_>>(), ["power", "style", "lights"]);
        assert_eq!(r[1].current, None);
        // colours carry their label
        let st = State { mode: Some("lounge".into()), style: Some("FOLLOW_COLOR".into()), setting: Some("HOT_LAVA".into()), ..State::default() };
        let r = rows(&st, &[]);
        assert_eq!(r[2].options[0].1, Some(Change::Style { style: "FOLLOW_COLOR", setting: "HOT_LAVA", label: Some("Hot Lava") }));
        assert_eq!(r[2].current, Some(0));
    }

    #[test]
    fn styles() {
        assert_eq!(style_body("FOLLOW_VIDEO", Some("STANDARD"), None), r#"{"styleName":"FOLLOW_VIDEO","isExpert":false,"menuSetting":"STANDARD"}"#);
        assert_eq!(
            style_body("FOLLOW_COLOR", Some("HOT_LAVA"), Some("Hot Lava")),
            r#"{"styleName":"FOLLOW_COLOR","isExpert":false,"menuSetting":"HOT_LAVA","stringValue":"Hot Lava"}"#
        );
        assert_eq!(lookup(VIDEO, "vivid"), Some("VIVID"));
        assert_eq!(lookup(AUDIO, "VU_METER"), Some("VU_METER"));
        assert_eq!(lookup(VIDEO, "loud"), None);
    }
}

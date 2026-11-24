//! Ambilight on Philips TVs, through the JointSpace API (so it needs
//! pairing: `tveee pair jointspace`). Part of the `philips` plugin.
//!
//! The TV drives the lights itself in one of its styles (following the
//! video, the sound, a colour; lounge), or hands them to something outside
//! it (`mode manual` / `expert`: tveee, or a program such as Hyperion),
//! which then sets the colours.

use crate::error::{failed, usage, Result};
use crate::plugins::philips::{js_get_paired, js_post};
use crate::registry::Command;
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
    let mut out = Vec::new();
    // walk the keys in order: a "layerN" opens a layer, a side name a side,
    // a number a light (whose r, g, b follow)
    let (mut layer, mut side) = (0, String::new());
    let mut rest = json;
    while let Some(q) = rest.find('"') {
        let after = &rest[q + 1..];
        let Some(end) = after.find('"') else { break };
        let key = &after[..end];
        rest = &after[end + 1..];
        if let Some(n) = key.strip_prefix("layer").and_then(|n| n.parse().ok()) {
            layer = n;
        } else if ["left", "top", "right", "bottom"].contains(&key) {
            side = key.to_string();
        } else if let Ok(n) = key.parse::<u32>() {
            let Some(close) = rest.find('}') else { break };
            let obj = &rest[..close];
            let c = |k: &str| crate::http::number_field(obj, k).map(|v| v.clamp(0.0, 255.0) as u8);
            if let (Some(r), Some(g), Some(b)) = (c("r"), c("g"), c("b")) {
                out.push(Light { layer, side: side.clone(), n, rgb: (r, g, b) });
            }
            rest = &rest[close..];
        }
    }
    out
}

/// The lights and the colours they're showing now, as lines; with a block of
/// the colour itself when `swatch` (a terminal).
pub fn lights(tv: &Tv, swatch: bool) -> Result<Vec<String>> {
    tv.connect()?;
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
    /// The state (as [`status`] lines) and each light's colour now.
    Shown { status: Vec<String>, lights: Vec<Light> },
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
    let (status, lights) = std::thread::scope(|sc| {
        let l = sc.spawn(|| get(tv, "/6/ambilight/processed").map(|j| parse_lights(&j)));
        (status(tv), l.join().unwrap_or_else(|_| failed("ambilight: a read failed")))
    });
    match status {
        Ok(status) => Panel::Shown { status, lights: lights.unwrap_or_default() },
        Err(e) => Panel::Failed(e.to_string()),
    }
}

/// What the lights are doing, as lines.
pub fn status(tv: &Tv) -> Result<Vec<String>> {
    tv.connect()?;
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
    let field = |json: &Result<String>, k: &str| json.as_ref().ok().and_then(|j| crate::http::string_field(j, k));
    let mut out = vec![format!("power:   {}", crate::http::string_field(&power, "power").unwrap_or_default().to_lowercase())];
    if let Some(m) = field(&mode, "current") {
        let who = if m == "manual" { "outside the TV (tveee's 'set', or another program)" } else { "the TV" };
        out.push(format!("control: {who}"));
    }
    if let Some(style) = field(&config, "styleName") {
        let setting = field(&config, "menuSetting").map(|s| format!(", {}", s.to_lowercase().replace('_', " "))).unwrap_or_default();
        out.push(format!("style:   {}{setting}", style.to_lowercase().replace('_', " ")));
    }
    if let Some(h) = field(&hue, "power") {
        out.push(format!("hue:     {} (Ambilight+Hue)", h.to_lowercase()));
    }
    if let Ok(l) = &layout {
        let side = |k: &str| crate::http::number_field(l, k).unwrap_or(0.0) as u32;
        let lights: Vec<String> = [("left", side("left")), ("top", side("top")), ("right", side("right")), ("bottom", side("bottom"))]
            .iter()
            .filter(|(_, n)| *n > 0)
            .map(|(s, n)| format!("{n} {s}"))
            .collect();
        out.push(format!("lights:  {}", lights.join(", ")));
    }
    Ok(out)
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
        ["mode"] => crate::http::string_field(&get(tv, "/6/ambilight/mode")?, "current").unwrap_or_default(),
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

pub fn commands() -> Vec<Command> {
    vec![Command::new(
        CAT,
        "ambilight",
        "[on|off | lights | video | audio | colour | set | mode | hue]",
        "Ambilight: what it's doing, its styles, and handing it to outside control",
        ambilight,
    )
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

fn item(f: fn(&Tv) -> Result<String>) -> fn(&Tv) -> Result<String> {
    f
}

pub fn menu() -> Vec<crate::menu::MenuItem> {
    use crate::menu::MenuItem;
    vec![
        MenuItem::new(WHAT, 'A', "ambilight on/off", item(|tv| {
            let on = crate::http::string_field(&get(tv, "/6/ambilight/power")?, "power").is_some_and(|p| p.eq_ignore_ascii_case("on"));
            set_power(tv, !on)
        })),
        MenuItem::new(WHAT, 'v', "ambilight: follow the video", item(|tv| {
            set_style(tv, "FOLLOW_VIDEO", Some("STANDARD"), None)?;
            Ok("ambilight follows the video".into())
        })),
        MenuItem::new(WHAT, 'u', "ambilight: follow the sound", item(|tv| {
            set_style(tv, "FOLLOW_AUDIO", Some("ENERGY_ADAPTIVE_BRIGHTNESS"), None)?;
            Ok("ambilight follows the sound".into())
        })),
        MenuItem::new(WHAT, 'l', "ambilight: warm white (lounge light)", item(|tv| {
            set_style(tv, "FOLLOW_COLOR", Some("ISF"), Some("Warm White"))?;
            Ok("ambilight: warm white".into())
        })),
        MenuItem::with_text(WHAT, 'c', "ambilight: one colour (outside control)", "Colour:", "red, warm, #ff8800 or 255,136,0", |tv, text| {
            match parse_colour(text) {
                Some(rgb) => set_colour(tv, rgb),
                None => failed(format!("'{text}' isn't a colour tveee knows (try red, #ff8800 or 255,136,0)")),
            }
        }),
    ]
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

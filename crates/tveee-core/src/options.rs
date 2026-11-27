//! tveee's own settings (not the TV's): on/off options kept in
//! `~/.config/tveee/options.txt` as `name=on|off`, and a choice for each
//! vendor plugin (`plugin.kodi=on|off`; none: decided by checking the TV).
//! Set from the UI's options dialog (`,`) or `tveee options`.

use crate::error::{failed, usage, Result};
use crate::actions::ActionDescriptor;
use crate::Ctx;
use std::sync::Mutex;

pub struct Opt {
    pub name: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    pub default: bool,
}

pub const SCROLL_ANIMATION: &str = "scroll-animation";
pub const FAST_KEYS: &str = "fast-keys";
pub const SHOW_PACKAGES: &str = "show-packages";
pub const LOW_BANDWIDTH: &str = "low-bandwidth";
pub const SHOW_LOAD: &str = "show-load";
pub const PREDICT_POSITION: &str = "predict-position";

pub const OPTIONS: &[Opt] = &[
    Opt {
        name: SCROLL_ANIMATION,
        label: "scroll animations",
        help: "the UI's guide slides smoothly when moving through time (else it jumps)",
        default: true,
    },
    Opt {
        name: FAST_KEYS,
        label: "fast keys",
        help: "keys go straight to the TV's input devices (off: Android's slower 'input' command)",
        default: true,
    },
    Opt {
        name: SHOW_PACKAGES,
        label: "package names",
        help: "the UI's launcher shows each app's package (com.example.app) too; # there toggles it",
        default: false,
    },
    Opt {
        name: LOW_BANDWIDTH,
        label: "low bandwidth",
        help: "the UI reads the TV every 4s, not every second (what you do still shows at once)",
        default: false,
    },
    Opt {
        name: PREDICT_POSITION,
        label: "predict position",
        help: "the UI moves the progress bar between reads, as fast as it's playing (still while you pause or seek)",
        default: true,
    },
    Opt {
        name: SHOW_LOAD,
        label: "load & memory",
        help: "the UI's top line shows the TV's load and free memory (off: not read, unless the system panel is open)",
        default: true,
    },
];

fn file() -> Option<std::path::PathBuf> {
    Some(crate::dotenv::config_dir()?.join("options.txt"))
}

/// `name=on|off` lines; anything else is ignored.
pub fn parse(text: &str) -> Vec<(String, bool)> {
    text.lines()
        .filter_map(|l| {
            let (k, v) = l.split_once('=')?;
            let on = match v.trim() {
                "on" | "true" | "yes" | "1" => true,
                "off" | "false" | "no" | "0" => false,
                _ => return None,
            };
            Some((k.trim().to_string(), on))
        })
        .collect()
}

static SET: Mutex<Option<Vec<(String, bool)>>> = Mutex::new(None);

fn saved() -> Vec<(String, bool)> {
    let mut set = SET.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    set.get_or_insert_with(|| file().and_then(|f| std::fs::read_to_string(f).ok()).map(|t| parse(&t)).unwrap_or_default()).clone()
}

/// Whether option `name` is on (its default if it was never set).
pub fn get(name: &str) -> bool {
    let default = OPTIONS.iter().find(|o| o.name == name).is_some_and(|o| o.default);
    saved().iter().rev().find(|(k, _)| k == name).map_or(default, |(_, v)| *v)
}

/// Set option `name` and save every option.
pub fn set(name: &str, on: bool) -> Result<()> {
    if !OPTIONS.iter().any(|o| o.name == name) {
        return usage(format!("options: no option '{name}' (see 'tveee options')"));
    }
    let mut all = saved();
    all.retain(|(k, _)| k != name);
    all.push((name.to_string(), on));
    save(all)
}

/// The file's text for `all`: the options in their order, then the
/// plugin choices.
pub fn format(all: &[(String, bool)]) -> String {
    let on = |v: bool| if v { "on" } else { "off" };
    let mut text = String::from("# tveee options (tveee options, or , in the ui)\n");
    for o in OPTIONS {
        if let Some((_, v)) = all.iter().find(|(k, _)| k == o.name) {
            text.push_str(&format!("{}={}\n", o.name, on(*v)));
        }
    }
    for (k, v) in all.iter().filter(|(k, _)| k.starts_with(PLUGIN)) {
        text.push_str(&format!("{k}={}\n", on(*v)));
    }
    text
}

fn save(all: Vec<(String, bool)>) -> Result<()> {
    let Some(f) = file() else { return failed("options: no config folder to save them in") };
    if let Some(dir) = f.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(f, format(&all))?;
    *SET.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(all);
    Ok(())
}

/// The prefix of a plugin's choice in the file: `plugin.kodi=off`.
const PLUGIN: &str = "plugin.";

/// The choice saved for vendor plugin `name`: on, off, or None (decided by
/// checking the TV).
pub fn plugin(name: &str) -> Option<bool> {
    let key = format!("{PLUGIN}{name}");
    saved().iter().rev().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

/// Every plugin choice saved: name -> on.
pub fn plugin_choices() -> std::collections::HashMap<String, bool> {
    saved().into_iter().filter_map(|(k, v)| Some((k.strip_prefix(PLUGIN)?.to_string(), v))).collect()
}

/// Save a choice for vendor plugin `name` (None: back to checking the TV).
/// It applies from tveee's next start.
pub fn set_plugin(name: &str, choice: Option<bool>) -> Result<()> {
    let key = format!("{PLUGIN}{name}");
    let mut all = saved();
    all.retain(|(k, _)| *k != key);
    if let Some(on) = choice {
        all.push((key, on));
    }
    save(all)
}

/// A choice said: "on", "off", "auto".
pub fn choice_word(choice: Option<bool>) -> &'static str {
    match choice {
        Some(true) => "on",
        Some(false) => "off",
        None => "auto",
    }
}

fn options(c: &Ctx, args: &[String]) -> Result<()> {
    match args {
        [] => {
            for o in OPTIONS {
                println!("{:<18} {:<4} {}", o.name, if get(o.name) { "on" } else { "off" }, o.help);
            }
            println!("\nvendor plugins (tveee options plugin <name> on|off|auto; from the next start):");
            for p in c.plugins.iter().filter(|p| p.source != crate::plugin::Source::Builtin) {
                let now = if p.enabled { "on now" } else { "off now" };
                println!("  {:<16} {:<4} ({now})  {}", p.name, choice_word(plugin(p.name)), p.desc);
            }
            Ok(())
        }
        [p, name, v] if p == "plugin" => {
            let choice = match v.as_str() {
                "on" => Some(true),
                "off" => Some(false),
                "auto" => None,
                _ => return usage("usage: tveee options plugin <name> on|off|auto"),
            };
            match c.plugins.iter().find(|s| s.name == name) {
                None => return usage(format!("options: no plugin '{name}' (see 'tveee plugins')")),
                Some(s) if s.source == crate::plugin::Source::Builtin => return failed(format!("options: {name} is built in: always on")),
                Some(_) => {}
            }
            set_plugin(name, choice)?;
            println!("plugin {name}: {v} (from tveee's next start)");
            Ok(())
        }
        [name, v] => {
            let on = match v.as_str() {
                "on" => true,
                "off" => false,
                _ => return usage("usage: tveee options [<name> on|off]"),
            };
            set(name, on)?;
            println!("{name}: {v}");
            Ok(())
        }
        _ => usage("usage: tveee options [<name> on|off | plugin <name> on|off|auto]"),
    }
}

pub fn actions() -> Vec<ActionDescriptor> {
    vec![ActionDescriptor::terminal("options.options", "options", "tveee's own settings: list them, or set one", options).cli("options", "[<name> on|off | plugin <name> on|off|auto]", "tveee's own settings: list them, or set one").long(
        "Saved in ~/.config/tveee/options.txt. In the ui, ',' opens the same options.\n\
         \n\
         Vendor plugins (philips, kodi, youtube) are on when checking the TV finds\n\
         what they're for ('auto'); 'on' or 'off' fixes that, from tveee's next start.\n\
         TVEEE_PLUGINS outweighs it. Built-in plugins are always on.",
    )]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_options() {
        assert_eq!(parse("# x\nscroll-animation=off\nother=on\nbad=maybe\n"), [("scroll-animation".to_string(), false), ("other".to_string(), true)]);
        assert!(OPTIONS.iter().any(|o| o.name == SCROLL_ANIMATION));
    }

    #[test]
    fn plugin_choices_kept_after_the_options() {
        let all = vec![("plugin.kodi".to_string(), false), (SCROLL_ANIMATION.to_string(), true), ("plugin.philips".to_string(), true)];
        let text = format(&all);
        assert_eq!(text.lines().skip(1).collect::<Vec<_>>(), ["scroll-animation=on", "plugin.kodi=off", "plugin.philips=on"]);
        assert_eq!(parse(&text), [(SCROLL_ANIMATION.to_string(), true), ("plugin.kodi".to_string(), false), ("plugin.philips".to_string(), true)]);
        assert_eq!((choice_word(Some(true)), choice_word(Some(false)), choice_word(None)), ("on", "off", "auto"));
    }
}

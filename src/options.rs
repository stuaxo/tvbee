//! tveee's own settings (not the TV's): on/off options kept in
//! `~/.config/tveee/options.txt` as `name=on|off`. Set from the UI's options
//! dialog (`,`) or `tveee options`.

use crate::error::{failed, usage, Result};
use crate::registry::Command;
use crate::Ctx;
use std::sync::Mutex;

pub struct Opt {
    pub name: &'static str,
    #[cfg_attr(not(feature = "ui"), allow(dead_code))]
    pub label: &'static str,
    pub help: &'static str,
    pub default: bool,
}

pub const SCROLL_ANIMATION: &str = "scroll-animation";
pub const FAST_KEYS: &str = "fast-keys";
pub const SHOW_PACKAGES: &str = "show-packages";

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
    let Some(f) = file() else { return failed("options: no config folder to save them in") };
    if let Some(dir) = f.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut text = String::from("# tveee options (tveee options, or , in the ui)\n");
    for o in OPTIONS {
        if let Some((_, v)) = all.iter().find(|(k, _)| k == o.name) {
            text.push_str(&format!("{}={}\n", o.name, if *v { "on" } else { "off" }));
        }
    }
    std::fs::write(f, text)?;
    *SET.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(all);
    Ok(())
}

fn options(_: &Ctx, args: &[String]) -> Result<()> {
    match args {
        [] => {
            for o in OPTIONS {
                println!("{:<18} {:<4} {}", o.name, if get(o.name) { "on" } else { "off" }, o.help);
            }
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
        _ => usage("usage: tveee options [<name> on|off]"),
    }
}

pub fn commands() -> Vec<Command> {
    vec![Command::new("options", "options", "[<name> on|off]", "tveee's own settings: list them, or set one", options).long(
        "Saved in ~/.config/tveee/options.txt. In the ui, ',' opens the same options.",
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
}

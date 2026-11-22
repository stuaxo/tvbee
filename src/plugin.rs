//! Plugins are the equivalent of Django apps: each one contributes a set of
//! commands (its help category). Built-in plugins are always on; vendor
//! plugins carry a detection check that decides whether they are enabled
//! for the TV being talked to.
//!
//! Resolution order for a vendor plugin: `TVBEE_PLUGINS` override, then the
//! per-TV cache, then (only when one of its commands was asked for, or on
//! `plugins --refresh`) a live probe whose result is cached. Plain `help`
//! never touches the TV.

use crate::error::Result;
use crate::menu::{self, MenuItem};
use crate::registry::Command;
use crate::tv::Tv;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub enum Enable {
    Always,
    Detect(fn(&Tv) -> Result<bool>),
}

pub struct Plugin {
    pub name: &'static str,
    pub desc: &'static str,
    pub enable: Enable,
    pub commands: fn() -> Vec<Command>,
    /// Actions for the `ui` menu.
    pub menu: fn() -> Vec<MenuItem>,
}

impl Plugin {
    pub fn builtin(name: &'static str, desc: &'static str, commands: fn() -> Vec<Command>) -> Self {
        Plugin { name, desc, enable: Enable::Always, commands, menu: menu::none }
    }

    pub fn detect(
        name: &'static str,
        desc: &'static str,
        detect: fn(&Tv) -> Result<bool>,
        commands: fn() -> Vec<Command>,
    ) -> Self {
        Plugin { name, desc, enable: Enable::Detect(detect), commands, menu: menu::none }
    }

    /// Offer these actions in the `ui` menu.
    pub fn with_menu(mut self, menu: fn() -> Vec<MenuItem>) -> Self {
        self.menu = menu;
        self
    }

    /// Command names and aliases this plugin provides.
    pub fn command_names(&self) -> Vec<&'static str> {
        (self.commands)()
            .iter()
            .flat_map(|c| std::iter::once(c.name).chain(c.aliases.iter().copied()))
            .collect()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    Builtin,
    Override,
    Cache,
    Probed,
    /// Not overridden, not cached, and no probe was allowed (or it failed).
    Unknown,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Builtin => "builtin",
            Source::Override => "override",
            Source::Cache => "cached",
            Source::Probed => "probed",
            Source::Unknown => "unknown",
        }
    }
}

pub struct PluginState {
    pub name: &'static str,
    pub desc: &'static str,
    pub enabled: bool,
    pub source: Source,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Overrides and cache only; never contacts the TV.
    Cached,
    /// As `Cached`, but probe plugins that have no cached answer.
    Probe,
    /// Probe every detectable plugin, ignoring the cache.
    Refresh,
}

/// `philips,-other` -> {philips: true, other: false}
pub fn parse_overrides(s: &str) -> HashMap<String, bool> {
    s.split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| match p.strip_prefix('-') {
            Some(name) => (name.to_string(), false),
            None => (p.to_string(), true),
        })
        .collect()
}

pub fn parse_cache(s: &str) -> HashMap<String, bool> {
    s.lines()
        .filter_map(|l| l.split_once('='))
        .filter_map(|(k, v)| Some((k.trim().to_string(), v.trim().parse().ok()?)))
        .collect()
}

pub fn format_cache(cache: &HashMap<String, bool>) -> String {
    let mut lines: Vec<String> = cache.iter().map(|(k, v)| format!("{k}={v}")).collect();
    lines.sort();
    lines.join("\n") + "\n"
}

/// `$XDG_CACHE_HOME/tvbee/<ip>.plugins` (or `~/.cache/...`).
pub fn cache_path(ip: &str) -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("tvbee").join(format!("{ip}.plugins")))
}

fn read_cache(path: &Path) -> HashMap<String, bool> {
    std::fs::read_to_string(path).map(|s| parse_cache(&s)).unwrap_or_default()
}

fn write_cache(path: &Path, cache: &HashMap<String, bool>) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, format_cache(cache));
}

pub fn resolve(
    plugins: &[Plugin],
    tv: &Tv,
    mode: Mode,
    overrides: &HashMap<String, bool>,
    cache_file: Option<&Path>,
) -> Vec<PluginState> {
    let mut cache = cache_file.map(read_cache).unwrap_or_default();
    let mut dirty = false;
    let mut states = Vec::new();
    for p in plugins {
        let (enabled, source) = match &p.enable {
            Enable::Always => (true, Source::Builtin),
            Enable::Detect(detect) => {
                let cached = cache.get(p.name).copied();
                if let (Some(v), true) = (overrides.get(p.name), mode != Mode::Refresh) {
                    (*v, Source::Override)
                } else if let (Some(v), true) = (cached, mode != Mode::Refresh) {
                    (v, Source::Cache)
                } else if mode != Mode::Cached {
                    match detect(tv) {
                        Ok(v) => {
                            cache.insert(p.name.to_string(), v);
                            dirty = true;
                            (v, Source::Probed)
                        }
                        Err(_) => (false, Source::Unknown),
                    }
                } else {
                    (false, Source::Unknown)
                }
            }
        };
        states.push(PluginState { name: p.name, desc: p.desc, enabled, source });
    }
    if let (true, Some(f)) = (dirty, cache_file) {
        write_cache(f, &cache);
    }
    states
}

/// Whether `sub` is a command of a detectable plugin, i.e. worth probing for.
pub fn needs_probe(plugins: &[Plugin], sub: &str) -> bool {
    plugins
        .iter()
        .any(|p| matches!(p.enable, Enable::Detect(_)) && p.command_names().contains(&sub))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overrides_parse() {
        let o = parse_overrides("philips, -sony,,");
        assert_eq!(o["philips"], true);
        assert_eq!(o["sony"], false);
        assert_eq!(o.len(), 2);
    }

    #[test]
    fn cache_round_trips() {
        let mut c = HashMap::new();
        c.insert("philips".to_string(), true);
        c.insert("sony".to_string(), false);
        assert_eq!(parse_cache(&format_cache(&c)), c);
        assert!(parse_cache("junk\nx=maybe\n").is_empty());
    }

    fn none() -> Vec<Command> {
        Vec::new()
    }

    fn yes(_: &Tv) -> Result<bool> {
        Ok(true)
    }

    fn tv() -> Tv {
        Tv { ip: "0.0.0.0".into() }
    }

    #[test]
    fn resolution_order() {
        let plugins = [Plugin::builtin("core", "", none), Plugin::detect("v", "", yes, none)];
        let no_over = HashMap::new();

        // no probe allowed, nothing cached -> unknown/disabled
        let s = resolve(&plugins, &tv(), Mode::Cached, &no_over, None);
        assert!(s[0].enabled && s[0].source == Source::Builtin);
        assert!(!s[1].enabled && s[1].source == Source::Unknown);

        // probe allowed -> detected
        let s = resolve(&plugins, &tv(), Mode::Probe, &no_over, None);
        assert!(s[1].enabled && s[1].source == Source::Probed);

        // override wins over probing
        let off = parse_overrides("-v");
        let s = resolve(&plugins, &tv(), Mode::Probe, &off, None);
        assert!(!s[1].enabled && s[1].source == Source::Override);
    }
}

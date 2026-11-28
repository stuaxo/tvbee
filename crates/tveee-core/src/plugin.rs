//! Plugins are the equivalent of Django apps: each one contributes a set of
//! commands (its help category). Built-in plugins are always on; vendor
//! plugins carry a detection check that decides whether they are enabled
//! for the TV being talked to.
//!
//! Resolution order for a vendor plugin: `TVEEE_PLUGINS` override, then the
//! choice saved in tveee's options (`plugin.kodi=off`), then the per-TV
//! cache, then (only when one of its commands was asked for, or on
//! `plugins --refresh`) a live probe whose result is cached. Plain `help`
//! never touches the TV.

use crate::actions::ActionDescriptor;
use crate::builtin::apps::App;
use crate::error::Result;
use crate::registry::Command;
use crate::tv::Tv;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// What the enabled plugins contribute of one kind (providers, apps, ...):
/// set by main (`install`) before anything looks; empty until then. It can
/// be set again (tests install every plugin's), the old list left as it was
/// for anyone still holding it.
pub struct Registry<T: 'static> {
    slot: std::sync::RwLock<Option<&'static [T]>>,
}

impl<T: 'static> Registry<T> {
    pub const fn new() -> Self {
        Registry { slot: std::sync::RwLock::new(None) }
    }

    pub fn install(&self, items: Vec<T>) {
        *self.slot.write().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Box::leak(items.into_boxed_slice()));
    }

    /// What was installed (None: nothing yet).
    pub fn get(&self) -> Option<&'static [T]> {
        *self.slot.read().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl<T: 'static> Default for Registry<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// Install what the enabled plugins (by `states`, alongside `all`) bring:
/// their apps, providers, pairings, panels, namers and actions. At the
/// start, and again when one is turned on or off (it applies at once).
/// Returns their actions.
pub fn install_enabled(all: &[Plugin], states: &[PluginState]) -> Vec<ActionDescriptor> {
    let on = || all.iter().zip(states).filter(|(_, s)| s.enabled).map(|(p, _)| p);
    crate::builtin::apps::install(on().flat_map(|p| (p.apps)()).collect());
    crate::provider::install(on().flat_map(|p| (p.providers)()).collect());
    crate::pairing::install(on().flat_map(|p| (p.pairings)()).collect());
    crate::panel::install(on().flat_map(|p| (p.panels)()).collect());
    crate::nowplaying::install_namers(on().flat_map(|p| (p.namers)()).collect());
    let actions: Vec<ActionDescriptor> = on().flat_map(|p| (p.actions)()).collect();
    crate::actions::install(actions.clone());
    actions
}

pub enum Enable {
    Always,
    Detect(fn(&Tv) -> Result<bool>),
}

pub struct Plugin {
    pub name: &'static str,
    pub desc: &'static str,
    pub enable: Enable,
    /// What it can do (see `crate::actions`): commands, menu items, ...
    pub actions: fn() -> Vec<ActionDescriptor>,
    /// Apps this plugin knows (added to the core list when it's enabled).
    pub apps: fn() -> Vec<App>,
    /// Content sources this plugin provides (live TV, YouTube, ...).
    pub providers: fn() -> Vec<&'static dyn crate::provider::Provider>,
    /// Services on the TV this plugin can pair with (JointSpace, ...).
    pub pairings: fn() -> Vec<&'static dyn crate::pairing::Pairing>,
    /// Panels it offers front ends (the Ambilight).
    pub panels: fn() -> Vec<crate::panel::Spec>,
    /// Ways it can tell which app a host package is showing.
    pub namers: fn() -> Vec<crate::nowplaying::Namer>,
    /// Its part of `tveee diagnose` (while it's enabled).
    pub diagnose: fn(&Tv) -> Vec<Finding>,
}

/// A line of `tveee diagnose`: ok (Some(true)), a problem (Some(false)), or
/// just so (None); what it's about, and the detail.
pub struct Finding {
    pub ok: Option<bool>,
    pub what: &'static str,
    pub detail: String,
}

fn no_findings(_: &Tv) -> Vec<Finding> {
    Vec::new()
}

fn no_pairings() -> Vec<&'static dyn crate::pairing::Pairing> {
    Vec::new()
}

fn no_providers() -> Vec<&'static dyn crate::provider::Provider> {
    Vec::new()
}

fn no_apps() -> Vec<App> {
    Vec::new()
}

impl Plugin {
    pub fn builtin(name: &'static str, desc: &'static str, actions: fn() -> Vec<ActionDescriptor>) -> Self {
        Plugin {
            name,
            desc,
            enable: Enable::Always,
            actions,
            apps: no_apps,
            providers: no_providers,
            pairings: no_pairings,
            panels: Vec::new,
            namers: Vec::new,
            diagnose: no_findings,
        }
    }

    pub fn detect(name: &'static str, desc: &'static str, detect: fn(&Tv) -> Result<bool>, actions: fn() -> Vec<ActionDescriptor>) -> Self {
        Plugin {
            name,
            desc,
            enable: Enable::Detect(detect),
            actions,
            apps: no_apps,
            providers: no_providers,
            pairings: no_pairings,
            panels: Vec::new,
            namers: Vec::new,
            diagnose: no_findings,
        }
    }

    /// Register these providers while the plugin is enabled.
    pub fn with_providers(mut self, providers: fn() -> Vec<&'static dyn crate::provider::Provider>) -> Self {
        self.providers = providers;
        self
    }

    /// Register these pairings while the plugin is enabled.
    pub fn with_pairings(mut self, pairings: fn() -> Vec<&'static dyn crate::pairing::Pairing>) -> Self {
        self.pairings = pairings;
        self
    }

    /// Offer these panels while the plugin is enabled.
    pub fn with_panels(mut self, panels: fn() -> Vec<crate::panel::Spec>) -> Self {
        self.panels = panels;
        self
    }

    /// Name what these host packages show while the plugin is enabled.
    pub fn with_namers(mut self, namers: fn() -> Vec<crate::nowplaying::Namer>) -> Self {
        self.namers = namers;
        self
    }

    /// Add this to `tveee diagnose` while the plugin is enabled.
    pub fn with_diagnose(mut self, diagnose: fn(&Tv) -> Vec<Finding>) -> Self {
        self.diagnose = diagnose;
        self
    }

    /// Add these apps to the app list while the plugin is enabled.
    pub fn with_apps(mut self, apps: fn() -> Vec<App>) -> Self {
        self.apps = apps;
        self
    }

    /// Its commands: the actions the command line reaches.
    pub fn commands(&self) -> Vec<Command> {
        (self.actions)().iter().filter_map(ActionDescriptor::command).collect()
    }

    /// Command names and aliases this plugin provides.
    pub fn command_names(&self) -> Vec<&'static str> {
        self.commands().iter().flat_map(|c| std::iter::once(c.name).chain(c.aliases.iter().copied())).collect()
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

#[derive(Clone, Debug)]
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
/// What decides vendor plugins before checking the TV: the choices saved in
/// tveee's options, with `TVEEE_PLUGINS` over them.
pub fn overrides() -> HashMap<String, bool> {
    let mut o = crate::options::plugin_choices();
    o.extend(parse_overrides(&std::env::var("TVEEE_PLUGINS").unwrap_or_default()));
    o
}

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
    s.lines().filter_map(|l| l.split_once('=')).filter_map(|(k, v)| Some((k.trim().to_string(), v.trim().parse().ok()?))).collect()
}

pub fn format_cache(cache: &HashMap<String, bool>) -> String {
    let mut lines: Vec<String> = cache.iter().map(|(k, v)| format!("{k}={v}")).collect();
    lines.sort();
    lines.join("\n") + "\n"
}

/// `$XDG_CACHE_HOME/tveee/<ip>.plugins` (or `~/.cache/...`).
pub fn cache_path(ip: &str) -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .or_else(|| std::env::var_os("LOCALAPPDATA").map(PathBuf::from))?;
    Some(base.join("tveee").join(format!("{ip}.plugins")))
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

pub fn resolve(plugins: &[Plugin], tv: &Tv, mode: Mode, overrides: &HashMap<String, bool>, cache_file: Option<&Path>) -> Vec<PluginState> {
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

/// Commands that never need app knowledge from vendor plugins.
const OFFLINE: &[&str] = &["help", "commands", "completion", "plugins", "findtv"];

/// Whether running `sub` is worth probing the TV for vendor plugins: it's one
/// of their commands, or it may look up apps a vendor plugin provides (so
/// `launch iplayer` works on a TV whose plugins haven't been checked yet).
/// Probes are cached, so this costs one adb call per plugin per TV.
pub fn needs_probe(plugins: &[Plugin], sub: &str, args: &[String]) -> bool {
    let detect = || plugins.iter().filter(|p| matches!(p.enable, Enable::Detect(_)));
    if detect().any(|p| p.command_names().contains(&sub)) {
        return true;
    }
    // `do <id>` with one of their actions
    if sub == "do" && args.first().is_some_and(|id| detect().any(|p| (p.actions)().iter().any(|a| a.id == id))) {
        return true;
    }
    let offline = OFFLINE.contains(&sub)
        || (sub == "apps" && args.iter().any(|a| a == "--all"))
        || (sub == "do" && args.first().is_some_and(|a| a == "--all"))
        || (sub == "ui" && args.iter().any(|a| a == "--keys"));
    !offline && detect().any(|p| !(p.apps)().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overrides_parse() {
        let o = parse_overrides("philips, -sony,,");
        assert!(o["philips"]);
        assert!(!o["sony"]);
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

    fn none() -> Vec<ActionDescriptor> {
        Vec::new()
    }

    fn yes(_: &Tv) -> Result<bool> {
        Ok(true)
    }

    fn tv() -> Tv {
        Tv { ip: "0.0.0.0".into() }
    }

    fn some_apps() -> Vec<App> {
        vec![crate::builtin::apps::app("x", "X", "com.x", crate::builtin::apps::Kind::Video)]
    }

    #[test]
    fn probes_only_when_useful() {
        fn vcmds() -> Vec<ActionDescriptor> {
            vec![ActionDescriptor::terminal("v.vendorcmd", "v", "", |_, _| Ok(())).cli("vendorcmd", "", "")]
        }
        let a = |v: &[&str]| v.iter().map(std::string::ToString::to_string).collect::<Vec<_>>();
        let quiet = [Plugin::detect("v", "", yes, vcmds)];
        assert!(needs_probe(&quiet, "vendorcmd", &[]));
        assert!(needs_probe(&quiet, "do", &a(&["v.vendorcmd"])));
        assert!(!needs_probe(&quiet, "do", &a(&["power.sleep"])));
        assert!(!needs_probe(&quiet, "launch", &a(&["x"])));
        let with_apps = [Plugin::detect("v", "", yes, vcmds).with_apps(some_apps)];
        assert!(needs_probe(&with_apps, "launch", &a(&["x"])));
        assert!(!needs_probe(&with_apps, "help", &[]));
        assert!(!needs_probe(&with_apps, "apps", &a(&["--all"])));
        assert!(needs_probe(&with_apps, "apps", &[]));
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

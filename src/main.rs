mod devtools;
#[cfg(feature = "engine")]
mod engine;
mod dotenv;
mod actions;
mod crypto;
mod error;
mod favourites;
mod http;
mod lineups;
mod https;
mod keys;
mod labels;
mod log;
mod mdns;
mod more;
mod nowplaying;
mod options;
mod plugin;
#[cfg(feature = "engine")]
mod preview;
mod pairing;
mod plugins;
mod provider;
mod registry;
mod store;
mod tv;
mod watch;
mod ws;

use error::Error;
use plugin::{Mode, PluginState};
use registry::Command;
use tv::Tv;

pub struct Ctx {
    pub tv: Tv,
    /// Commands of the enabled plugins only.
    pub commands: Vec<Command>,
    /// Actions of the enabled plugins, in plugin order.
    pub actions: Vec<actions::ActionDescriptor>,
    /// Those with a menu key.
    #[cfg_attr(not(feature = "ui"), allow(dead_code))]
    pub menu: Vec<actions::ActionDescriptor>,
    pub plugins: Vec<PluginState>,
}

fn main() {
    // before anything reads TV_IP etc. (and before any threads)
    dotenv::load();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let tv = Tv::from_env();
    let all = plugins::all();

    let sub = match args.first().map(String::as_str) {
        None | Some("-h") | Some("--help") => "help",
        Some(s) => s,
    };
    let rest = args.get(1..).unwrap_or_default();
    let rest = if sub == "help" && args.first().is_some_and(|a| a.starts_with('-')) { &[][..] } else { rest };
    let help = sub != "help" && registry::wants_help(rest);

    // Only touch the TV to detect plugins when one of their commands was asked
    // for (and never just to show help).
    let mode = if !help && plugin::needs_probe(&all, sub, rest) { Mode::Probe } else { Mode::Cached };
    let overrides = plugin::parse_overrides(&std::env::var("TVEEE_PLUGINS").unwrap_or_default());
    let cache_file = plugin::cache_path(&tv.ip);
    let states = plugin::resolve(&all, &tv, mode, &overrides, cache_file.as_deref());

    let commands: Vec<Command> = all
        .iter()
        .zip(&states)
        .filter(|(_, s)| s.enabled)
        .flat_map(|(p, _)| p.commands())
        .collect();
    // the enabled plugins' apps join the core list, before anything looks one up
    plugins::apps::install(all.iter().zip(&states).filter(|(_, s)| s.enabled).flat_map(|(p, _)| (p.apps)()).collect());
    provider::install(all.iter().zip(&states).filter(|(_, s)| s.enabled).flat_map(|(p, _)| (p.providers)()).collect());
    pairing::install(all.iter().zip(&states).filter(|(_, s)| s.enabled).flat_map(|(p, _)| (p.pairings)()).collect());
    let actions: Vec<actions::ActionDescriptor> = all
        .iter()
        .zip(&states)
        .filter(|(_, s)| s.enabled)
        .flat_map(|(p, _)| (p.actions)())
        .collect();
    let menu = actions.iter().filter(|a| a.key.is_some()).copied().collect();
    let ctx = Ctx { tv, commands, actions, menu, plugins: states };

    // `tveee <cmd> --help`: any command, enabled or not, without the TV
    if help {
        let every: Vec<Command> = all.iter().flat_map(|p| p.commands()).collect();
        if let Some(cmd) = registry::find(&every, sub) {
            print!("{}", registry::help_for(cmd));
            let disabled = all.iter().find(|p| p.command_names().contains(&sub));
            if let (None, Some(p)) = (registry::find(&ctx.commands, sub), disabled) {
                println!("\nnote: this comes from the '{}' plugin, which isn't enabled for this TV (see 'tveee plugins').", p.name);
            }
            return;
        }
    }

    let Some(cmd) = registry::find(&ctx.commands, sub) else {
        // a command from a plugin that isn't enabled?
        if let Some((p, s)) = all
            .iter()
            .zip(&ctx.plugins)
            .find(|(p, s)| !s.enabled && p.command_names().contains(&sub))
        {
            eprintln!(
                "tveee: '{sub}' is provided by the '{}' plugin, which is not enabled for this TV ({}).\nTry 'tveee plugins --refresh', or set TVEEE_PLUGINS={}.",
                p.name,
                s.source.label(),
                p.name
            );
            std::process::exit(1);
        }
        if let Some((_, feature)) = plugins::NOT_BUILT.iter().find(|(c, _)| *c == sub) {
            eprintln!(
                "tveee: '{sub}' isn't in this build; it needs the '{feature}' feature:\n  cargo install --path . --features {feature}"
            );
            std::process::exit(1);
        }
        match registry::suggest(&ctx.commands, sub) {
            Some(close) => eprintln!("tveee: unknown command '{sub}' - did you mean '{close}'? (see 'tveee help')"),
            None => {
                eprintln!("tveee: unknown command '{sub}'");
                eprint!("{}", registry::usage_text(&ctx.commands));
            }
        }
        std::process::exit(2);
    };
    if let Err(e) = cmd.run.call(&ctx, rest) {
        eprintln!("tveee: {e}");
        if let Error::Usage(_) = e {
            eprintln!("(see 'tveee {} --help')", cmd.name);
        }
        std::process::exit(match e {
            Error::Usage(_) => 2,
            Error::Failed(_) | Error::Unavailable(_) => 1,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// The README's command listing must match `tveee help` for the default
    /// build (built-in plugins only, so `ui` and vendor plugins are left out).
    #[test]
    fn readme_lists_the_commands() {
        // tolerate a CRLF checkout (Windows)
        let readme = include_str!("../README.md").replace("\r\n", "\n");
        let start = "<!-- commands:start (checked by a test: cargo test readme) -->\n```text\n";
        let end = "```\n<!-- commands:end -->";
        let from = readme.find(start).expect("README: commands:start marker") + start.len();
        let to = from + readme[from..].find(end).expect("README: commands:end marker");
        let cmds: Vec<Command> = plugins::all()
            .iter()
            .filter(|p| matches!(p.enable, plugin::Enable::Always) && p.name != "ui")
            .flat_map(|p| p.commands())
            .collect();
        let want = registry::usage_text(&cmds);
        assert!(
            readme[from..to] == want,
            "README's command list is out of date; replace the block between the commands markers with:\n\n{want}"
        );
    }

    #[test]
    fn menu_keys_are_unique() {
        let mut seen = HashSet::new();
        for m in plugins::all().iter().flat_map(|p| (p.actions)()).filter(|a| a.key.is_some()) {
            assert!(seen.insert(m.key), "duplicate menu key: {:?}", m.key);
        }
    }

    #[test]
    fn action_ids_are_unique_and_namespaced() {
        let mut seen = HashSet::new();
        for p in plugins::all() {
            for a in (p.actions)() {
                assert!(seen.insert(a.id), "duplicate action id: {}", a.id);
                assert!(a.id.split_once('.').is_some_and(|(ns, rest)| !ns.is_empty() && !rest.is_empty()), "action id isn't <plugin>.<what>: {}", a.id);
            }
        }
    }

    #[test]
    fn names_and_aliases_are_unique() {
        let mut seen = HashSet::new();
        for c in plugins::all().iter().flat_map(|p| p.commands()) {
            for n in std::iter::once(c.name).chain(c.aliases.iter().copied()) {
                assert!(seen.insert(n), "duplicate command or alias: {n}");
            }
        }
    }
}

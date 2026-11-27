use crate::error::{failed, usage, Result};
use crate::plugin::{self, Mode};
use crate::actions::ActionDescriptor;
use crate::registry;
use crate::Ctx;
use std::io::Write;
use std::process::{Command as Proc, Stdio};
use std::time::Duration;

const CAT: &str = "tools";


const COMPLETION: &str = r#"# source this from ~/.bashrc, or ~/.zshrc after: autoload -U +X bashcompinit && bashcompinit
_tveee() {
  local cur=${COMP_WORDS[COMP_CWORD]} prev=${COMP_WORDS[COMP_CWORD-1]}
  local cmds apps
  cmds=$(tveee commands 2>/dev/null)
  apps=$(tveee apps --all 2>/dev/null | awk '{print $1}')
  case $prev in
    launch|foreground|fg|kill|--free|--quiet) COMPREPLY=($(compgen -W "$apps --free --quiet" -- "$cur")) ;;
    input) COMPREPLY=($(compgen -W "$(tveee inputs --all 2>/dev/null | awk '{print $1}')" -- "$cur")) ;;
    vol|channel|ch) COMPREPLY=($(compgen -W "up down" -- "$cur")) ;;
    *) COMPREPLY=($(compgen -W "$cmds" -- "$cur")) ;;
  esac
}
complete -F _tveee tveee
"#;

/// Hand over to `cmd`: on Unix it replaces tveee (so signals and the
/// terminal go straight to it); elsewhere tveee runs it, waits, and exits
/// with its status.
pub fn run_replacing(cmd: &mut Proc) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        Err(cmd.exec().into())
    }
    #[cfg(not(unix))]
    {
        let status = cmd.status()?;
        std::process::exit(status.code().unwrap_or(1));
    }
}

/// Case-insensitive extended-regex filter via `grep -iE`. Where there is no
/// grep (Windows), falls back to a case-insensitive substring match.
pub fn grep_i(text: &str, pattern: &str) -> Result<String> {
    let spawned = Proc::new("grep")
        .args(["-iE", "--", pattern])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn();
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("note: grep not found; matching '{pattern}' as plain text, not a regex");
            return Ok(contains_i(text, pattern));
        }
        Err(e) => return Err(e.into()),
    };
    let mut stdin = child.stdin.take().expect("piped");
    let input = text.to_string();
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let out = child.wait_with_output()?;
    let _ = writer.join();
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Lines of `text` containing `needle`, ignoring case.
pub fn contains_i(text: &str, needle: &str) -> String {
    let needle = needle.to_lowercase();
    text.lines().filter(|l| l.to_lowercase().contains(&needle)).map(|l| format!("{l}\n")).collect()
}

fn wait(c: &Ctx, args: &[String]) -> Result<()> {
    let limit: u64 = match args.first() {
        None => 180,
        Some(s) => s.parse().or_else(|_| usage("usage: tveee wait [secs]"))?,
    };
    let mut waited = 0;
    while waited < limit {
        if c.tv.shell(&["getprop", "sys.boot_completed"]).is_ok_and(|s| s.trim() == "1")
        {
            println!("TV is up");
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(3));
        waited += 3;
    }
    failed(format!("wait: timed out after {limit}s"))
}

fn commands_list(c: &Ctx, _: &[String]) -> Result<()> {
    let mut names: Vec<&str> = c
        .commands
        .iter()
        .flat_map(|c| std::iter::once(c.name).chain(c.aliases.iter().copied()))
        .collect();
    names.sort();
    println!("{}", names.join("\n"));
    Ok(())
}

/// `tveee do [<id> [args...]]`: run any action by id, or list them.
fn do_action(c: &Ctx, args: &[String]) -> Result<()> {
    let Some(id) = args.first() else {
        let mut all: Vec<_> = c.actions.iter().collect();
        all.sort_by_key(|a| a.id);
        let call = |a: &crate::actions::ActionDescriptor| a.call_line(a.id);
        let width = all.iter().map(|a| call(a).len()).max().unwrap_or(0);
        for a in all {
            println!("{:<width$}  {}", call(a), a.title);
        }
        return Ok(());
    };
    match crate::actions::find(&c.actions, id) {
        Some(a) => a.run.call(a.fronts, c, &args[1..]),
        None => {
            // one of a plugin's that isn't enabled?
            let all = (c.all)();
            if let Some((p, s)) = all.iter().zip(&c.plugins).find(|(p, _)| (p.actions)().iter().any(|a| a.id == id)) {
                return failed(format!(
                    "do: '{id}' is in the '{}' plugin, which is not enabled for this TV ({}); see 'tveee plugins'",
                    p.name,
                    s.source.label()
                ));
            }
            let hint = crate::actions::suggest(&c.actions, id).map_or(String::new(), |s| format!(" - did you mean '{s}'?"));
            usage(format!("do: no action '{id}'{hint} (see 'tveee do')"))
        }
    }
}

fn completion(_: &Ctx, _: &[String]) -> Result<()> {
    print!("{COMPLETION}");
    Ok(())
}

fn help(c: &Ctx, args: &[String]) -> Result<()> {
    match args.first() {
        None => {
            print!("{}", registry::usage_text(&c.commands));
            let off: Vec<_> = c.plugins.iter().filter(|p| !p.enabled).collect();
            if !off.is_empty() {
                let names: Vec<_> = off.iter().map(|p| format!("{} ({})", p.name, p.source.label())).collect();
                println!("Plugins not enabled: {} - see 'tveee plugins'", names.join(", "));
            }
        }
        Some(name) => match registry::find(&c.commands, name) {
            Some(cmd) => print!("{}", registry::help_for(cmd)),
            None => {
                let hint = registry::suggest(&c.commands, name).map_or(String::new(), |s| format!(" - did you mean '{s}'?"));
                return usage(format!("help: unknown command '{name}'{hint}"));
            }
        },
    }
    Ok(())
}

fn plugins_cmd(c: &Ctx, args: &[String]) -> Result<()> {
    let refreshed;
    let states = match args.first().map(String::as_str) {
        None => &c.plugins,
        Some("--refresh") => {
            let overrides = plugin::parse_overrides(&std::env::var("TVEEE_PLUGINS").unwrap_or_default());
            refreshed = plugin::resolve(
                &(c.all)(),
                &c.tv,
                Mode::Refresh,
                &overrides,
                plugin::cache_path(&c.tv.ip).as_deref(),
            );
            &refreshed
        }
        Some(_) => return usage("usage: tveee plugins [--refresh]"),
    };
    for s in states {
        let on = if s.enabled { "on" } else { "off" };
        println!("{:<10} {:<4} {:<9} {}", s.name, on, s.source.label(), s.desc);
    }
    Ok(())
}

pub fn actions() -> Vec<ActionDescriptor> {
    vec![
        ActionDescriptor::terminal("tools.wait", CAT, "wait until the TV is reachable and booted", wait).cli("wait", "[secs]", "wait until the TV is reachable and booted")
            .long("Default timeout 180s. Useful as: tveee reboot && tveee wait && tveee launch iplayer"),
        ActionDescriptor::terminal("tools.plugins", CAT, "list plugins and whether they are enabled", plugins_cmd).cli("plugins", "[--refresh]", "list plugins and whether they are enabled").long(
            "Vendor plugins are enabled by detection, cached per TV in ~/.cache/tveee.\n  --refresh  re-probe the TV now (contacts it) and update the cache\nTVEEE_PLUGINS=philips,-other forces plugins on or off.",
        ),
        ActionDescriptor::terminal("tools.do", CAT, "run an action by id", do_action)
            .cli("do", "[<id> [args...]]", "run any action by its id (list them with no id)")
            .long(
                "Everything tveee can do has an id, including what only the ui menu offers\n\
                 (e.g. 'tveee do kodi.subtitles.off'). With no id, lists them, with the\n\
                 arguments each takes.",
            ),
        ActionDescriptor::terminal("tools.commands", CAT, "list all command names and aliases", commands_list).cli("commands", "", "list all command names and aliases"),
        ActionDescriptor::terminal("tools.completion", CAT, "print a bash/zsh completion script", completion).cli("completion", "", "print a bash/zsh completion script"),
        ActionDescriptor::terminal("tools.help", CAT, "show this help, or help for one command", help).cli("help", "[command]", "show this help, or help for one command"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_filter() {
        assert_eq!(contains_i("Foo bar\nbaz\nFOO\n", "foo"), "Foo bar\nFOO\n");
    }

    #[cfg(unix)]
    #[test]
    fn grep_filters_case_insensitively() {
        let out = grep_i("Foo bar\nbaz\nFOO\n", "foo").unwrap();
        assert_eq!(out, "Foo bar\nFOO\n");
    }
}

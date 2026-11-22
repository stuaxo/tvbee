use crate::error::{failed, usage, Result};
use crate::plugin::{self, Mode};
use crate::registry::{self, Command};
use crate::Ctx;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::process::{Command as Proc, Stdio};
use std::time::Duration;

const CAT: &str = "tools";

/// Where `debloat` looks for the script when TV_DEBLOAT isn't set.
const DEBLOAT_SCRIPT: &str = "scripts/tv-debloat.sh";

const COMPLETION: &str = r#"# source this from ~/.bashrc, or ~/.zshrc after: autoload -U +X bashcompinit && bashcompinit
_tvbee() {
  local cur=${COMP_WORDS[COMP_CWORD]} prev=${COMP_WORDS[COMP_CWORD-1]}
  local cmds apps
  cmds=$(tvbee commands 2>/dev/null)
  apps=$(tvbee apps --all 2>/dev/null | awk '{print $1}')
  case $prev in
    launch|foreground|fg|kill|--free|--quiet) COMPREPLY=($(compgen -W "$apps --free --quiet" -- "$cur")) ;;
    input) COMPREPLY=($(compgen -W "$(tvbee inputs --all 2>/dev/null | awk '{print $1}')" -- "$cur")) ;;
    vol|channel|ch) COMPREPLY=($(compgen -W "up down" -- "$cur")) ;;
    *) COMPREPLY=($(compgen -W "$cmds" -- "$cur")) ;;
  esac
}
complete -F _tvbee tvbee
"#;

/// Case-insensitive extended-regex filter via `grep -iE`.
pub fn grep_i(text: &str, pattern: &str) -> Result<String> {
    let mut child = Proc::new("grep")
        .args(["-iE", "--", pattern])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().expect("piped");
    let input = text.to_string();
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let out = child.wait_with_output()?;
    let _ = writer.join();
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn wait(c: &Ctx, args: &[String]) -> Result<()> {
    let limit: u64 = match args.first() {
        None => 180,
        Some(s) => s.parse().or_else(|_| usage("usage: tvbee wait [secs]"))?,
    };
    let mut waited = 0;
    while waited < limit {
        if c.tv.try_connect()
            && c.tv.shell(&["getprop", "sys.boot_completed"]).is_ok_and(|s| s.trim() == "1")
        {
            println!("TV is up");
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(3));
        waited += 3;
    }
    failed(format!("wait: timed out after {limit}s"))
}

/// The debloat script: `$TV_DEBLOAT` if set, else `scripts/tv-debloat.sh` in
/// the current directory. Err says where it looked.
pub fn find_debloat(env: Option<&str>, cwd: &std::path::Path) -> std::result::Result<std::path::PathBuf, String> {
    match env {
        Some(p) => {
            let p = std::path::PathBuf::from(p);
            if p.is_file() { Ok(p) } else { Err(format!("TV_DEBLOAT is set to {}, which doesn't exist", p.display())) }
        }
        None => {
            let p = cwd.join(DEBLOAT_SCRIPT);
            if p.is_file() {
                Ok(p)
            } else {
                Err(format!(
                    "tv-debloat.sh not found (looked for {}); run from a directory with {DEBLOAT_SCRIPT}, or set TV_DEBLOAT",
                    p.display()
                ))
            }
        }
    }
}

fn debloat(_: &Ctx, args: &[String]) -> Result<()> {
    let env = std::env::var("TV_DEBLOAT").ok();
    let cwd = std::env::current_dir()?;
    let script = find_debloat(env.as_deref(), &cwd).or_else(|why| failed(format!("debloat: {why}")))?;
    Err(Proc::new(script).args(args).exec().into())
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
                println!("Plugins not enabled: {} - see 'tvbee plugins'", names.join(", "));
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
            let overrides = plugin::parse_overrides(&std::env::var("TVBEE_PLUGINS").unwrap_or_default());
            refreshed = plugin::resolve(
                &crate::plugins::all(),
                &c.tv,
                Mode::Refresh,
                &overrides,
                plugin::cache_path(&c.tv.ip).as_deref(),
            );
            &refreshed
        }
        Some(_) => return usage("usage: tvbee plugins [--refresh]"),
    };
    for s in states {
        let on = if s.enabled { "on" } else { "off" };
        println!("{:<10} {:<4} {:<9} {}", s.name, on, s.source.label(), s.desc);
    }
    Ok(())
}

pub fn commands() -> Vec<Command> {
    vec![
        Command::new(CAT, "wait", "[secs]", "wait until the TV is reachable and booted", wait)
            .long("Default timeout 180s. Useful as: tvbee reboot && tvbee wait && tvbee launch iplayer"),
        Command::new(CAT, "debloat", "<args>", "run tv-debloat.sh", debloat)
            .long("Runs tv-debloat.sh with the given arguments. The script is TV_DEBLOAT if set,\nelse scripts/tv-debloat.sh in the current directory; tvbee doesn't ship one."),
        Command::new(CAT, "plugins", "[--refresh]", "list plugins and whether they are enabled", plugins_cmd).long(
            "Vendor plugins are enabled by detection, cached per TV in ~/.cache/tvbee.\n  --refresh  re-probe the TV now (contacts it) and update the cache\nTVBEE_PLUGINS=philips,-other forces plugins on or off.",
        ),
        Command::new(CAT, "commands", "", "list all command names and aliases", commands_list),
        Command::new(CAT, "completion", "", "print a bash/zsh completion script", completion),
        Command::new(CAT, "help", "[command]", "show this help, or help for one command", help),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_debloat_script() {
        let dir = std::env::temp_dir().join(format!("tvbee-debloat-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("scripts")).unwrap();
        // not there yet: says where it looked
        let err = find_debloat(None, &dir).unwrap_err();
        assert!(err.contains("scripts/tv-debloat.sh") && err.contains("TV_DEBLOAT"), "{err}");
        std::fs::write(dir.join(DEBLOAT_SCRIPT), "#!/bin/sh\n").unwrap();
        assert_eq!(find_debloat(None, &dir).unwrap(), dir.join(DEBLOAT_SCRIPT));
        // TV_DEBLOAT wins, but must exist
        let other = dir.join("other.sh");
        assert!(find_debloat(other.to_str(), &dir).unwrap_err().contains("doesn't exist"));
        std::fs::write(&other, "").unwrap();
        assert_eq!(find_debloat(other.to_str(), &dir).unwrap(), other);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn grep_filters_case_insensitively() {
        let out = grep_i("Foo bar\nbaz\nFOO\n", "foo").unwrap();
        assert_eq!(out, "Foo bar\nFOO\n");
    }
}

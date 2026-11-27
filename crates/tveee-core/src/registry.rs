//! Command registry. Each module under `src/` contributes the commands of
//! one category, and the help output groups them by that category.

use crate::error::Result;
use crate::Ctx;
use std::collections::BTreeMap;

pub use crate::actions::Run;

pub struct Command {
    pub category: &'static str,
    pub name: &'static str,
    pub usage: &'static str,
    pub desc: &'static str,
    pub long: &'static str,
    pub aliases: &'static [&'static str],
    /// The app it brings to the front, if it does (it takes `--no-front`).
    pub fronts: Option<&'static str>,
    pub run: Run,
}

impl Command {
    pub fn new(
        category: &'static str,
        name: &'static str,
        usage: &'static str,
        desc: &'static str,
        run: fn(&Ctx, &[String]) -> Result<()>,
    ) -> Self {
        Command { category, name, usage, desc, long: "", aliases: &[], fronts: None, run: Run::Terminal(run) }
    }

    pub fn long(mut self, long: &'static str) -> Self {
        self.long = long;
        self
    }

    pub fn aliases(mut self, aliases: &'static [&'static str]) -> Self {
        self.aliases = aliases;
        self
    }
}

/// Whether `args` ask for a command's help: `--help` anywhere before a `--`,
/// or `-h` as the first argument. (`-h` further on, or anything after `--`, is
/// left for the command: `mirror` passes its arguments to scrcpy, which has
/// its own `-h`, and `text` may be asked to type it.)
pub fn wants_help(args: &[String]) -> bool {
    args.first().is_some_and(|a| a == "-h") || args.iter().take_while(|a| *a != "--").any(|a| a == "--help")
}

pub fn find<'a>(cmds: &'a [Command], name: &str) -> Option<&'a Command> {
    cmds.iter().find(|c| c.name == name || c.aliases.contains(&name))
}

/// Edit distance: insertions, deletions, substitutions and swaps of
/// neighbouring letters each count 1 (so "staus" is 1 from "status").
pub fn distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut d = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            d[i][j] = (d[i - 1][j] + 1).min(d[i][j - 1] + 1).min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                d[i][j] = d[i][j].min(d[i - 2][j - 2] + 1);
            }
        }
    }
    d[a.len()][b.len()]
}

/// How many edits still count as "close": a third of the longer word (at
/// least 2), or half of it when the first three letters match, since a shared
/// start ("now...") is good evidence of the intended word.
pub fn close_enough(typed: &str, candidate: &str, dist: usize) -> bool {
    let longest = typed.chars().count().max(candidate.chars().count());
    let same_start = typed.len() >= 3 && candidate.len() >= 3 && typed.get(..3) == candidate.get(..3);
    let allowed = if same_start { longest / 2 } else { longest / 3 };
    dist <= allowed.max(2)
}

/// The command (by name) closest to a mistyped `name`, if any is close enough.
pub fn suggest<'a>(cmds: &'a [Command], name: &str) -> Option<&'a str> {
    let name = name.to_lowercase();
    cmds.iter()
        .flat_map(|c| std::iter::once(c.name).chain(c.aliases.iter().copied()).map(move |n| (c.name, n)))
        .map(|(cmd, n)| (distance(&name, n), cmd, n))
        .filter(|(d, _, n)| close_enough(&name, n, *d))
        .min_by_key(|(d, _, _)| *d)
        .map(|(_, cmd, _)| cmd)
}

/// Django-style listing: `[category]` headings, commands sorted beneath.
pub fn usage_text(cmds: &[Command]) -> String {
    let mut by_cat: BTreeMap<&str, Vec<&Command>> = BTreeMap::new();
    for c in cmds {
        by_cat.entry(c.category).or_default().push(c);
    }
    let mut out = String::from(
        "Usage: tveee <command> [args]\n\nType 'tveee <command> --help' (or 'tveee help <command>') for help on a command.\n",
    );
    for (cat, mut group) in by_cat {
        group.sort_by_key(|c| c.name);
        out.push_str(&format!("\n[{cat}]\n"));
        for c in group {
            let alias = if c.aliases.is_empty() {
                String::new()
            } else {
                format!(" (alias: {})", c.aliases.join(" "))
            };
            out.push_str(&format!("    {:<12} {}{}\n", c.name, c.desc, alias));
        }
    }
    out.push_str("\nEnv: TV_IP (default 192.168.1.19); also read from ./.env or ~/.config/tveee/env\n");
    out
}

pub fn help_for(c: &Command) -> String {
    let flag = if c.fronts.is_some() { format!(" [{}]", crate::actions::NO_FRONT) } else { String::new() };
    let mut out = format!("usage: tveee {} {}{flag}\n\n{}\n", c.name, c.usage, c.desc)
        .replace("  [", " [")
        .replace(" \n\n", "\n\n");
    if !c.long.is_empty() {
        out.push_str(&format!("\n{}\n", c.long));
    }
    if let Some(app) = c.fronts {
        out.push_str(&format!("\n{}: leave what's on the screen alone (it brings {app} to the front first)\n", crate::actions::NO_FRONT));
    }
    if !c.aliases.is_empty() {
        out.push_str(&format!("\naliases: {}\n", c.aliases.join(" ")));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nop(_: &Ctx, _: &[String]) -> Result<()> {
        Ok(())
    }

    fn sample() -> Vec<Command> {
        vec![
            Command::new("power", "sleep", "", "standby", nop),
            Command::new("apps", "launch", "<app>", "launch an app", nop).aliases(&["l"]),
            Command::new("apps", "free", "", "free ram", nop),
        ]
    }

    #[test]
    fn groups_by_category_sorted() {
        let t = usage_text(&sample());
        let apps = t.find("[apps]").unwrap();
        let power = t.find("[power]").unwrap();
        assert!(apps < power);
        assert!(t.find("free").unwrap() < t.find("launch").unwrap());
        assert!(t.contains("(alias: l)"));
    }

    #[test]
    fn suggests_close_commands() {
        let n = |name: &'static str| Command::new("x", name, "", "", nop);
        let cmds = vec![
            n("nowplaying").aliases(&["np"]),
            n("now"),
            n("status"),
            n("launch"),
            n("screenshot"),
            n("play"),
            n("playpause"),
            n("foreground").aliases(&["fg"]),
        ];
        assert_eq!(suggest(&cmds, "nowwatching"), Some("nowplaying"));
        assert_eq!(suggest(&cmds, "staus"), Some("status"));
        assert_eq!(suggest(&cmds, "Status"), Some("status"));
        assert_eq!(suggest(&cmds, "lanuch"), Some("launch"));
        assert_eq!(suggest(&cmds, "scrnshot"), Some("screenshot"));
        assert_eq!(suggest(&cmds, "plya"), Some("play"));
        assert_eq!(suggest(&cmds, "nwo"), Some("now"));
        assert_eq!(suggest(&cmds, "gf"), Some("foreground"));
        assert_eq!(suggest(&cmds, "xyzzy"), None);
        assert_eq!(suggest(&cmds, "reboot"), None);
        // a shared start alone isn't enough
        assert_eq!(suggest(&cmds, "nowhereatall"), None);
        assert_eq!(distance("staus", "status"), 1);
        assert_eq!(distance("lanuch", "launch"), 1);
    }

    #[test]
    fn spots_help_requests() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(wants_help(&a(&["--help"])));
        assert!(wants_help(&a(&["-h"])));
        assert!(wants_help(&a(&["youtube", "--help"])));
        assert!(!wants_help(&a(&[])));
        assert!(!wants_help(&a(&["--no-control", "-h"])));
        assert!(!wants_help(&a(&["--", "--help"])));
        assert!(!wants_help(&a(&["hello"])));
    }

    #[test]
    fn find_by_alias() {
        let s = sample();
        assert_eq!(find(&s, "l").unwrap().name, "launch");
        assert!(find(&s, "nope").is_none());
    }

    #[test]
    fn help_trims_empty_usage() {
        let s = sample();
        assert!(help_for(find(&s, "sleep").unwrap()).starts_with("usage: tveee sleep\n\nstandby"));
    }
}

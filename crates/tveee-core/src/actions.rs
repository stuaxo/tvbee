//! Actions: everything tveee can do, described once for every front end.
//!
//! An [`ActionDescriptor`] says what an action is called (its id and title),
//! where it's listed (its category: the help heading and the ui menu's), what
//! it needs (its [`Params`]), how the command line reaches it ([`Cli`]), its
//! key in interactive front ends, whether to ask first, and what runs it.
//! Plugins contribute them with `Plugin::with_actions`; the command line, the
//! `ui` menu (and later its command palette, or a GUI) all read the same list.
//!
//! Plain data, so plugins describe them whether or not the ui is built.

use crate::Ctx;
use crate::builtin::apps::Front;
use crate::error::Result;
use crate::registry::Command;
use crate::tv::Tv;

/// What an action gets to work with.
pub struct Cx<'a> {
    pub tv: &'a Tv,
    /// It may bring its app to the front (see
    /// [`ActionDescriptor::brings_to_front`]): false after `--no-front` on
    /// the command line. Always true in the ui.
    pub front: bool,
}

/// The command line's flag for an action that brings its app to the front:
/// leave what's in front alone.
pub const NO_FRONT: &str = "--no-front";

/// `args` without [`NO_FRONT`], and whether the app may come to the front.
/// Only for an action that brings one forward (`fronts`): others get their
/// arguments as they are.
pub fn front_flag(fronts: Option<Front>, args: &[String]) -> (bool, Vec<String>) {
    if fronts.is_none() {
        return (true, args.to_vec());
    }
    // not after "--": that's the action's own
    let end = args.iter().position(|a| a == "--").unwrap_or(args.len());
    let no = args[..end].iter().any(|a| a == NO_FRONT);
    let rest = args.iter().enumerate().filter(|(i, a)| *i >= end || *a != NO_FRONT).map(|(_, a)| a.clone()).collect();
    (!no, rest)
}

/// How the command line shows what an action says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Say {
    /// Its message on stdout (errors on stderr, and the exit code).
    Plain,
    /// Nothing on success (`-q`, `--quiet`, or `TVEEE_QUIET=1`): errors and
    /// the exit code still say when it failed.
    Quiet,
    /// One line of JSON on stdout: `{"ok":true,"message":"..."}`, or
    /// `{"ok":false,"error":"..."}` (`--json`).
    Json,
}

/// The command line's output flags taken out of `args` (not after `--`:
/// those are the action's own); `quiet_env`: `TVEEE_QUIET`'s value.
pub fn say_flags(args: &[String], quiet_env: Option<&str>) -> (Say, Vec<String>) {
    let end = args.iter().position(|a| a == "--").unwrap_or(args.len());
    let ours = |i: usize, a: &str| i < end && matches!(a, "-q" | "--quiet" | "--json");
    let json = args[..end].iter().any(|a| a == "--json");
    let quiet = args[..end].iter().any(|a| a == "-q" || a == "--quiet") || quiet_env.is_some_and(|v| !v.is_empty() && v != "0");
    let say = if json {
        Say::Json
    } else if quiet {
        Say::Quiet
    } else {
        Say::Plain
    };
    (say, args.iter().enumerate().filter(|(i, a)| !ours(*i, a)).map(|(_, a)| a.clone()).collect())
}

/// What an action did, to show (status line, or stdout for the CLI).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Output {
    pub text: String,
}

impl From<String> for Output {
    fn from(text: String) -> Self {
        Output { text }
    }
}

impl From<&str> for Output {
    fn from(text: &str) -> Self {
        Output { text: text.to_string() }
    }
}

/// Runs an action with its arguments: the CLI's words, or for a
/// [`Params::Text`] action in the ui, the line typed (as one argument).
pub type Do = fn(&Cx, &[String]) -> Result<Output>;

/// A command-line-only action that uses the terminal itself (prints tables,
/// streams output, runs scrcpy, ...).
pub type Terminal = fn(&Ctx, &[String]) -> Result<()>;

#[derive(Clone, Copy)]
pub enum Run {
    Do(Do),
    Terminal(Terminal),
    /// Something the interactive remote does itself (opens its guide, shows
    /// its log, ...): listed with the rest, run only there.
    Ui,
}

impl Run {
    /// Run it from the command line: what a [`Run::Do`] action says goes to
    /// stdout as [`Say`] has it (`-q`/`--quiet`, `--json`, else plain).
    /// `fronts`: the app it brings to the front, if it does (then
    /// `--no-front` is taken out and kept from it).
    pub fn call(self, fronts: Option<Front>, c: &Ctx, args: &[String]) -> Result<()> {
        let (front, args) = front_flag(fronts, args);
        match self {
            Run::Terminal(f) => f(c, &args),
            Run::Ui => crate::error::failed("that's done in the interactive remote ('tveee ui')"),
            Run::Do(f) => {
                let (say, args) = say_flags(&args, std::env::var("TVEEE_QUIET").ok().as_deref());
                let result = f(&Cx { tv: &c.tv, front }, &args);
                if let Some(line) = said(say, &result) {
                    println!("{line}");
                }
                result.map(drop)
            }
        }
    }
}

/// What the command line prints for an action's `result`, as `say` has it
/// (None: nothing). A failure's plain message is the caller's to print
/// (on stderr); in JSON it's on stdout too.
pub fn said(say: Say, result: &Result<Output>) -> Option<String> {
    match (say, result) {
        (Say::Plain, Ok(out)) if !out.text.is_empty() => Some(out.text.clone()),
        (Say::Json, Ok(out)) => Some(serde_json::json!({ "ok": true, "message": out.text }).to_string()),
        (Say::Json, Err(e)) => Some(serde_json::json!({ "ok": false, "error": e.to_string() }).to_string()),
        _ => None,
    }
}

/// What an action needs from an interactive front end before it can run.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Params {
    None,
    /// A line of text: `prompt` asks for it, `hint` is an example.
    Text {
        prompt: &'static str,
        hint: &'static str,
    },
    /// One of a few: each (what it says, the argument it runs with). A
    /// front end offers them as a list; run with no argument, the action
    /// does its default (the first, usually).
    Choice(&'static [(&'static str, &'static str)]),
}

/// How the command line reaches an action: `tveee <name> <usage>`.
#[derive(Clone, Copy)]
pub struct Cli {
    pub name: &'static str,
    pub usage: &'static str,
    pub desc: &'static str,
    pub long: &'static str,
    pub aliases: &'static [&'static str],
}

#[derive(Clone, Copy)]
pub struct ActionDescriptor {
    /// Unique, `<plugin>.<what>`: "power.sleep", "kodi.subtitles.off".
    pub id: &'static str,
    /// Heading it's listed under (help, menu).
    pub category: &'static str,
    /// What it does, as the menu says it.
    pub title: &'static str,
    pub params: Params,
    /// The arguments it takes, as usage text ("[next|off]"), for `tveee do`
    /// and `tveee app <app>`. Commands say theirs in [`Cli::usage`].
    pub args: &'static str,
    /// None: interactive front ends only.
    pub cli: Option<Cli>,
    /// Its key in the ui menu.
    pub key: Option<char>,
    /// Ask first, with this warning ("The TV will restart..."). Interactive
    /// front ends only: the command line runs it as asked.
    pub confirm: Option<&'static str>,
    /// Only for this kind of thing playing ("video": subtitles, audio
    /// tracks): the ui offers it on the now-playing card only then. None:
    /// whatever plays.
    pub media: Option<&'static str>,
    /// The app it brings to the front ("live TV"): the command line takes
    /// `--no-front` to leave what's in front alone.
    pub fronts: Option<Front>,
    /// More words it's found by in a palette (not shown): "url" for opening
    /// a link, "standby" for sleep. Its command-line name and aliases count
    /// too.
    pub words: &'static [&'static str],
    pub run: Run,
}

impl ActionDescriptor {
    pub const fn new(id: &'static str, category: &'static str, title: &'static str, run: Do) -> Self {
        ActionDescriptor {
            id,
            category,
            title,
            params: Params::None,
            args: "",
            cli: None,
            key: None,
            confirm: None,
            media: None,
            fronts: None,
            words: &[],
            run: Run::Do(run),
        }
    }

    /// A command-line action that prints for itself (see [`Terminal`]).
    pub const fn terminal(id: &'static str, category: &'static str, title: &'static str, run: Terminal) -> Self {
        ActionDescriptor {
            id,
            category,
            title,
            params: Params::None,
            args: "",
            cli: None,
            key: None,
            confirm: None,
            media: None,
            fronts: None,
            words: &[],
            run: Run::Terminal(run),
        }
    }

    /// Something the interactive remote does itself ([`Run::Ui`]), in the
    /// "ui" category.
    pub const fn ui(id: &'static str, title: &'static str) -> Self {
        let mut a = Self::terminal(id, "ui", title, |_, _| Ok(()));
        a.run = Run::Ui;
        a
    }

    /// Whether an interactive front end's palette lists it: it runs there
    /// (not one that prints for itself on the command line).
    pub fn in_palette(&self) -> bool {
        matches!(self.run, Run::Do(_) | Run::Ui)
    }

    /// More words a palette finds it by (see [`ActionDescriptor::words`]).
    pub const fn words(mut self, words: &'static [&'static str]) -> Self {
        self.words = words;
        self
    }

    /// Everything a palette matches what's typed against: its title, id,
    /// command-line name and aliases, and [`ActionDescriptor::words`].
    pub fn search_text(&self) -> String {
        let mut t = format!("{} {}", self.title, self.id);
        if let Some(c) = &self.cli {
            t.push(' ');
            t.push_str(c.name);
            for a in c.aliases {
                t.push(' ');
                t.push_str(a);
            }
        }
        for w in self.words {
            t.push(' ');
            t.push_str(w);
        }
        t
    }

    /// Reachable as `tveee <name>`, `desc` its one-line help.
    pub const fn cli(mut self, name: &'static str, usage: &'static str, desc: &'static str) -> Self {
        self.cli = Some(Cli { name, usage, desc, long: "", aliases: &[] });
        self
    }

    /// The command's longer help (after [`Self::cli`]).
    pub const fn long(mut self, long: &'static str) -> Self {
        if let Some(c) = &mut self.cli {
            c.long = long;
        }
        self
    }

    /// Other names for the command (after [`Self::cli`]).
    /// How it's called, for listings: `<id or word> <args> [--no-front]`.
    pub fn call_line(&self, name: &str) -> String {
        let flag = if self.fronts.is_some() { format!(" [{NO_FRONT}]") } else { String::new() };
        format!("{name} {}{flag}", self.args).replace("  [", " [").trim().to_string()
    }

    /// It brings `app` to the front first (as the TV needs, or to show
    /// what it did): the command line takes `--no-front` to leave what's in
    /// front alone, and its help says so. A [`Run::Do`] action reads
    /// [`Cx::front`].
    pub const fn brings_to_front(mut self, app: Front) -> Self {
        self.fronts = Some(app);
        self
    }

    /// Only offered on the ui's now-playing card while `media` plays
    /// ("video", "audio").
    pub const fn while_playing(mut self, media: &'static str) -> Self {
        self.media = Some(media);
        self
    }

    pub const fn aliases(mut self, aliases: &'static [&'static str]) -> Self {
        if let Some(c) = &mut self.cli {
            c.aliases = aliases;
        }
        self
    }

    /// The arguments it takes, as usage text.
    pub const fn args(mut self, args: &'static str) -> Self {
        self.args = args;
        self
    }

    pub const fn key(mut self, key: char) -> Self {
        self.key = Some(key);
        self
    }

    /// Ask for a line of text first (`prompt`, with an example `hint`).
    pub const fn text(mut self, prompt: &'static str, hint: &'static str) -> Self {
        self.params = Params::Text { prompt, hint };
        self
    }

    /// It takes one of `choices` (see [`Params::Choice`]).
    pub const fn choices(mut self, choices: &'static [(&'static str, &'static str)]) -> Self {
        self.params = Params::Choice(choices);
        self
    }

    pub const fn confirm(mut self, warning: &'static str) -> Self {
        self.confirm = Some(warning);
        self
    }

    /// What it takes, in words, for listings: its arguments, the text it
    /// asks for (with the example), or its choices; and whether it asks
    /// first. Empty: nothing.
    pub fn input_text(&self) -> String {
        let mut t = match self.params {
            Params::None if self.args.is_empty() => String::new(),
            Params::None => format!("args: {}", self.args),
            Params::Text { prompt, hint } => format!("asks \"{prompt}\" (e.g. {hint})"),
            Params::Choice(c) => format!("choose: {}", c.iter().map(|(t, _)| *t).collect::<Vec<_>>().join(" | ")),
        };
        if self.confirm.is_some() {
            t.push_str(if t.is_empty() { "asks to confirm" } else { "; asks to confirm" });
        }
        t
    }

    /// Run it with these arguments. A [`Run::Terminal`] action can't run
    /// this way (it needs the command line's context).
    pub fn run(&self, tv: &Tv, args: &[String]) -> Result<Output> {
        match self.run {
            Run::Do(f) => f(&Cx { tv, front: true }, args),
            Run::Terminal(_) => crate::error::failed(format!("{} only runs from the command line", self.title)),
            Run::Ui => crate::error::failed(format!("{} is done in the interactive remote itself", self.title)),
        }
    }

    /// Its command, if the command line has one.
    pub fn command(&self) -> Option<Command> {
        let c = self.cli?;
        Some(Command {
            category: self.category,
            name: c.name,
            usage: c.usage,
            desc: c.desc,
            long: c.long,
            aliases: c.aliases,
            fronts: self.fronts,
            run: self.run,
        })
    }
}

/// The action with this id.
static REGISTRY: crate::plugin::Registry<ActionDescriptor> = crate::plugin::Registry::new();

/// Set the enabled plugins' actions (see [`crate::plugin::install_enabled`]).
pub fn install(actions: Vec<ActionDescriptor>) {
    REGISTRY.install(actions);
}

/// The enabled plugins' actions, as last installed (None: not yet).
pub fn installed() -> Option<&'static [ActionDescriptor]> {
    REGISTRY.get()
}

pub fn find<'a>(actions: &'a [ActionDescriptor], id: &str) -> Option<&'a ActionDescriptor> {
    actions.iter().find(|a| a.id == id)
}

/// The action id closest to a mistyped `id`, if one is close enough.
pub fn suggest(actions: &[ActionDescriptor], id: &str) -> Option<&'static str> {
    actions
        .iter()
        .map(|a| (crate::registry::distance(id, a.id), a.id))
        .filter(|(d, n)| crate::registry::close_enough(id, n, *d))
        .min_by_key(|(d, _)| *d)
        .map(|(_, n)| n)
}

#[cfg(test)]
mod tests {

    #[test]
    fn output_flags() {
        let v = |a: &[&str]| a.iter().map(ToString::to_string).collect::<Vec<_>>();
        assert_eq!(say_flags(&v(&["up"]), None), (Say::Plain, v(&["up"])));
        assert_eq!(say_flags(&v(&["-q", "up"]), None), (Say::Quiet, v(&["up"])));
        assert_eq!(say_flags(&v(&["up", "--quiet"]), None), (Say::Quiet, v(&["up"])));
        assert_eq!(say_flags(&v(&["--json", "-q"]), None), (Say::Json, v(&[])));
        // after --: the action's own
        assert_eq!(say_flags(&v(&["--", "--json"]), None), (Say::Plain, v(&["--", "--json"])));
        // TVEEE_QUIET: quiet unless empty or 0
        assert_eq!(say_flags(&v(&[]), Some("1")).0, Say::Quiet);
        assert_eq!(say_flags(&v(&[]), Some("0")).0, Say::Plain);
        assert_eq!(say_flags(&v(&["--json"]), Some("1")).0, Say::Json);
    }

    #[test]
    fn what_is_said() {
        let ok: Result<Output> = Ok("muted".into());
        let quiet_ok: Result<Output> = Ok(Output::default());
        let bad: Result<Output> = crate::error::failed("the TV isn't answering");
        assert_eq!(crate::actions::said(Say::Plain, &ok).as_deref(), Some("muted"));
        assert_eq!(crate::actions::said(Say::Plain, &quiet_ok), None);
        assert_eq!(crate::actions::said(Say::Quiet, &ok), None);
        // a plain failure is said on stderr by the caller, not here
        assert_eq!(crate::actions::said(Say::Plain, &bad), None);
        assert_eq!(crate::actions::said(Say::Json, &ok).as_deref(), Some(r#"{"message":"muted","ok":true}"#));
        assert_eq!(crate::actions::said(Say::Json, &bad).as_deref(), Some(r#"{"error":"the TV isn't answering","ok":false}"#));
    }

    #[test]
    fn no_front_flag() {
        let a = |v: &[&str]| v.iter().map(std::string::ToString::to_string).collect::<Vec<_>>();
        // an action that brings its app forward: the flag is taken out
        assert_eq!(front_flag(Some(Front::Package("x")), &a(&["up"])), (true, a(&["up"])));
        assert_eq!(front_flag(Some(Front::Package("x")), &a(&["--no-front", "up"])), (false, a(&["up"])));
        assert_eq!(front_flag(Some(Front::Package("x")), &a(&["up", "--no-front"])), (false, a(&["up"])));
        // after "--" it's the action's own
        assert_eq!(front_flag(Some(Front::Package("x")), &a(&["--", "--no-front"])), (true, a(&["--", "--no-front"])));
        // one that doesn't: left as it is
        assert_eq!(front_flag(None, &a(&["--no-front"])), (true, a(&["--no-front"])));
        let d = ActionDescriptor::new("x.go", "x", "go", said).args("<where>").brings_to_front(Front::Package("x"));
        assert_eq!(d.call_line("go"), "go <where> [--no-front]");
        assert_eq!(ActionDescriptor::new("x.say", "x", "say it", said).call_line("say"), "say");
    }

    use super::*;

    fn said(_: &Cx, args: &[String]) -> Result<Output> {
        Ok(args.join(" ").into())
    }

    #[test]
    fn builds_a_command() {
        let a = ActionDescriptor::new("x.say", "x", "say it", said).cli("say", "<words>", "say the words").aliases(&["s"]).long("more");
        let c = a.command().expect("a command");
        assert_eq!((c.category, c.name, c.usage, c.desc, c.long, c.aliases), ("x", "say", "<words>", "say the words", "more", &["s"][..]));
        assert!(ActionDescriptor::new("x.quiet", "x", "no cli", said).command().is_none());
    }

    #[test]
    fn long_and_aliases_need_a_cli() {
        let a = ActionDescriptor::new("x.say", "x", "say it", said).long("ignored").aliases(&["s"]);
        assert!(a.cli.is_none());
    }

    #[test]
    fn runs_with_args() {
        let tv = Tv { ip: "127.0.0.1".into() };
        let a = ActionDescriptor::new("x.say", "x", "say it", said);
        assert_eq!(a.run(&tv, &["hi".into(), "there".into()]).unwrap().text, "hi there");
    }

    #[test]
    fn says_what_it_takes() {
        fn ok(_: &Cx, _: &[String]) -> Result<Output> {
            Ok(Output::default())
        }
        let a = ActionDescriptor::new("x.y", "x", "y", ok);
        assert_eq!(a.input_text(), "");
        assert_eq!(a.args("<n>").input_text(), "args: <n>");
        assert_eq!(a.text("Link:", "https://...").confirm("Sure?").input_text(), "asks \"Link:\" (e.g. https://...); asks to confirm");
        assert_eq!(a.choices(&[("on", "on"), ("off", "off")]).input_text(), "choose: on | off");
        assert_eq!(a.confirm("Sure?").input_text(), "asks to confirm");
    }
}

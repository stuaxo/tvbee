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

use crate::error::Result;
use crate::registry::Command;
use crate::tv::Tv;
use crate::Ctx;

/// What an action gets to work with.
pub struct Cx<'a> {
    pub tv: &'a Tv,
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
}

impl Run {
    /// Run it from the command line: what a [`Run::Do`] action says goes to
    /// stdout (when it says anything).
    pub fn call(self, c: &Ctx, args: &[String]) -> Result<()> {
        match self {
            Run::Terminal(f) => f(c, args),
            Run::Do(f) => {
                let out = f(&Cx { tv: &c.tv }, args)?;
                if !out.text.is_empty() {
                    println!("{}", out.text);
                }
                Ok(())
            }
        }
    }
}

/// What an action needs from an interactive front end before it can run.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Params {
    None,
    /// A line of text: `prompt` asks for it, `hint` is an example.
    Text { prompt: &'static str, hint: &'static str },
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
    /// None: interactive front ends only.
    pub cli: Option<Cli>,
    /// Its key in the ui menu.
    pub key: Option<char>,
    /// Ask first, with this warning ("The TV will restart..."). Interactive
    /// front ends only: the command line runs it as asked.
    pub confirm: Option<&'static str>,
    pub run: Run,
}

impl ActionDescriptor {
    pub const fn new(id: &'static str, category: &'static str, title: &'static str, run: Do) -> Self {
        ActionDescriptor { id, category, title, params: Params::None, cli: None, key: None, confirm: None, run: Run::Do(run) }
    }

    /// A command-line action that prints for itself (see [`Terminal`]).
    pub const fn terminal(id: &'static str, category: &'static str, title: &'static str, run: Terminal) -> Self {
        ActionDescriptor { id, category, title, params: Params::None, cli: None, key: None, confirm: None, run: Run::Terminal(run) }
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
    pub const fn aliases(mut self, aliases: &'static [&'static str]) -> Self {
        if let Some(c) = &mut self.cli {
            c.aliases = aliases;
        }
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

    pub const fn confirm(mut self, warning: &'static str) -> Self {
        self.confirm = Some(warning);
        self
    }

    /// Run it with these arguments. A [`Run::Terminal`] action can't run
    /// this way (it needs the command line's context).
    pub fn run(&self, tv: &Tv, args: &[String]) -> Result<Output> {
        match self.run {
            Run::Do(f) => f(&Cx { tv }, args),
            Run::Terminal(_) => crate::error::failed(format!("{} only runs from the command line", self.title)),
        }
    }

    /// Its command, if the command line has one.
    pub fn command(&self) -> Option<Command> {
        let c = self.cli?;
        Some(Command { category: self.category, name: c.name, usage: c.usage, desc: c.desc, long: c.long, aliases: c.aliases, run: self.run })
    }
}

/// The action with this id.
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
}

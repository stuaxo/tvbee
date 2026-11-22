//! Pairing: one-time trust set up between tvbee and a service on the TV
//! (Philips' JointSpace API today; others such as Android TV Remote, Sony or
//! LG APIs could follow). Plugins register pairings with
//! `Plugin::with_pairings`; `tvbee pair`, `unpair` and `pairings` drive them,
//! and anything that needs one calls [`require`] to get the saved
//! credentials or a "pairing required" error.
//!
//! Credentials are saved per pairing and TV in
//! `~/.config/tvbee/pairings/<id>-<ip>.txt` (readable only by you).

use crate::error::{failed, usage, Error, Result};
use crate::registry::Command;
use crate::tv::Tv;
use crate::Ctx;
use std::path::PathBuf;

/// What a pairing saved: `key=value` fields.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Saved(pub Vec<(String, String)>);

impl Saved {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    pub fn parse(text: &str) -> Saved {
        Saved(
            text.lines()
                .filter(|l| !l.trim_start().starts_with('#'))
                .filter_map(|l| l.split_once('='))
                .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
                .collect(),
        )
    }

    pub fn render(&self, header: &str) -> String {
        let mut s = format!("# {header}\n");
        for (k, v) in &self.0 {
            s.push_str(&format!("{k}={v}\n"));
        }
        s
    }
}

/// A service tvbee can pair with.
pub trait Pairing: Sync {
    /// Short id: `tvbee pair <id>`.
    fn id(&self) -> &'static str;
    fn name(&self) -> &'static str;
    /// What pairing makes possible, for `tvbee pairings` and errors.
    fn unlocks(&self) -> &'static str;
    /// What the user will have to do ("the TV shows a PIN; type it here").
    fn how(&self) -> &'static str;
    /// Do the pairing (talking to the user as needed); returns what to save.
    fn pair(&self, tv: &Tv) -> Result<Saved>;
    /// After saving: a quick read showing it works, as a line to print.
    fn check(&self, _tv: &Tv, _saved: &Saved) -> Option<String> {
        None
    }
}

static REGISTRY: std::sync::OnceLock<Vec<&'static dyn Pairing>> = std::sync::OnceLock::new();

/// Set the pairings of the enabled plugins (main does).
pub fn install(pairings: Vec<&'static dyn Pairing>) {
    let _ = REGISTRY.set(pairings);
}

/// Every pairing. Without [`install`] (tests) that's every plugin's.
pub fn all() -> &'static [&'static dyn Pairing] {
    REGISTRY.get_or_init(|| crate::plugins::all().iter().flat_map(|p| (p.pairings)()).collect())
}

pub fn by_id(id: &str) -> Option<&'static dyn Pairing> {
    all().iter().copied().find(|p| p.id() == id)
}

fn file(id: &str, ip: &str) -> Option<PathBuf> {
    Some(crate::dotenv::config_dir()?.join("pairings").join(format!("{id}-{ip}.txt")))
}

/// The saved credentials of pairing `id` with the TV at `ip`.
pub fn load(id: &str, ip: &str) -> Option<Saved> {
    let saved = Saved::parse(&std::fs::read_to_string(file(id, ip)?).ok()?);
    (!saved.0.is_empty()).then_some(saved)
}

fn save(id: &str, ip: &str, saved: &Saved) -> Result<PathBuf> {
    let Some(path) = file(id, ip) else { return failed("pair: no config folder to save the pairing in") };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, saved.render(&format!("{id} pairing with {ip} (tvbee pair {id})")))?;
    // it's a password for the TV
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(path)
}

/// The "pairing required" error for `what` (e.g. "the channel list").
pub fn required_error(id: &str, what: &str) -> Error {
    let how = by_id(id).map_or(String::new(), |p| format!(" ({})", p.how()));
    let name = by_id(id).map_or(id, |p| p.name());
    Error::Failed(format!("{what} needs tvbee to be paired with {name}: run 'tvbee pair {id}'{how}"))
}

/// The saved credentials for `id`, or the "pairing required" error.
pub fn require(id: &str, tv: &Tv, what: &str) -> Result<Saved> {
    load(id, &tv.ip).ok_or_else(|| required_error(id, what))
}

fn pair(c: &Ctx, args: &[String]) -> Result<()> {
    let p = match args {
        [id] => by_id(id).map_or_else(|| usage(format!("pair: no pairing called '{id}' here (see 'tvbee pairings')")), Ok)?,
        [] => match all() {
            [one] => *one,
            [] => return failed("pair: nothing to pair with on this TV (see 'tvbee pairings')"),
            _ => return usage("pair: say which: tvbee pair <id> (see 'tvbee pairings')"),
        },
        _ => return usage("usage: tvbee pair [<id>]"),
    };
    if load(p.id(), &c.tv.ip).is_some() {
        println!("(already paired with {} on {}; pairing again replaces it)", p.name(), c.tv.ip);
    }
    let saved = p.pair(&c.tv)?;
    let path = save(p.id(), &c.tv.ip, &saved)?;
    println!("paired with {}; saved in {}", p.name(), path.display());
    if let Some(line) = p.check(&c.tv, &saved) {
        println!("{line}");
    }
    Ok(())
}

fn unpair(c: &Ctx, args: &[String]) -> Result<()> {
    let [id] = args else { return usage("usage: tvbee unpair <id>") };
    match file(id, &c.tv.ip).filter(|f| f.exists()) {
        Some(f) => {
            std::fs::remove_file(f)?;
            println!("forgot the {id} pairing with {} (the TV may still list tvbee; remove it there if you like)", c.tv.ip);
            Ok(())
        }
        None => failed(format!("unpair: not paired with {id} on {}", c.tv.ip)),
    }
}

fn pairings(c: &Ctx, _: &[String]) -> Result<()> {
    if all().is_empty() {
        println!("nothing to pair with on this TV");
    }
    for p in all() {
        let state = if load(p.id(), &c.tv.ip).is_some() { "paired" } else { "not paired" };
        println!("{:<11} {:<11} {}: {}", p.id(), state, p.name(), p.unlocks());
    }
    Ok(())
}

pub fn commands() -> Vec<Command> {
    vec![
        Command::new("pairing", "pair", "[<id>]", "pair tvbee with a service on the TV", pair).long(
            "Some TV services need a one-time pairing before tvbee can use them (on\n\
             Philips TVs, the JointSpace API: the TV shows a PIN, you type it here).\n\
             With one pairing available, the id can be left out. 'tvbee pairings' lists\n\
             them. Saved in ~/.config/tvbee/pairings/ (readable only by you).",
        ),
        Command::new("pairing", "unpair", "<id>", "forget a saved pairing", unpair),
        Command::new("pairing", "pairings", "", "services tvbee can pair with, and whether it has", pairings),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_round_trip() {
        let s = Saved(vec![("user".into(), "abc".into()), ("key".into(), "x=y".into())]);
        let text = s.render("test");
        assert!(text.starts_with("# test\n"));
        assert_eq!(Saved::parse(&text), s);
        assert_eq!(s.get("key"), Some("x=y"));
        assert_eq!(s.get("nope"), None);
    }

    #[test]
    fn registry_and_errors() {
        assert!(by_id("jointspace").is_some());
        let e = required_error("jointspace", "the channel list").to_string();
        assert!(e.starts_with("the channel list needs tvbee to be paired with") && e.contains("tvbee pair jointspace"), "{e}");
    }
}

//! Pairing: one-time trust set up between tveee and a service on the TV
//! (Philips' JointSpace API today; others such as Android TV Remote, Sony or
//! LG APIs could follow). Plugins register pairings with
//! `Plugin::with_pairings`; `tveee pair`, `unpair` and `pairings` drive them,
//! and anything that needs one calls [`require`] to get the saved
//! credentials or a "pairing required" error.
//!
//! Credentials are saved per pairing and TV in
//! `~/.config/tveee/pairings/<id>-<ip>.txt` (readable only by you).

use crate::error::{failed, usage, Error, Result};
use crate::actions::ActionDescriptor;
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

/// Finishing a pairing a front end started, with what the user typed (the
/// PIN the TV shows); returns what to save.
pub type Finish = Box<dyn FnOnce(&Tv, &str) -> Result<Saved> + Send>;

/// A service tveee can pair with.
pub trait Pairing: Sync {
    /// Short id: `tveee pair <id>`.
    fn id(&self) -> &'static str;
    fn name(&self) -> &'static str;
    /// What pairing makes possible, for `tveee pairings` and errors.
    fn unlocks(&self) -> &'static str;
    /// What the user will have to do ("the TV shows a PIN; type it here").
    fn how(&self) -> &'static str;
    /// Do the pairing (talking to the user as needed); returns what to save.
    fn pair(&self, tv: &Tv) -> Result<Saved>;
    /// Only needed sometimes (Kodi: with a password): `diagnose` doesn't
    /// list it as missing; the plugin's own check says if it's needed.
    fn optional(&self) -> bool {
        false
    }
    /// For front ends: start pairing (the TV shows a PIN), to finish with
    /// what the user types. None: it can't be done that way.
    fn start(&self, _tv: &Tv) -> Option<Result<Finish>> {
        None
    }
    /// After saving: a quick read showing it works, as a line to print.
    fn check(&self, _tv: &Tv, _saved: &Saved) -> Option<String> {
        None
    }
}

static REGISTRY: crate::plugin::Registry<&'static dyn Pairing> = crate::plugin::Registry::new();

/// Set the pairings of the enabled plugins (main does).
pub fn install(pairings: Vec<&'static dyn Pairing>) {
    REGISTRY.install(pairings);
}

/// Every pairing of the enabled plugins (none before [`install`]).
pub fn all() -> &'static [&'static dyn Pairing] {
    REGISTRY.get().unwrap_or_default()
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

pub fn save(id: &str, ip: &str, saved: &Saved) -> Result<PathBuf> {
    let Some(path) = file(id, ip) else { return failed("pair: no config folder to save the pairing in") };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, saved.render(&format!("{id} pairing with {ip} (tveee pair {id})")))?;
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
    Error::Failed(format!("{what} needs tveee to be paired with {name}: run 'tveee pair {id}'{how}"))
}

/// The saved credentials for `id`, or the "pairing required" error.
pub fn require(id: &str, tv: &Tv, what: &str) -> Result<Saved> {
    load(id, &tv.ip).ok_or_else(|| required_error(id, what))
}

fn pair(c: &Ctx, args: &[String]) -> Result<()> {
    let p = match args {
        [id] => by_id(id).map_or_else(|| usage(format!("pair: no pairing called '{id}' here (see 'tveee pairings')")), Ok)?,
        [] => match all() {
            [one] => *one,
            [] => return failed("pair: nothing to pair with on this TV (see 'tveee pairings')"),
            _ => return usage("pair: say which: tveee pair <id> (see 'tveee pairings')"),
        },
        _ => return usage("usage: tveee pair [<id>]"),
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
    let [id] = args else { return usage("usage: tveee unpair <id>") };
    match file(id, &c.tv.ip).filter(|f| f.exists()) {
        Some(f) => {
            std::fs::remove_file(f)?;
            println!("forgot the {id} pairing with {} (the TV may still list tveee; remove it there if you like)", c.tv.ip);
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

pub fn actions() -> Vec<ActionDescriptor> {
    vec![
        ActionDescriptor::terminal("pairing.pair", "pairing", "pair tveee with a service on the TV", pair).cli("pair", "[<id>]", "pair tveee with a service on the TV").long(
            "Some TV services need a one-time pairing before tveee can use them (on\n\
             Philips TVs, the JointSpace API: the TV shows a PIN, you type it here).\n\
             With one pairing available, the id can be left out. 'tveee pairings' lists\n\
             them. Saved in ~/.config/tveee/pairings/ (readable only by you).",
        ),
        ActionDescriptor::terminal("pairing.unpair", "pairing", "forget a saved pairing", unpair).cli("unpair", "<id>", "forget a saved pairing"),
        ActionDescriptor::terminal("pairing.pairings", "pairing", "services tveee can pair with, and whether it has", pairings).cli("pairings", "", "services tveee can pair with, and whether it has"),
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

}

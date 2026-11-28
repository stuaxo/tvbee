//! Kodi's add-ons: listed (installed, or in its repositories), their page
//! opened, and installed.
//!
//! Kodi's API can list add-ons but has no "install": tveee opens the
//! add-on's page in Kodi (its add-on browser, searched for the add-on) and
//! presses Install there, as someone with the remote would. Anything else
//! Kodi asks on the way is left on screen and said.

use super::{PACKAGE, Result, Tv, call, failed, str_of};
use serde_json::Value;
use std::time::{Duration, Instant};

/// The kinds listed: plugins (video, music, pictures, programs). Skins,
/// scrapers and the like are add-ons too, but not what's meant here.
const TYPE: &str = "xbmc.python.pluginsource";

/// Kodi's windows, as `GUI.GetProperties` names them by id.
const HOME: u64 = 10000;
const BROWSER: u64 = 10040;
const INFO: u64 = 10146;
/// Progress dialogs: fine while installing.
const PROGRESS: [u64; 2] = [10101, 10151];

#[derive(Debug, Clone, PartialEq)]
pub struct Addon {
    pub id: String,
    pub name: String,
    pub version: String,
    pub summary: String,
    pub enabled: bool,
}

/// The add-ons in a `Addons.GetAddons` result.
pub fn parse(v: &Value) -> Vec<Addon> {
    v.get("addons")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| {
                    Some(Addon {
                        id: str_of(x, "addonid")?,
                        name: str_of(x, "name").unwrap_or_default(),
                        version: str_of(x, "version").unwrap_or_default(),
                        summary: str_of(x, "summary").unwrap_or_default(),
                        enabled: x.get("enabled").and_then(Value::as_bool).unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Kodi's plugins: those installed, or those its repositories offer that
/// aren't.
pub fn list(tv: &Tv, installed: bool) -> Result<Vec<Addon>> {
    let params = format!(r#"{{"type":"{TYPE}","installed":{installed},"properties":["name","version","summary","enabled"]}}"#);
    let mut found = parse(&call(tv, "Addons.GetAddons", &params)?);
    found.sort_by_key(|a| a.name.to_lowercase());
    Ok(found)
}

/// Those whose id, name or summary has every word of `q` (any case).
pub fn matching<'a>(all: &'a [Addon], q: &str) -> Vec<&'a Addon> {
    let words: Vec<String> = q.split_whitespace().map(str::to_lowercase).collect();
    all.iter()
        .filter(|a| {
            let text = format!("{} {} {}", a.id, a.name, a.summary).to_lowercase();
            words.iter().all(|w| text.contains(w.as_str()))
        })
        .collect()
}

/// The one add-on `q` means: its id, or its name (any case), else the only
/// match.
pub fn pick<'a>(all: &'a [Addon], q: &str) -> std::result::Result<&'a Addon, Vec<&'a Addon>> {
    let q = q.trim();
    if let Some(a) = all.iter().find(|a| a.id.eq_ignore_ascii_case(q) || a.name.eq_ignore_ascii_case(q)) {
        return Ok(a);
    }
    match matching(all, q).as_slice() {
        [one] => Ok(one),
        many => Err(many.to_vec()),
    }
}

/// A line per add-on: name, id and version, what it is.
pub fn lines(found: &[&Addon]) -> String {
    let w = found.iter().map(|a| a.name.chars().count()).max().unwrap_or(0).min(32);
    found
        .iter()
        .map(|a| {
            let off = if a.enabled { "" } else { "  (disabled)" };
            let summary: String = a.summary.chars().take(60).collect();
            format!("{:<w$}  {} {}{off}  {summary}", a.name, a.id, a.version)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `tveee kodi-addons [installed|available] [words]`.
pub fn list_cmd(tv: &Tv, args: &[String]) -> Result<String> {
    let (installed, rest) = match args.first().map(String::as_str) {
        Some("available") => (false, &args[1..]),
        Some("installed") => (true, &args[1..]),
        _ => (true, args),
    };
    let q = rest.join(" ");
    let all = list(tv, installed)?;
    let found = matching(&all, &q);
    let which = if installed { "installed" } else { "to install" };
    if found.is_empty() {
        return Ok(if q.is_empty() { format!("no Kodi add-ons {which}") } else { format!("no Kodi add-ons {which} match \"{q}\"") });
    }
    let more = if installed { "\n(tveee kodi-addons available [words]: the ones you can install)" } else { "\n(tveee kodi-install <name>: install one)" };
    Ok(format!("{}{more}", lines(&found)))
}

/// Which window Kodi shows, and the label of what's focused in it.
fn window(tv: &Tv) -> Result<(u64, String)> {
    let r = call(tv, "GUI.GetProperties", r#"{"properties":["currentwindow","currentcontrol"]}"#)?;
    let id = r.pointer("/currentwindow/id").and_then(Value::as_u64).unwrap_or(0);
    let focus = r.pointer("/currentcontrol/label").and_then(Value::as_str).unwrap_or("").to_string();
    Ok((id, focus))
}

fn label(tv: &Tv, name: &str) -> Result<String> {
    let r = super::call_json(tv, "XBMC.GetInfoLabels", serde_json::json!({ "labels": [name] }))?;
    Ok(r.get(name).and_then(Value::as_str).unwrap_or("").to_string())
}

/// Wait up to `secs` for `ok` to say yes about the window shown.
fn wait_for(tv: &Tv, secs: u64, ok: impl Fn(u64, &str) -> bool) -> Result<(u64, String)> {
    let until = Instant::now() + Duration::from_secs(secs);
    loop {
        let (id, focus) = window(tv)?;
        if ok(id, &focus) || Instant::now() >= until {
            return Ok((id, focus));
        }
        std::thread::sleep(Duration::from_millis(400));
    }
}

/// What Kodi shows, said for someone who can't see it.
fn showing(tv: &Tv) -> String {
    let (id, focus) = window(tv).unwrap_or_default();
    let name = label(tv, "System.CurrentWindow").unwrap_or_default();
    let heading = label(tv, "Control.GetLabel(1)").unwrap_or_default();
    let mut s = format!("Kodi is showing {}", if name.is_empty() { format!("window {id}") } else { name });
    if !heading.is_empty() {
        s += &format!(" (\"{heading}\")");
    }
    if !focus.is_empty() {
        s += &format!(", on \"{focus}\"");
    }
    s
}

/// Open `a`'s page in Kodi: the add-on browser, searched for its name, on
/// its row, then its page. Returns the focused button's label there.
fn open_page(tv: &Tv, a: &Addon) -> Result<String> {
    let _ = tveee_core::builtin::apps::bring_to_front(tv, PACKAGE);
    let path = format!("addons://search/{}/", a.name);
    super::call_json(tv, "GUI.ActivateWindow", serde_json::json!({ "window": "addonbrowser", "parameters": [path] }))?;
    let (id, _) = wait_for(tv, 8, |id, _| id == BROWSER)?;
    if id != BROWSER {
        return failed(format!("Kodi didn't open its add-on browser ({})", showing(tv)));
    }
    // the rows: "..", then what the search found
    let rows: usize = label(tv, "Container.NumItems")?.parse().unwrap_or(0);
    let mut on = false;
    for _ in 0..=rows + 1 {
        if label(tv, "ListItem.Property(Addon.ID)")? == a.id {
            on = true;
            break;
        }
        call(tv, "Input.Down", "{}")?;
        std::thread::sleep(Duration::from_millis(300));
    }
    if !on {
        return failed(format!("Kodi's add-on search for \"{}\" didn't list {} (left on screen)", a.name, a.id));
    }
    call(tv, "Input.Select", "{}")?;
    let (id, focus) = wait_for(tv, 8, |id, _| id == INFO)?;
    if id != INFO {
        return failed(format!("{} didn't open its page ({})", a.name, showing(tv)));
    }
    Ok(focus)
}

/// Back from the add-on's page to Kodi's home screen (Back from the search
/// would open its keyboard).
fn close_page(tv: &Tv) {
    if window(tv).is_ok_and(|(id, _)| id == INFO) {
        let _ = call(tv, "Input.Back", "{}");
        let _ = wait_for(tv, 4, |id, _| id != INFO);
    }
    let _ = call(tv, "GUI.ActivateWindow", r#"{"window":"home"}"#);
}

/// The add-on `q` means, from those to install (or those installed).
fn find(tv: &Tv, q: &str) -> Result<(Addon, bool)> {
    for installed in [false, true] {
        let all = list(tv, installed)?;
        match pick(&all, q) {
            Ok(a) => return Ok((a.clone(), installed)),
            Err(many) if !many.is_empty() => {
                return failed(format!("{} Kodi add-ons match \"{q}\"; say which (name or id):\n{}", many.len(), lines(&many)));
            }
            Err(_) => {}
        }
    }
    failed(format!("no Kodi add-on called \"{q}\" (tveee kodi-addons available {q}: look for it)"))
}

/// `tveee kodi-addon <name>`: open its page in Kodi.
pub fn page_cmd(tv: &Tv, q: &str) -> Result<String> {
    let (a, installed) = find(tv, q)?;
    open_page(tv, &a)?;
    let what = if installed { "installed" } else { "not installed: tveee kodi-install installs it" };
    Ok(format!("{} ({} {}) is open in Kodi; {what}", a.name, a.id, a.version))
}

/// `tveee kodi-install <name>`: open its page, press Install, wait until
/// Kodi says it's there.
pub fn install_cmd(tv: &Tv, q: &str) -> Result<String> {
    let (a, installed) = find(tv, q)?;
    if installed {
        return Ok(format!("{} ({}) is already installed", a.name, a.id));
    }
    let focus = open_page(tv, &a)?;
    if focus != "Install()" && !focus.eq_ignore_ascii_case("install") {
        return failed(format!("{}'s page in Kodi isn't offering Install ({}; left on screen)", a.name, showing(tv)));
    }
    call(tv, "Input.Select", "{}")?;
    // done when Kodi lists it; something else asked on the way is left to
    // the person in front of the TV
    let until = Instant::now() + Duration::from_secs(120);
    loop {
        std::thread::sleep(Duration::from_millis(800));
        let done = super::call_json(tv, "Addons.GetAddonDetails", serde_json::json!({ "addonid": a.id, "properties": ["installed"] }))
            .is_ok_and(|r| r.pointer("/addon/installed").and_then(Value::as_bool).unwrap_or(true));
        if done {
            close_page(tv);
            return Ok(format!("installed {} ({} {})", a.name, a.id, a.version));
        }
        let (id, _) = window(tv)?;
        if !(id == INFO || id == BROWSER || id == HOME || PROGRESS.contains(&id)) {
            return failed(format!("installing {}: {} (answer it on the TV)", a.name, showing(tv)));
        }
        if Instant::now() >= until {
            return failed(format!("installing {}: not done after 2 minutes ({})", a.name, showing(tv)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addons() -> Vec<Addon> {
        parse(&serde_json::json!({"addons": [
            {"addonid": "plugin.audio.c3lounge", "name": "c3lounge Radio", "version": "1.0.0", "summary": "Stream c3lounge Radio", "enabled": true},
            {"addonid": "plugin.video.catchuptvandmore", "name": "Catch-up TV & More", "version": "0.2.42", "summary": "It's all in the title.", "enabled": true},
            {"addonid": "plugin.audio.cbcradio", "name": "CBC Radio", "version": "2.0.5", "summary": "CBC Radio", "enabled": false},
        ]}))
    }

    #[test]
    fn parsed() {
        let a = addons();
        assert_eq!(a.len(), 3);
        assert_eq!(a[1].name, "Catch-up TV & More");
        assert!(!a[2].enabled);
    }

    #[test]
    fn matched_by_every_word() {
        let a = addons();
        assert_eq!(matching(&a, "radio").len(), 2);
        assert_eq!(matching(&a, "cbc radio").len(), 1);
        assert_eq!(matching(&a, "").len(), 3);
        assert_eq!(matching(&a, "CATCH-UP").len(), 1);
    }

    #[test]
    fn picked_by_id_name_or_only_match() {
        let a = addons();
        assert_eq!(pick(&a, "plugin.audio.cbcradio").map(|x| x.name.as_str()), Ok("CBC Radio"));
        assert_eq!(pick(&a, "catch-up tv & more").map(|x| x.id.as_str()), Ok("plugin.video.catchuptvandmore"));
        assert_eq!(pick(&a, "lounge").map(|x| x.id.as_str()), Ok("plugin.audio.c3lounge"));
        // two radios: say which
        assert_eq!(pick(&a, "radio").map_err(|m| m.len()), Err(2));
        assert_eq!(pick(&a, "nothing like it").map_err(|m| m.len()), Err(0));
    }

    #[test]
    fn listed_a_line_each() {
        let a = addons();
        let s = lines(&a.iter().collect::<Vec<_>>());
        assert_eq!(s.lines().count(), 3);
        assert!(s.contains("plugin.audio.cbcradio 2.0.5  (disabled)"), "{s}");
    }
}

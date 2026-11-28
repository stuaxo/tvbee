//! Installing a Kodi add-on from a zip file here (not from Kodi's
//! repositories), and taking one off: no Kodi screens.
//!
//! An add-on zip holds one folder, named after the add-on's id, with its
//! `addon.xml`. tveee unzips it here, checks it and what it needs, copies
//! the folder into Kodi's add-ons over adb, and restarts Kodi (back where it
//! was) so it finds it. Kodi's API can't install or uninstall, and its
//! EventServer only listens on the TV itself, which adb's shell can't reach
//! with UDP; a restart is what's left.

use super::{PACKAGE, Result, Tv, call, failed};
use serde_json::Value;
use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Where Kodi keeps its add-ons on Android.
pub const ADDONS: &str = "/sdcard/Android/data/org.xbmc.kodi/files/.kodi/addons";

/// What an add-on's `addon.xml` says about it.
#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    /// Add-ons it needs (id, version), optional ones left out.
    pub requires: Vec<(String, String)>,
}

/// The value of `attr="..."` (or '...') in a tag's text.
fn attr(tag: &str, name: &str) -> Option<String> {
    let mut rest = tag;
    while let Some(i) = rest.find(name) {
        let before = rest[..i].chars().last();
        let after = rest[i + name.len()..].trim_start();
        if before.is_some_and(char::is_whitespace) {
            if let Some(v) = after.strip_prefix('=') {
                let v = v.trim_start();
                let q = v.chars().next()?;
                if q == '"' || q == '\'' {
                    let end = v[1..].find(q)?;
                    return Some(unescape(&v[1..1 + end]));
                }
            }
        }
        rest = &rest[i + name.len()..];
    }
    None
}

fn unescape(s: &str) -> String {
    s.replace("&quot;", "\"").replace("&apos;", "'").replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

/// The tags named `name` in `xml` (their text between < and >).
fn tags<'a>(xml: &'a str, name: &str) -> Vec<&'a str> {
    let open = format!("<{name}");
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find(&open) {
        let tag = &rest[i..];
        let next = tag[open.len()..].chars().next();
        let end = tag.find('>').unwrap_or(tag.len());
        if next.is_some_and(|c| c.is_whitespace() || c == '>' || c == '/') {
            out.push(&tag[..end]);
        }
        rest = &tag[end.min(tag.len())..];
        if end == 0 {
            break;
        }
    }
    out
}

/// Read an `addon.xml` (enough of it: no XML crate for four attributes).
pub fn parse_manifest(xml: &str) -> Result<Manifest> {
    let Some(addon) = tags(xml, "addon").into_iter().next() else { return failed("addon.xml has no <addon> tag") };
    let Some(id) = attr(addon, "id") else { return failed("addon.xml: <addon> has no id") };
    let requires = tags(xml, "import")
        .into_iter()
        .filter(|t| attr(t, "optional").as_deref() != Some("true"))
        .filter_map(|t| Some((attr(t, "addon")?, attr(t, "version").unwrap_or_default())))
        .collect();
    Ok(Manifest { name: attr(addon, "name").unwrap_or_else(|| id.clone()), version: attr(addon, "version").unwrap_or_default(), id, requires })
}

/// Parts of Kodi itself, not add-ons to install: the Python and GUI
/// interfaces an add-on says it's written for.
pub fn built_in(id: &str) -> bool {
    id.starts_with("xbmc.") || id.starts_with("kodi.")
}

/// An add-on zip, read: what it is, and its files under the add-on's folder.
pub struct Package {
    pub manifest: Manifest,
    pub files: Vec<(PathBuf, Vec<u8>)>,
}

/// Read and check a zip: one top folder named after the add-on, with its
/// addon.xml; no paths that climb out (`../`) or start at `/`.
pub fn read_zip(path: &Path) -> Result<Package> {
    let f = std::fs::File::open(path).map_err(|e| tveee_core::error::Error::Failed(format!("{}: {e}", path.display())))?;
    let mut z = zip::ZipArchive::new(f).map_err(|e| tveee_core::error::Error::Failed(format!("{}: not a zip file Kodi could use ({e})", path.display())))?;
    let mut files = Vec::new();
    let mut tops = BTreeSet::new();
    for i in 0..z.len() {
        let mut entry = z.by_index(i).map_err(|e| tveee_core::error::Error::Failed(format!("{}: {e}", path.display())))?;
        let Some(name) = entry.enclosed_name() else { return failed(format!("{}: {} would land outside the add-on's folder", path.display(), entry.name())) };
        if let Some(top) = name.components().next() {
            tops.insert(top.as_os_str().to_string_lossy().into_owned());
        }
        if entry.is_dir() {
            continue;
        }
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).map_err(|e| tveee_core::error::Error::Failed(format!("{}: {e}", path.display())))?;
        files.push((name, bytes));
    }
    let top = match tops.into_iter().collect::<Vec<_>>().as_slice() {
        [one] => one.clone(),
        [] => return failed(format!("{}: empty", path.display())),
        many => return failed(format!("{}: an add-on zip holds one folder, this has {}", path.display(), many.join(", "))),
    };
    let xml = files.iter().find(|(p, _)| *p == Path::new(&top).join("addon.xml"));
    let Some((_, xml)) = xml else { return failed(format!("{}: no {top}/addon.xml (is it a Kodi add-on?)", path.display())) };
    let manifest = parse_manifest(&String::from_utf8_lossy(xml))?;
    if manifest.id != top {
        return failed(format!("{}: its folder is {top} but addon.xml says {}", path.display(), manifest.id));
    }
    // paths inside the add-on's folder
    let files = files.into_iter().filter_map(|(p, b)| Some((p.strip_prefix(&top).ok()?.to_path_buf(), b))).collect();
    Ok(Package { manifest, files })
}

/// Every add-on Kodi has, installed or not: id -> installed.
fn known(tv: &Tv) -> Result<Vec<(String, bool)>> {
    let mut out = Vec::new();
    for installed in [true, false] {
        let r = call(tv, "Addons.GetAddons", &format!(r#"{{"installed":{installed}}}"#))?;
        for a in r.get("addons").and_then(Value::as_array).into_iter().flatten() {
            if let Some(id) = a.get("addonid").and_then(Value::as_str) {
                out.push((id.to_string(), installed));
            }
        }
    }
    Ok(out)
}

/// What `m` needs that Kodi hasn't got: (to get from its repositories,
/// nowhere to be had).
pub fn missing(m: &Manifest, known: &[(String, bool)]) -> (Vec<String>, Vec<String>) {
    let (mut get, mut none) = (Vec::new(), Vec::new());
    for (id, _) in m.requires.iter().filter(|(id, _)| !built_in(id)) {
        match known.iter().find(|(k, _)| k == id) {
            Some((_, true)) => {}
            Some((_, false)) => get.push(id.clone()),
            None => none.push(id.clone()),
        }
    }
    (get, none)
}

/// Whether Kodi is running (it has a process).
fn running(tv: &Tv) -> bool {
    tv.shell(&["pidof", PACKAGE]).is_ok_and(|s| !s.trim().is_empty())
}

/// Restart Kodi, back where it was, so it reads its add-ons again; none
/// needed when it isn't running (it will when it starts).
fn rescan(tv: &Tv) -> Result<String> {
    if !running(tv) {
        return Ok("Kodi will find it when it next starts".into());
    }
    let said = tveee_core::builtin::apps::restart(tv, PACKAGE, PACKAGE, true)?;
    // until its API answers again
    let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !super::ping(tv) && std::time::Instant::now() < until {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    Ok(said)
}

/// Installed in Kodi, and on (enabled; turned on if Kodi left it off).
fn enabled(tv: &Tv, id: &str) -> Result<Option<bool>> {
    let params = format!(r#"{{"addonid":{},"properties":["enabled"]}}"#, Value::from(id));
    match call(tv, "Addons.GetAddonDetails", &params) {
        Ok(r) => Ok(r.pointer("/addon/enabled").and_then(Value::as_bool)),
        Err(_) => Ok(None),
    }
}

/// `tveee kodi-install <file.zip>`.
pub fn install_zip(tv: &Tv, path: &Path) -> Result<String> {
    let pkg = read_zip(path)?;
    let m = &pkg.manifest;
    let (get, none) = missing(m, &known(tv)?);
    if !none.is_empty() {
        return failed(format!("{} needs add-ons Kodi can't get: {} (put their zips in first)", m.name, none.join(", ")));
    }
    if !get.is_empty() {
        let cmds: Vec<String> = get.iter().map(|id| format!("tveee kodi-install {id}")).collect();
        return failed(format!("{} needs add-ons from Kodi's repositories first:\n{}", m.name, cmds.join("\n")));
    }
    let before = enabled(tv, &m.id)?;
    // unpacked here, then copied over whole (an older copy goes first)
    let dir = std::env::temp_dir().join(format!("tveee-kodi-{}", std::process::id()));
    let local = dir.join(&m.id);
    let _ = std::fs::remove_dir_all(&dir);
    for (rel, bytes) in &pkg.files {
        let p = local.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&p, bytes)?;
    }
    let remote = format!("{ADDONS}/{}", m.id);
    let pushed = tv.shell(&["rm", "-rf", &remote]).and_then(|_| tv.adb(&["push", &local.to_string_lossy(), &format!("{ADDONS}/")]));
    let _ = std::fs::remove_dir_all(&dir);
    pushed?;
    let restarted = rescan(tv)?;
    let what = if before.is_some() { "updated" } else { "installed" };
    match enabled(tv, &m.id)? {
        Some(true) => Ok(format!("{what} {} ({} {}); {restarted}", m.name, m.id, m.version)),
        Some(false) => {
            call(tv, "Addons.SetAddonEnabled", &format!(r#"{{"addonid":{},"enabled":true}}"#, Value::from(m.id.as_str())))?;
            Ok(format!("{what} {} ({} {}) and turned it on; {restarted}", m.name, m.id, m.version))
        }
        None if running(tv) => failed(format!("copied {} into Kodi, but Kodi doesn't list it after restarting (look in its add-ons, or its log)", m.id)),
        None => Ok(format!("copied {} ({} {}) into Kodi; {restarted}", m.name, m.id, m.version)),
    }
}

/// The add-ons in a `Addons.GetAddons` result (with "dependencies") that
/// need `id` (not just optionally).
pub fn needed_by(r: &Value, id: &str) -> Vec<String> {
    let needs = |a: &Value| {
        a.get("dependencies")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .any(|d| d.get("addonid").and_then(Value::as_str) == Some(id) && d.get("optional").and_then(Value::as_bool) != Some(true))
    };
    r.get("addons")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|a| needs(a))
        .filter_map(|a| a.get("addonid").and_then(Value::as_str).map(str::to_string))
        .collect()
}

/// `tveee kodi-uninstall <id>`: take an add-on's folder out, unless
/// another add-on needs it, and restart Kodi.
pub fn uninstall(tv: &Tv, id: &str) -> Result<String> {
    if built_in(id) || id.contains('/') || id.contains("..") || id.trim().is_empty() {
        return failed(format!("{id}: not an add-on that can be taken off"));
    }
    let remote = format!("{ADDONS}/{id}");
    if tv.shell(&["ls", "-d", &remote]).is_err() {
        return failed(format!("{id} isn't in Kodi's add-ons folder (built in, or not installed)"));
    }
    let r = call(tv, "Addons.GetAddons", r#"{"installed":true,"properties":["dependencies"]}"#)?;
    let needed_by = needed_by(&r, id);
    if !needed_by.is_empty() {
        return failed(format!("{id} is needed by {}", needed_by.join(", ")));
    }
    tv.shell(&["rm", "-rf", &remote])?;
    let restarted = rescan(tv)?;
    Ok(format!("took {id} off Kodi; {restarted}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<addon id="plugin.video.tveee-test" name="tveee &amp; test" version="0.1.0" provider-name="tveee">
  <requires>
    <import addon="xbmc.python" version="3.0.0"/>
    <import addon='script.module.requests' version='2.22.0'/>
    <import addon="script.module.six" version="1.0" optional="true"/>
  </requires>
  <extension point="xbmc.python.pluginsource" library="default.py"><provides>video</provides></extension>
</addon>"#;

    #[test]
    fn manifest_read() {
        let m = parse_manifest(XML).unwrap();
        assert_eq!((m.id.as_str(), m.name.as_str(), m.version.as_str()), ("plugin.video.tveee-test", "tveee & test", "0.1.0"));
        // the optional one isn't needed
        assert_eq!(m.requires, vec![("xbmc.python".to_string(), "3.0.0".to_string()), ("script.module.requests".to_string(), "2.22.0".to_string())]);
        // an attribute's name inside another's doesn't count
        assert_eq!(attr(r#"<addon provider-name="x" name="y""#, "name").as_deref(), Some("y"));
        assert!(parse_manifest("<nothing/>").is_err());
    }

    #[test]
    fn what_needs_it() {
        let r = serde_json::json!({"addons": [
            {"addonid": "plugin.video.a", "dependencies": [{"addonid": "script.module.six", "optional": false}]},
            {"addonid": "plugin.video.b", "dependencies": [{"addonid": "script.module.six", "optional": true}]},
            {"addonid": "plugin.video.c", "dependencies": []},
        ]});
        assert_eq!(needed_by(&r, "script.module.six"), vec!["plugin.video.a".to_string()]);
        assert!(needed_by(&r, "plugin.video.tveee-test").is_empty());
    }

    #[test]
    fn what_is_missing() {
        let m = parse_manifest(XML).unwrap();
        let known = vec![("script.module.requests".to_string(), false)];
        assert_eq!(missing(&m, &known), (vec!["script.module.requests".to_string()], vec![]));
        assert_eq!(missing(&m, &[]), (vec![], vec!["script.module.requests".to_string()]));
        assert_eq!(missing(&m, &[("script.module.requests".to_string(), true)]), (vec![], vec![]));
    }

    fn zip_of(files: &[(&str, &str)]) -> PathBuf {
        use std::io::Write;
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!("tveee-test-{}-{}.zip", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
        // stored (not compressed): the crate is built to read only
        let mut out = Vec::new();
        let mut central = Vec::new();
        for (name, body) in files {
            let crc = crc32(body.as_bytes());
            let off = out.len() as u32;
            let head = |sig: u32, central: bool| {
                let mut h = Vec::new();
                h.extend(sig.to_le_bytes());
                if central {
                    h.extend(20u16.to_le_bytes());
                }
                h.extend(20u16.to_le_bytes());
                h.extend([0u8; 2]); // flags
                h.extend([0u8; 2]); // stored
                h.extend([0u8; 4]); // time, date
                h.extend(crc.to_le_bytes());
                h.extend((body.len() as u32).to_le_bytes());
                h.extend((body.len() as u32).to_le_bytes());
                h.extend((name.len() as u16).to_le_bytes());
                h.extend([0u8; 2]);
                if central {
                    // comment length, disk, internal and external attributes
                    h.extend([0u8; 2 + 2 + 2 + 4]);
                    h.extend(off.to_le_bytes());
                }
                h.extend(name.as_bytes());
                h
            };
            out.extend(head(0x0403_4b50, false));
            out.extend(body.as_bytes());
            central.extend(head(0x0201_4b50, true));
        }
        let cd_off = out.len() as u32;
        out.extend(&central);
        out.extend(0x0605_4b50u32.to_le_bytes());
        out.extend([0u8; 4]);
        out.extend((files.len() as u16).to_le_bytes());
        out.extend((files.len() as u16).to_le_bytes());
        out.extend((central.len() as u32).to_le_bytes());
        out.extend(cd_off.to_le_bytes());
        out.extend([0u8; 2]);
        std::fs::File::create(&p).unwrap().write_all(&out).unwrap();
        p
    }

    fn crc32(b: &[u8]) -> u32 {
        let mut c = !0u32;
        for &x in b {
            c ^= u32::from(x);
            for _ in 0..8 {
                c = if c & 1 == 1 { (c >> 1) ^ 0xEDB8_8320 } else { c >> 1 };
            }
        }
        !c
    }

    #[test]
    fn zips_checked() {
        let good = zip_of(&[("plugin.video.tveee-test/addon.xml", XML), ("plugin.video.tveee-test/default.py", "pass\n")]);
        let p = read_zip(&good).unwrap();
        assert_eq!(p.manifest.id, "plugin.video.tveee-test");
        assert_eq!(p.files.len(), 2);
        assert!(p.files.iter().any(|(f, _)| f == Path::new("default.py")));
        // two folders, a folder that isn't its id, climbing out
        let two = zip_of(&[("plugin.video.tveee-test/addon.xml", XML), ("other/x.py", "")]);
        assert!(read_zip(&two).is_err());
        let wrong = zip_of(&[("plugin.video.other/addon.xml", XML)]);
        assert!(read_zip(&wrong).err().unwrap().to_string().contains("addon.xml says"));
        let climb = zip_of(&[("plugin.video.tveee-test/addon.xml", XML), ("plugin.video.tveee-test/../../evil.py", "")]);
        assert!(read_zip(&climb).err().unwrap().to_string().contains("outside"));
        for z in [good, two, wrong, climb] {
            let _ = std::fs::remove_file(z);
        }
    }
}

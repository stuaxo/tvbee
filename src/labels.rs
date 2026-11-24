//! Apps' own names ("projectM", not `com.psperl.projectMTV`), as the TV's
//! launcher shows them.
//!
//! Android has no command that prints an app's name. Some apps give it
//! outright (`nonLocalizedLabel` in `cmd package query-activities`); the rest
//! point at a string in their APK (`labelRes`). For those, tveee reads the
//! APK's resource table (`resources.arsc`) straight out of the zip on the TV
//! with `dd` (the zip's directory, then the one file: no copying whole
//! APKs) and looks the string up, in the TV's language where the app has it.
//!
//! Names are cached per TV with the APK's path, which changes whenever the
//! app is installed again or updated, so an app is only read once a version.
//! The ui finds them (in the background); everything else uses what it found.
#![cfg_attr(not(feature = "ui"), allow(dead_code))]

use crate::error::{failed, Result};
use crate::tv::Tv;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// Launchable apps: TV launcher ones, and ordinary ones.
const QUERY: &str = "cmd package query-activities -a android.intent.action.MAIN -c android.intent.category.LEANBACK_LAUNCHER; \
                     cmd package query-activities -a android.intent.action.MAIN -c android.intent.category.LAUNCHER";

/// What the TV says about an app's name.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub package: String,
    /// The name, when the app gives it outright.
    pub plain: Option<String>,
    /// Otherwise the string resource holding it.
    pub res: Option<u32>,
    /// The APK it's in.
    pub apk: String,
}

/// Apps from [`QUERY`]'s (verbose) output: the activity's label if it has
/// one, else its application's; the first activity of a package counts.
pub fn parse_query(out: &str) -> Vec<Found> {
    let mut list: Vec<Found> = Vec::new();
    // per block: the package, label lines in order (activity, application), apk
    let mut pkg: Option<String> = None;
    let mut labels: Vec<(Option<String>, Option<u32>)> = Vec::new();
    let flush = |pkg: &mut Option<String>, labels: &mut Vec<(Option<String>, Option<u32>)>, apk: Option<String>, list: &mut Vec<Found>| {
        if let (Some(p), Some(apk)) = (pkg.take(), apk) {
            let plain = labels.iter().find_map(|(n, _)| n.clone());
            let res = labels.iter().find_map(|(_, r)| *r);
            if !list.iter().any(|f| f.package == p) {
                list.push(Found { package: p, plain, res, apk });
            }
        }
        labels.clear();
    };
    for line in out.lines() {
        let t = line.trim();
        if let Some(p) = t.strip_prefix("packageName=") {
            if pkg.is_none() {
                pkg = Some(p.to_string());
            }
        } else if t.starts_with("labelRes=") {
            let field = |k: &str| t.split_whitespace().find_map(|w| w.strip_prefix(k));
            let res = field("labelRes=").and_then(|h| u32::from_str_radix(h.trim_start_matches("0x"), 16).ok()).filter(|r| *r != 0);
            // "nonLocalizedLabel=Air Receiver icon=..." : the name runs up to " icon="
            let plain = t.split_once("nonLocalizedLabel=").map(|(_, r)| r.split(" icon=").next().unwrap_or(r).trim().to_string());
            labels.push((plain.filter(|n| n != "null" && !n.is_empty()), res));
        } else if let Some(apk) = t.strip_prefix("sourceDir=") {
            flush(&mut pkg, &mut labels, Some(apk.to_string()), &mut list);
        }
    }
    list
}

fn u16_at(b: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(i..i + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?))
}

/// Where the zip's central directory is (offset, size), from its end
/// (`tail`, the last bytes of the file).
pub fn zip_directory(tail: &[u8]) -> Option<(u64, u64)> {
    let at = tail.windows(4).rposition(|w| w == b"PK\x05\x06")?;
    Some((u64::from(u32_at(tail, at + 16)?), u64::from(u32_at(tail, at + 12)?)))
}

/// Where `name` is in the zip, from its central directory: (offset of its
/// local header, size), if it's stored uncompressed.
pub fn zip_entry(dir: &[u8], name: &str) -> Option<(u64, u64)> {
    let mut p = 0;
    while dir.get(p..p + 4) == Some(b"PK\x01\x02") {
        let method = u16_at(dir, p + 10)?;
        let size = u32_at(dir, p + 20)?;
        let (fl, el, cl) = (usize::from(u16_at(dir, p + 28)?), usize::from(u16_at(dir, p + 30)?), usize::from(u16_at(dir, p + 32)?));
        let offset = u32_at(dir, p + 42)?;
        if dir.get(p + 46..p + 46 + fl) == Some(name.as_bytes()) {
            // compressed: would need inflating (APKs store it plain)
            return (method == 0).then_some((u64::from(offset), u64::from(size)));
        }
        p += 46 + fl + el + cl;
    }
    None
}

/// The strings of a string pool chunk starting at `p`.
fn string_pool(b: &[u8], p: usize) -> Option<Vec<String>> {
    let count = u32_at(b, p + 8)? as usize;
    let utf8 = u32_at(b, p + 16)? & 0x100 != 0;
    let start = p + u32_at(b, p + 20)? as usize;
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let mut q = start + u32_at(b, p + 28 + 4 * i)? as usize;
        let s = if utf8 {
            // length in characters, then in bytes; each 1 or 2 bytes
            let skip = |q: &mut usize| -> Option<usize> {
                let n = usize::from(*b.get(*q)?);
                if n & 0x80 != 0 {
                    let m = ((n & 0x7f) << 8) | usize::from(*b.get(*q + 1)?);
                    *q += 2;
                    Some(m)
                } else {
                    *q += 1;
                    Some(n)
                }
            };
            skip(&mut q)?;
            let n = skip(&mut q)?;
            String::from_utf8_lossy(b.get(q..q + n)?).into_owned()
        } else {
            let mut n = usize::from(u16_at(b, q)?);
            q += 2;
            if n & 0x8000 != 0 {
                n = ((n & 0x7fff) << 16) | usize::from(u16_at(b, q)?);
                q += 2;
            }
            let units: Vec<u16> = (0..n).filter_map(|k| u16_at(b, q + 2 * k)).collect();
            String::from_utf16_lossy(&units)
        };
        out.push(s);
    }
    Some(out)
}

/// The string resource `id` in a resource table, in language `lang` (e.g.
/// "en-GB") if the app has it: that language and country, then the
/// language, then the default, then whatever there is.
pub fn resolve(arsc: &[u8], id: u32, lang: &str) -> Option<String> {
    let (want_lang, want_country) = lang.split_once(['-', '_']).unwrap_or((lang, ""));
    let (pkg_id, type_id, entry) = (id >> 24, ((id >> 16) & 0xff) as u8, (id & 0xffff) as usize);
    let header = usize::from(u16_at(arsc, 2)?);
    let strings = string_pool(arsc, header)?;
    let mut p = header + u32_at(arsc, header + 4)? as usize;
    // (how good a match the config is, the string)
    let mut best: Option<(u8, String)> = None;
    while p + 8 <= arsc.len() {
        let (kind, size) = (u16_at(arsc, p)?, u32_at(arsc, p + 4)? as usize);
        if size == 0 {
            break;
        }
        if kind == 0x0200 && u32_at(arsc, p + 8)? == pkg_id {
            let mut q = p + usize::from(u16_at(arsc, p + 2)?);
            while q + 8 <= p + size {
                let (k, hsize, csize) = (u16_at(arsc, q)?, usize::from(u16_at(arsc, q + 2)?), u32_at(arsc, q + 4)? as usize);
                if csize == 0 {
                    break;
                }
                if k == 0x0201 && *arsc.get(q + 8)? == type_id && entry < u32_at(arsc, q + 12)? as usize {
                    let at = u32_at(arsc, q + hsize + 4 * entry)?;
                    let config = q + 20;
                    let text = |i: usize| String::from_utf8_lossy(arsc.get(config + i..config + i + 2).unwrap_or_default()).trim_end_matches('\0').to_string();
                    let (l, c) = (text(8), text(10));
                    if at != u32::MAX {
                        let e = q + u32_at(arsc, q + 16)? as usize + at as usize;
                        // a simple entry holding a string
                        if u16_at(arsc, e + 2)? & 1 == 0 && *arsc.get(e + 8 + 3)? == 3 {
                            let s = strings.get(u32_at(arsc, e + 12)? as usize)?.clone();
                            let score = match (l.as_str(), c.as_str()) {
                                (l, c) if l == want_lang && c.eq_ignore_ascii_case(want_country) && !c.is_empty() => 4,
                                (l, "") if l == want_lang => 3,
                                (l, _) if l == want_lang => 2,
                                ("", "") => 1,
                                _ => 0,
                            };
                            if best.as_ref().is_none_or(|(b, _)| score > *b) {
                                best = Some((score, s));
                            }
                        }
                    }
                }
                q += csize;
            }
        }
        p += size;
    }
    best.map(|(_, s)| s)
}

/// Bytes `from..from+len` of file `path` on the TV.
fn read_range(tv: &Tv, path: &str, from: u64, len: u64) -> Result<Vec<u8>> {
    const BLOCK: u64 = 4096;
    let (skip, start) = (from / BLOCK, (from % BLOCK) as usize);
    let count = (start as u64 + len).div_ceil(BLOCK);
    let raw = tv.exec_out(&[&format!("dd if='{path}' bs={BLOCK} skip={skip} count={count} 2>/dev/null")])?;
    match raw.get(start..start + len as usize) {
        Some(b) => Ok(b.to_vec()),
        None => failed(format!("short read of {path}")),
    }
}

/// How much of the end of an APK to read for its zip directory.
const TAIL: u64 = 64 * 1024;

/// The name in `apk` with resource `id`, read on the TV: three reads (the
/// end of the zip with the file's size, its directory if that's further
/// back, then the resource table).
pub fn read_label(tv: &Tv, apk: &str, id: u32, lang: &str) -> Result<Option<String>> {
    // "<size>\n" then the last TAIL bytes (whole blocks: trimmed below)
    let raw = tv.exec_out(&[&format!(
        "s=$(stat -c %s '{apk}') && echo $s && n=$(( s / 4096 )) && dd if='{apk}' bs=4096 skip=$(( n > {b} ? n - {b} : 0 )) 2>/dev/null",
        b = TAIL / 4096
    )])?;
    let Some(nl) = raw.iter().position(|b| *b == b'\n') else { return failed(format!("can't read {apk}")) };
    let Ok(size) = String::from_utf8_lossy(&raw[..nl]).trim().parse::<u64>() else { return failed(format!("can't read {apk}")) };
    // dd read to the end of the file: the tail starts this far in
    let tail = &raw[nl + 1..];
    let tail_at = size.saturating_sub(tail.len() as u64);
    let Some((dir_at, dir_len)) = zip_directory(tail) else { return Ok(None) };
    let dir = if dir_at >= tail_at {
        tail.get((dir_at - tail_at) as usize..).unwrap_or_default().to_vec()
    } else {
        read_range(tv, apk, dir_at, dir_len)?
    };
    let Some((local, len)) = zip_entry(&dir, "resources.arsc") else { return Ok(None) };
    // the local header (30 bytes, then a name and an extra field of its own:
    // allow for those), and the file after it, in one read
    let slack = 30 + 1024;
    let raw = read_range(tv, apk, local, (len + slack).min(size - local))?;
    let skip = 30 + usize::from(u16_at(&raw, 26).unwrap_or(0)) + usize::from(u16_at(&raw, 28).unwrap_or(0));
    let Some(arsc) = raw.get(skip..skip + len as usize) else { return Ok(None) };
    Ok(resolve(arsc, id, lang))
}

fn cache_file(ip: &str) -> Option<std::path::PathBuf> {
    Some(crate::plugin::cache_path(ip)?.with_extension("labels"))
}

/// `package<TAB>apk<TAB>name` lines (an empty name: tried, none found).
fn parse_cache(text: &str) -> HashMap<String, (String, String)> {
    text.lines()
        .filter_map(|l| {
            let mut w = l.splitn(3, '\t');
            let (p, apk, name) = (w.next()?, w.next()?, w.next()?);
            Some((p.to_string(), (apk.to_string(), name.to_string())))
        })
        .collect()
}

fn render_cache(m: &HashMap<String, (String, String)>) -> String {
    let mut rows: Vec<_> = m.iter().collect();
    rows.sort();
    rows.iter().map(|(p, (apk, name))| format!("{p}\t{apk}\t{name}\n")).collect()
}

/// Package → name, for this process's TV, from the cache (and what
/// [`refresh`] found since).
fn known() -> &'static Mutex<HashMap<String, (String, String)>> {
    static KNOWN: OnceLock<Mutex<HashMap<String, (String, String)>>> = OnceLock::new();
    KNOWN.get_or_init(|| {
        // tests don't depend on what this machine has cached
        if cfg!(test) {
            return Mutex::new(HashMap::new());
        }
        let ip = Tv::from_env().ip;
        let m = cache_file(&ip).and_then(|f| std::fs::read_to_string(f).ok()).map(|t| parse_cache(&t)).unwrap_or_default();
        Mutex::new(m)
    })
}

/// The app's own name, if tveee has found it.
pub fn name(pkg: &str) -> Option<String> {
    let m = known().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    m.get(pkg).map(|(_, n)| n.clone()).filter(|n| !n.is_empty())
}

/// Find the names of the TV's apps that aren't known yet (or whose APK
/// changed). Returns how many it found. Slow-ish the first time (a few
/// small reads per app): for the background.
pub fn refresh(tv: &Tv) -> Result<usize> {
    let found = parse_query(&tv.shell(&[QUERY])?);
    let lang = tv.shell(&["getprop", "persist.sys.locale"]).map(|l| l.trim().to_string()).unwrap_or_default();
    let mut new = 0;
    // only apps that would otherwise show as their package
    let named = |pkg: &str| crate::plugins::apps::by_package(pkg).is_some_and(|a| !a.name.starts_with("a ") && !a.name.starts_with("the "));
    for f in found.into_iter().filter(|f| !named(&f.package)) {
        let current = known().lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&f.package).cloned();
        if current.as_ref().is_some_and(|(apk, _)| *apk == f.apk) {
            continue;
        }
        let name = match (&f.plain, f.res) {
            (Some(n), _) => Some(n.clone()),
            (None, Some(id)) => read_label(tv, &f.apk, id, &lang).ok().flatten(),
            (None, None) => None,
        };
        new += usize::from(name.is_some());
        let mut m = known().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        m.insert(f.package, (f.apk, name.unwrap_or_default()));
        if let Some(file) = cache_file(&tv.ip) {
            if let Some(dir) = file.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(file, render_cache(&m));
        }
    }
    Ok(new)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_query() {
        let out = "  Activity #0:
      name=com.psperl.prjM.TvActivity
      packageName=com.psperl.projectMTV
      labelRes=0x7f13001c nonLocalizedLabel=null icon=0x0 banner=0x0
      ApplicationInfo:
        packageName=com.psperl.projectMTV
        labelRes=0x7f13001c nonLocalizedLabel=null icon=0x7f080189 banner=0x7f080079
        sourceDir=/data/app/com.psperl.projectMTV-k3/base.apk
  Activity #1:
      name=com.softmedia.receiver.app.SplashActivity
      packageName=com.softmedia.receiver
      labelRes=0x0 nonLocalizedLabel=null icon=0x0 banner=0x0
      ApplicationInfo:
        packageName=com.softmedia.receiver
        labelRes=0x0 nonLocalizedLabel=Air Receiver icon=0x7f0f0001 banner=0x7f0f0000
        sourceDir=/data/app/com.softmedia.receiver-1/base.apk
  Activity #2:
      packageName=com.psperl.projectMTV
      labelRes=0x7f130099 nonLocalizedLabel=null icon=0x0 banner=0x0
      ApplicationInfo:
        packageName=com.psperl.projectMTV
        sourceDir=/data/app/com.psperl.projectMTV-k3/base.apk
";
        let got = parse_query(out);
        assert_eq!(got.len(), 2);
        assert_eq!((got[0].res, got[0].plain.as_deref(), got[0].apk.as_str()), (Some(0x7f13001c), None, "/data/app/com.psperl.projectMTV-k3/base.apk"));
        assert_eq!((got[1].res, got[1].plain.as_deref()), (None, Some("Air Receiver")));
    }

    /// A resource table with one package (0x7f) holding string 0x7f130002
    /// in the default config ("Default") and in en-GB ("British").
    fn table() -> Vec<u8> {
        fn chunk(kind: u16, header: &[u8], body: &[u8]) -> Vec<u8> {
            let hsize = 8 + header.len();
            let mut c = Vec::new();
            c.extend(kind.to_le_bytes());
            c.extend((hsize as u16).to_le_bytes());
            c.extend(((hsize + body.len()) as u32).to_le_bytes());
            c.extend(header);
            c.extend(body);
            c
        }
        fn pool(strings: &[&str]) -> Vec<u8> {
            // UTF-16
            let mut data = Vec::new();
            let mut offs = Vec::new();
            for s in strings {
                offs.push(data.len() as u32);
                let units: Vec<u16> = s.encode_utf16().collect();
                data.extend((units.len() as u16).to_le_bytes());
                for u in units {
                    data.extend(u.to_le_bytes());
                }
                data.extend([0, 0]);
            }
            let mut header = Vec::new();
            header.extend((strings.len() as u32).to_le_bytes());
            header.extend(0u32.to_le_bytes()); // styles
            header.extend(0u32.to_le_bytes()); // flags: UTF-16
            header.extend((28 + 4 * strings.len() as u32).to_le_bytes()); // strings start
            header.extend(0u32.to_le_bytes());
            let mut body = Vec::new();
            for o in offs {
                body.extend(o.to_le_bytes());
            }
            body.extend(data);
            chunk(0x0001, &header, &body)
        }
        fn type_chunk(lang: &str, country: &str, value: u32) -> Vec<u8> {
            // config: size 28, then mcc/mnc, language, country
            let mut config = vec![0u8; 28];
            config[0] = 28;
            config[8..8 + lang.len()].copy_from_slice(lang.as_bytes());
            config[10..10 + country.len()].copy_from_slice(country.as_bytes());
            let entries = 3u32;
            let mut header = vec![0x13, 0, 0, 0]; // id 0x13, res0, res1
            header.extend(entries.to_le_bytes());
            let start = 8 + 12 + 28 + 4 * entries;
            header.extend(start.to_le_bytes());
            header.extend(config);
            let mut body = Vec::new();
            body.extend(u32::MAX.to_le_bytes());
            body.extend(u32::MAX.to_le_bytes());
            body.extend(0u32.to_le_bytes()); // entry 2 at 0
            // entry: size 8, flags 0, key 0; value: size 8, 0, type 3 (string), data
            body.extend([8, 0, 0, 0, 0, 0, 0, 0, 8, 0, 0, 3]);
            body.extend(value.to_le_bytes());
            chunk(0x0201, &header, &body)
        }
        let mut pkg_header = Vec::new();
        pkg_header.extend(0x7fu32.to_le_bytes());
        pkg_header.extend([0u8; 256]); // name
        pkg_header.extend([0u8; 20]); // string pool offsets etc. (unused here)
        let mut pkg_body = Vec::new();
        pkg_body.extend(type_chunk("", "", 0));
        pkg_body.extend(type_chunk("en", "GB", 1));
        let package = chunk(0x0200, &pkg_header, &pkg_body);
        let mut body = pool(&["Default", "British"]);
        body.extend(package);
        chunk(0x0002, &1u32.to_le_bytes(), &body)
    }

    #[test]
    fn looks_up_strings_by_language() {
        let t = table();
        assert_eq!(resolve(&t, 0x7f130002, "en-GB").as_deref(), Some("British"));
        assert_eq!(resolve(&t, 0x7f130002, "fr-FR").as_deref(), Some("Default"));
        // missing entry, wrong type
        assert_eq!(resolve(&t, 0x7f130000, "en-GB"), None);
        assert_eq!(resolve(&t, 0x7f140002, "en-GB"), None);
    }

    #[test]
    fn finds_files_in_a_zip() {
        // a central directory entry for "resources.arsc", stored, 5 bytes at 100
        let name = b"resources.arsc";
        let mut dir = b"PK\x01\x02".to_vec();
        dir.extend([0u8; 6]); // versions, flags
        dir.extend(0u16.to_le_bytes()); // method: stored
        dir.extend([0u8; 8]); // time, date, crc
        dir.extend(5u32.to_le_bytes());
        dir.extend(5u32.to_le_bytes());
        dir.extend((name.len() as u16).to_le_bytes());
        dir.extend([0u8; 12]); // extra, comment, disk, attrs
        dir.extend(100u32.to_le_bytes());
        dir.extend(name);
        assert_eq!(zip_entry(&dir, "resources.arsc"), Some((100, 5)));
        assert_eq!(zip_entry(&dir, "classes.dex"), None);
        // the end record: directory of 46+14 bytes at 200
        let mut end = b"PK\x05\x06".to_vec();
        end.extend([0u8; 8]);
        end.extend(60u32.to_le_bytes());
        end.extend(200u32.to_le_bytes());
        end.extend([0u8; 2]);
        assert_eq!(zip_directory(&end), Some((200, 60)));
    }

    #[test]
    fn cache_round_trip() {
        let mut m = HashMap::new();
        m.insert("a.b".to_string(), ("/data/app/a.b-1/base.apk".to_string(), "A B".to_string()));
        m.insert("c.d".to_string(), ("/x.apk".to_string(), String::new()));
        assert_eq!(parse_cache(&render_cache(&m)), m);
    }
}

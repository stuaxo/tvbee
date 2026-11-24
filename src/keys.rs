//! Sending keys quickly.
//!
//! `adb shell input keyevent` starts a Java VM on the TV for every key, which
//! takes seconds on a busy TV. Instead, tveee writes raw key events straight
//! into one of the TV's input devices (the shell user may: it's in the
//! `input` group), which takes about as long as an adb round trip.
//!
//! Which device and scan code give an Android key is worked out once per TV
//! boot ([`discover`]): the devices' declared keys (`getevent -i`), and their
//! key layout files (`.kl`, found the way Android finds them). Keys no device
//! can give (and the power keys) fall back to `input keyevent`, as does
//! everything with the `fast-keys` option off (`tveee options fast-keys off`).

use crate::error::Result;
use crate::tv::Tv;
use std::collections::HashMap;
use std::io::Write;

/// Android key codes tveee sends, by their names in `.kl` files.
const NAMES: &[(u32, &str)] = &[
    (3, "HOME"),
    (4, "BACK"),
    (19, "DPAD_UP"),
    (20, "DPAD_DOWN"),
    (21, "DPAD_LEFT"),
    (22, "DPAD_RIGHT"),
    (23, "DPAD_CENTER"),
    (24, "VOLUME_UP"),
    (25, "VOLUME_DOWN"),
    (55, "COMMA"),
    (56, "PERIOD"),
    (59, "SHIFT_LEFT"),
    (62, "SPACE"),
    (66, "ENTER"),
    (67, "DEL"),
    (69, "MINUS"),
    (70, "EQUALS"),
    (71, "LEFT_BRACKET"),
    (72, "RIGHT_BRACKET"),
    (73, "BACKSLASH"),
    (74, "SEMICOLON"),
    (75, "APOSTROPHE"),
    (76, "SLASH"),
    (77, "AT"),
    (81, "PLUS"),
    (82, "MENU"),
    (84, "SEARCH"),
    (85, "MEDIA_PLAY_PAUSE"),
    (86, "MEDIA_STOP"),
    (87, "MEDIA_NEXT"),
    (88, "MEDIA_PREVIOUS"),
    (89, "MEDIA_REWIND"),
    (90, "MEDIA_FAST_FORWARD"),
    (92, "PAGE_UP"),
    (93, "PAGE_DOWN"),
    (112, "FORWARD_DEL"),
    (126, "MEDIA_PLAY"),
    (127, "MEDIA_PAUSE"),
    (164, "VOLUME_MUTE"),
    (166, "CHANNEL_UP"),
    (167, "CHANNEL_DOWN"),
    (172, "GUIDE"),
    (165, "INFO"),
];

/// Never sent raw: power and sleep behave differently from a real remote.
const NOT_RAW: &[u32] = &[26, 223, 224];

const SHIFT: u32 = 59;

/// The `.kl` name of Android key `code`.
fn name_of(code: u32) -> Option<String> {
    match code {
        7..=16 => Some(((b'0' + (code - 7) as u8) as char).to_string()),
        29..=54 => Some(((b'A' + (code - 29) as u8) as char).to_string()),
        _ => NAMES.iter().find(|(c, _)| *c == code).map(|(_, n)| n.to_string()),
    }
}

/// The Android key (and whether Shift is held) that types `c`, for the
/// characters whose key doesn't depend on the keyboard's layout.
pub fn key_for_char(c: char) -> Option<(u32, bool)> {
    Some(match c {
        'a'..='z' => (29 + (c as u32 - 'a' as u32), false),
        'A'..='Z' => (29 + (c as u32 - 'A' as u32), true),
        '0'..='9' => (7 + (c as u32 - '0' as u32), false),
        ' ' => (62, false),
        ',' => (55, false),
        '.' => (56, false),
        '-' => (69, false),
        '=' => (70, false),
        '[' => (71, false),
        ']' => (72, false),
        '\\' => (73, false),
        ';' => (74, false),
        '\'' => (75, false),
        '/' => (76, false),
        _ => return None,
    })
}

/// One input device: its path, and the scan codes it declares.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Device {
    pub path: String,
    pub name: String,
    pub vendor: String,
    pub product: String,
    pub version: String,
    pub keys: Vec<u16>,
}

/// Devices from `getevent -i`.
pub fn parse_getevent(out: &str) -> Vec<Device> {
    let mut list: Vec<Device> = Vec::new();
    let mut in_keys = false;
    for line in out.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("add device ") {
            let path = rest.split_once(':').map_or("", |(_, p)| p.trim());
            list.push(Device { path: path.to_string(), ..Device::default() });
            in_keys = false;
            continue;
        }
        let Some(d) = list.last_mut() else { continue };
        let field = |k: &str| t.strip_prefix(k).map(|v| v.trim().to_string());
        if let Some(v) = field("vendor") {
            d.vendor = v;
        } else if let Some(v) = field("product") {
            d.product = v;
        } else if let Some(v) = field("version ") {
            // "version   0100" (the id), not "version:  1.0.1" (the driver)
            if !v.starts_with(':') {
                d.version = v;
            }
        } else if let Some(v) = field("name:") {
            d.name = v.trim_matches('"').to_string();
        }
        // KEY (0001): 0001  0002 ... and continuation lines of hex codes
        let codes = if let Some(rest) = t.strip_prefix("KEY (0001):") {
            in_keys = true;
            rest
        } else if in_keys && t.split_whitespace().all(|w| w.len() == 4 && u16::from_str_radix(w, 16).is_ok()) && !t.is_empty() {
            t
        } else {
            if t.contains("):") {
                in_keys = false;
            }
            continue;
        };
        d.keys.extend(codes.split_whitespace().filter_map(|w| u16::from_str_radix(w, 16).ok()));
    }
    list
}

/// Where Android looks for a device's key layout, in order.
pub fn layout_candidates(d: &Device) -> Vec<String> {
    let dirs = ["/odm/usr/keylayout", "/vendor/usr/keylayout", "/system/usr/keylayout", "/data/system/devices/keylayout"];
    let mut names = Vec::new();
    let generic = |s: &str| s.is_empty() || s.chars().all(|c| c == '0');
    if !(generic(&d.vendor) && generic(&d.product)) {
        if !generic(&d.version) {
            names.push(format!("Vendor_{}_Product_{}_Version_{}", d.vendor, d.product, d.version));
        }
        names.push(format!("Vendor_{}_Product_{}", d.vendor, d.product));
    }
    // the device name, with anything odd as '_'
    names.push(d.name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '_' }).collect());
    names.push("Generic".into());
    names.iter().flat_map(|n| dirs.iter().map(move |d| format!("{d}/{n}.kl"))).collect()
}

/// Scan code → key name, from a `.kl` file (`key 103 DPAD_UP`, flags after).
pub fn parse_layout(kl: &str) -> HashMap<u16, String> {
    kl.lines()
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            (w.next()? == "key").then_some(())?;
            let scan = w.next()?.parse().ok()?;
            Some((scan, w.next()?.to_string()))
        })
        .collect()
}

/// How to send keys raw on one TV, for as long as it stays up.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Map {
    /// The boot it was worked out in (device numbers can change on reboot).
    pub boot: String,
    /// 64-bit userspace: events are 24 bytes, not 16.
    pub wide: bool,
    /// Android key → (device path, scan code).
    pub keys: HashMap<u32, (String, u16)>,
}

impl Map {
    /// Work it out from the devices, their layouts (by device path), and
    /// which devices tveee may write to.
    pub fn build(devices: &[Device], layouts: &HashMap<String, HashMap<u16, String>>, writable: &[String]) -> Map {
        // what each device can give: Android key name → scan code
        let gives: Vec<(&Device, HashMap<&str, u16>)> = devices
            .iter()
            .filter(|d| writable.contains(&d.path))
            .filter_map(|d| {
                let kl = layouts.get(&d.path)?;
                let m: HashMap<&str, u16> = kl
                    .iter()
                    .filter(|(scan, _)| d.keys.contains(scan))
                    .map(|(scan, name)| (name.as_str(), *scan))
                    .collect();
                Some((d, m))
            })
            .collect();
        let wanted: Vec<(u32, String)> = (0..=200).filter_map(|c| name_of(c).map(|n| (c, n))).collect();
        // the device that gives the most of them goes first
        let mut order: Vec<&(&Device, HashMap<&str, u16>)> = gives.iter().collect();
        order.sort_by_key(|(d, m)| (std::cmp::Reverse(wanted.iter().filter(|(_, n)| m.contains_key(n.as_str())).count()), d.path.clone()));
        let mut keys = HashMap::new();
        for (code, name) in &wanted {
            if let Some((d, m)) = order.iter().find(|(_, m)| m.contains_key(name.as_str())) {
                keys.insert(*code, (d.path.clone(), m[name.as_str()]));
            }
        }
        Map { keys, ..Map::default() }
    }

    /// The shell command that presses `codes` (each down then up), or None
    /// if one of them can't be sent raw. With `shift_held`, Shift (on the
    /// same device) is held around a key.
    pub fn command(&self, presses: &[(u32, bool)]) -> Option<String> {
        let mut writes: Vec<(String, Vec<u8>)> = Vec::new();
        for &(code, shift) in presses {
            if NOT_RAW.contains(&code) {
                return None;
            }
            let (dev, scan) = self.keys.get(&code)?;
            let mut bytes = Vec::new();
            let shift_scan = match shift {
                true => match self.keys.get(&SHIFT) {
                    Some((d, s)) if d == dev => Some(*s),
                    _ => return None,
                },
                false => None,
            };
            if let Some(s) = shift_scan {
                bytes.extend(self.event(1, s, 1));
                bytes.extend(self.event(0, 0, 0));
            }
            bytes.extend(self.event(1, *scan, 1));
            bytes.extend(self.event(0, 0, 0));
            bytes.extend(self.event(1, *scan, 0));
            bytes.extend(self.event(0, 0, 0));
            if let Some(s) = shift_scan {
                bytes.extend(self.event(1, s, 0));
                bytes.extend(self.event(0, 0, 0));
            }
            // runs on one device go in one write
            match writes.last_mut() {
                Some((d, b)) if d == dev => b.extend(bytes),
                _ => writes.push((dev.clone(), bytes)),
            }
        }
        if writes.is_empty() {
            return None;
        }
        Some(
            writes
                .iter()
                .map(|(dev, b)| format!("print -n '{}' > {dev}", b.iter().map(|x| format!("\\x{x:02x}")).collect::<String>()))
                .collect::<Vec<_>>()
                .join("; "),
        )
    }

    /// One `struct input_event` (the kernel sets the time).
    fn event(&self, kind: u16, code: u16, value: i32) -> Vec<u8> {
        let mut b = vec![0u8; if self.wide { 16 } else { 8 }];
        b.extend(kind.to_le_bytes());
        b.extend(code.to_le_bytes());
        b.extend(value.to_le_bytes());
        b
    }

    /// The command that types `text`, if every character can be sent raw.
    pub fn text_command(&self, text: &str) -> Option<String> {
        let presses: Option<Vec<(u32, bool)>> = text.chars().map(key_for_char).collect();
        self.command(&presses?)
    }

    /// As saved in the cache file.
    pub fn render(&self) -> String {
        let mut out = format!("# how tveee sends keys raw to this TV (see src/keys.rs)\nboot {}\nwide {}\n", self.boot, u8::from(self.wide));
        let mut keys: Vec<_> = self.keys.iter().collect();
        keys.sort();
        for (code, (dev, scan)) in keys {
            out.push_str(&format!("key {code} {dev} {scan}\n"));
        }
        out
    }

    pub fn parse(text: &str) -> Option<Map> {
        let mut m = Map::default();
        for line in text.lines() {
            let w: Vec<&str> = line.split_whitespace().collect();
            match w.as_slice() {
                ["boot", b] => m.boot = b.to_string(),
                ["wide", x] => m.wide = *x == "1",
                ["key", code, dev, scan] => {
                    m.keys.insert(code.parse().ok()?, (dev.to_string(), scan.parse().ok()?));
                }
                _ => {}
            }
        }
        (!m.boot.is_empty()).then_some(m)
    }
}

const BOOT_ID: &str = "/proc/sys/kernel/random/boot_id";

/// Work out how to send keys raw on `tv` (two round trips).
pub fn discover(tv: &Tv) -> Result<Map> {
    let out = tv.shell(&[&format!(
        "cat {BOOT_ID}; getprop ro.product.cpu.abi; echo @@; getevent -i 2>/dev/null; echo @@; \
         for f in /dev/input/event*; do [ -w $f ] && echo $f; done"
    )])?;
    let parts: Vec<&str> = out.split("@@").collect();
    let head: Vec<&str> = parts.first().unwrap_or(&"").lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let devices = parse_getevent(parts.get(1).unwrap_or(&""));
    let writable: Vec<String> = parts.get(2).unwrap_or(&"").lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect();
    // each writable device's layout: the first candidate that exists
    let mut script = String::new();
    for d in devices.iter().filter(|d| writable.contains(&d.path) && !d.keys.is_empty()) {
        let files = layout_candidates(d).join(" ");
        script.push_str(&format!("for f in {files}; do [ -f $f ] && {{ echo '@@ {}'; grep '^key' $f; break; }}; done; ", d.path));
    }
    let mut layouts = HashMap::new();
    if !script.is_empty() {
        let out = tv.shell(&[&script])?;
        for chunk in out.split("@@ ").skip(1) {
            let (path, body) = chunk.split_once('\n').unwrap_or((chunk, ""));
            layouts.insert(path.trim().to_string(), parse_layout(body));
        }
    }
    let mut map = Map::build(&devices, &layouts, &writable);
    map.boot = head.first().unwrap_or(&"").to_string();
    map.wide = head.get(1).is_some_and(|abi| abi.contains("64"));
    Ok(map)
}

fn cache_file(ip: &str) -> Option<std::path::PathBuf> {
    Some(crate::plugin::cache_path(ip)?.with_extension("keys"))
}

/// The saved map for `tv`, if any (it may be from an earlier boot).
fn cached(tv: &Tv) -> Option<Map> {
    Map::parse(&std::fs::read_to_string(cache_file(&tv.ip)?).ok()?)
}

fn save(tv: &Tv, m: &Map) {
    if let Some(f) = cache_file(&tv.ip) {
        if let Some(dir) = f.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(f, m.render());
    }
}

/// `cmd`, but only if the TV is still in boot `boot` (else it prints "@@stale").
fn guarded(boot: &str, cmd: &str) -> String {
    format!("read b < {BOOT_ID}; if [ \"$b\" = '{boot}' ]; then {cmd}; else echo @@stale; fi")
}

/// Whether to send keys raw (the `fast-keys` option; off: always `input`).
fn fast() -> bool {
    crate::options::get(crate::options::FAST_KEYS)
}

/// Press `presses` on `tv` (one-off, e.g. from the command line): raw when it
/// can, else `input keyevent`.
pub fn press(tv: &Tv, presses: &[(u32, bool)]) -> Result<()> {
    let tries: &[bool] = if fast() { &[false, true] } else { &[] };
    for &fresh in tries {
        let map = match (fresh, cached(tv)) {
            (false, Some(m)) => m,
            (false, None) => continue,
            (true, _) => match discover(tv) {
                Ok(m) => {
                    save(tv, &m);
                    m
                }
                Err(_) => break,
            },
        };
        let Some(cmd) = map.command(presses) else { break };
        if !tv.shell(&[&guarded(&map.boot, &cmd)])?.contains("@@stale") {
            return Ok(());
        }
    }
    fallback(tv, presses)
}

/// Type `text` on `tv` (one-off): raw when every character can be, else
/// `input text`.
pub fn type_text(tv: &Tv, text: &str) -> Result<()> {
    let raw = fast().then(|| cached(tv).or_else(|| discover(tv).ok().inspect(|m| save(tv, m)))).flatten();
    if let Some(m) = raw {
        if let Some(cmd) = m.text_command(text) {
            if !tv.shell(&[&guarded(&m.boot, &cmd)])?.contains("@@stale") {
                return Ok(());
            }
            // another boot: work it out again next time
            if let Some(f) = cache_file(&tv.ip) {
                let _ = std::fs::remove_file(f);
            }
        }
    }
    tv.shell(&["input", "text", &crate::plugins::input::escape_text(text)]).map(drop)
}

fn fallback(tv: &Tv, presses: &[(u32, bool)]) -> Result<()> {
    let codes: Vec<String> = presses.iter().map(|(c, _)| c.to_string()).collect();
    let mut cmd = vec!["input", "keyevent"];
    cmd.extend(codes.iter().map(String::as_str));
    tv.shell(&cmd).map(drop)
}

/// A shell on the TV that stays open, for sending keys with as little delay
/// as possible (the ui): no adb start-up or connection per key, and keys
/// sent raw. Commands are written and not waited for.
#[cfg_attr(not(feature = "ui"), allow(dead_code))]
pub struct Session {
    tv: Tv,
    child: Option<std::process::Child>,
    map: Option<Map>,
}

#[cfg_attr(not(feature = "ui"), allow(dead_code))]
impl Session {
    pub fn new(tv: Tv) -> Session {
        Session { tv, child: None, map: None }
    }

    /// Get ready before the first key: the map, and the shell.
    pub fn warm(&mut self) {
        if fast() {
            let _ = self.map();
        }
        let _ = self.send("true");
    }

    fn map(&mut self) -> Option<&Map> {
        if self.map.is_none() {
            // this boot's map is fine as it is; otherwise work it out again
            let boot = self.tv.shell(&["cat", BOOT_ID]).ok().map(|b| b.trim().to_string());
            let m = match cached(&self.tv) {
                Some(m) if Some(&m.boot) == boot.as_ref() => Some(m),
                _ => discover(&self.tv).ok().inspect(|m| save(&self.tv, m)),
            };
            self.map = m;
        }
        self.map.as_ref()
    }

    fn send(&mut self, cmd: &str) -> Result<()> {
        for _ in 0..2 {
            if self.child.is_none() {
                self.tv.connect()?;
                self.child = Some(self.tv.shell_session()?);
            }
            let ok = self.child.as_mut().and_then(|c| c.stdin.as_mut()).is_some_and(|s| s.write_all(format!("{cmd}\n").as_bytes()).and_then(|()| s.flush()).is_ok());
            if ok {
                return Ok(());
            }
            // the shell went away (the TV rebooted, say): start again
            if let Some(mut c) = self.child.take() {
                let _ = c.kill();
                let _ = c.wait();
            }
            self.map = None;
        }
        crate::error::failed("couldn't keep a shell open on the TV")
    }

    /// Press keys (Shift held where asked).
    pub fn press(&mut self, presses: &[(u32, bool)]) -> Result<()> {
        // the option is read each time, so changing it takes effect at once
        let raw = if fast() { self.map().and_then(|m| m.command(presses)) } else { None };
        match raw {
            Some(cmd) => self.send(&cmd),
            None => {
                let codes: Vec<String> = presses.iter().map(|(c, _)| c.to_string()).collect();
                self.send(&format!("input keyevent {}", codes.join(" ")))
            }
        }
    }

    /// Type `text` (raw when every character can be, else `input text`).
    pub fn text(&mut self, text: &str) -> Result<()> {
        let raw = if fast() { self.map().and_then(|m| m.text_command(text)) } else { None };
        match raw {
            Some(cmd) => self.send(&cmd),
            None => self.send(&format!("input text {}", crate::plugins::input::escape_text(text))),
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            drop(c.stdin.take());
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GETEVENT: &str = "add device 1: /dev/input/event9
  bus:      0006
  vendor    0000
  product   0000
  version   0000
  name:     \"virtual-search\"
  location: \"\"
  id:       \"\"
  version:  1.0.1
  events:
    KEY (0001): 0067  006c
add device 2: /dev/input/event5
  bus:      0019
  vendor    0001
  product   0001
  version   0100
  name:     \"TPV-PTA\"
  location: \"TPV-PTA/input0\"
  id:       \"\"
  version:  1.0.1
  events:
    KEY (0001): 000e  001c  001e  002a  0067  006c
                0069  006a  009e  0160
    MSC (0004): 0004
  input props:
    <none>
";

    const KL: &str = "# comment
key 14    DEL
key 28    ENTER
key 30    A
key 42    SHIFT_LEFT
key 103     DPAD_UP
key 105     DPAD_LEFT
key 106     DPAD_RIGHT
key 108     DPAD_DOWN
key 158     BACK
key 352     DPAD_CENTER
key 116   POWER   WAKE
key usage 0x000c0067 SOMETHING
";

    #[test]
    fn reads_devices() {
        let d = parse_getevent(GETEVENT);
        assert_eq!(d.len(), 2);
        assert_eq!((d[1].path.as_str(), d[1].name.as_str(), d[1].vendor.as_str(), d[1].version.as_str()), ("/dev/input/event5", "TPV-PTA", "0001", "0100"));
        assert_eq!(d[1].keys, [0x0e, 0x1c, 0x1e, 0x2a, 0x67, 0x6c, 0x69, 0x6a, 0x9e, 0x160]);
        // looked for the way Android does: vendor/product, then the name, then Generic
        let c = layout_candidates(&d[1]);
        assert_eq!(c[0], "/odm/usr/keylayout/Vendor_0001_Product_0001_Version_0100.kl");
        assert!(c.contains(&"/system/usr/keylayout/TPV-PTA.kl".to_string()));
        assert_eq!(c.last().unwrap(), "/data/system/devices/keylayout/Generic.kl");
        assert!(!layout_candidates(&d[0])[0].contains("Vendor_0000"));
    }

    fn map() -> Map {
        let devices = parse_getevent(GETEVENT);
        let mut layouts = HashMap::new();
        layouts.insert("/dev/input/event5".to_string(), parse_layout(KL));
        // event9 declares up/down but has no layout read: not used
        let writable = vec!["/dev/input/event5".to_string(), "/dev/input/event9".to_string()];
        Map { boot: "b1".into(), ..Map::build(&devices, &layouts, &writable) }
    }

    #[test]
    fn maps_android_keys_to_scan_codes() {
        let m = map();
        assert_eq!(m.keys[&19], ("/dev/input/event5".to_string(), 103));
        assert_eq!(m.keys[&23].1, 352);
        assert_eq!(m.keys[&29].1, 30); // A
        // declared but not in the layout, or the other way round: not mapped
        assert!(!m.keys.contains_key(&26));
        // round trip through the cache file
        assert_eq!(Map::parse(&m.render()), Some(m));
    }

    #[test]
    fn builds_raw_writes() {
        let m = map();
        // up: down, sync, up, sync; 16 bytes each on 32-bit
        let cmd = m.command(&[(19, false)]).unwrap();
        assert!(cmd.starts_with("print -n '") && cmd.ends_with("' > /dev/input/event5"), "{cmd}");
        assert_eq!(cmd.matches("\\x").count(), 64);
        assert!(cmd.contains("\\x01\\x00\\x67\\x00\\x01\\x00\\x00\\x00"), "{cmd}");
        // 64-bit: 24 bytes an event
        let wide = Map { wide: true, ..m.clone() };
        assert_eq!(wide.command(&[(19, false)]).unwrap().matches("\\x").count(), 96);
        // several keys, one write
        assert_eq!(m.command(&[(19, false), (20, false), (23, false)]).unwrap().matches("print").count(), 1);
        // Shift held around a capital
        assert_eq!(m.text_command("A").unwrap().matches("\\x").count(), 128);
        assert_eq!(m.text_command("aA").unwrap().matches("\\x").count(), 192);
        // can't: a key no device has, a character without a fixed key, power
        assert_eq!(m.command(&[(19, false), (85, false)]), None);
        assert_eq!(m.text_command("a!"), None);
        assert_eq!(m.command(&[(26, false)]), None);
    }

    #[test]
    fn checks_the_boot() {
        let g = guarded("abc", "print -n 'x' > /dev/input/event5");
        assert!(g.starts_with("read b < /proc/sys/kernel/random/boot_id; if [ \"$b\" = 'abc' ]"), "{g}");
        assert!(g.ends_with("else echo @@stale; fi"));
    }
}

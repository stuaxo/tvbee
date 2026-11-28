//! `debloat`: disable / remove unwanted system apps and pin a few settings,
//! driven by a profile (a small text file; see `philips-qm163e.txt` for the
//! format). Everything is reversible with `debloat restore`.
//!
//! Every entry carries a [`Tag`] saying why it's there. `keep` entries are
//! tagged `system` or `dependency:<packages that need it>`: they are never
//! disabled, and `apply` re-enables them if something turned them off.
//! `speed` entries only apply on low-spec TVs (see [`Spec`]).

use crate::Ctx;
use crate::actions::ActionDescriptor;
use crate::error::{Result, failed, usage};
use crate::tv::Tv;
use std::collections::BTreeSet;
use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;

const CAT: &str = "debloat";

/// Built-in profiles: (model they're for, text).
const BUILTIN: &[(&str, &str)] = &[("QM163E", include_str!("philips-qm163e.txt"))];

/// Why an entry is in a profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tag {
    Telemetry,
    Voice,
    Discontinued,
    Sync,
    PhoneServices,
    Games,
    /// Only recommended on low-spec TVs.
    Speed,
    /// Kept: a core part of the system.
    System,
    /// Kept: these packages need it.
    Dependency {
        of: Vec<String>,
    },
}

impl Tag {
    /// Every tag id, in the order groups are shown.
    pub const IDS: &[&str] = &["telemetry", "voice", "discontinued", "sync", "phoneservices", "games", "speed", "system", "dependency"];

    pub fn id(&self) -> &'static str {
        match self {
            Tag::Telemetry => "telemetry",
            Tag::Voice => "voice",
            Tag::Discontinued => "discontinued",
            Tag::Sync => "sync",
            Tag::PhoneServices => "phoneservices",
            Tag::Games => "games",
            Tag::Speed => "speed",
            Tag::System => "system",
            Tag::Dependency { .. } => "dependency",
        }
    }

    /// `telemetry`, `dependency:a.b,c.d`, ...
    pub fn parse(s: &str) -> std::result::Result<Tag, String> {
        let (id, args) = s.split_once(':').unwrap_or((s, ""));
        Ok(match id {
            "telemetry" => Tag::Telemetry,
            "voice" => Tag::Voice,
            "discontinued" => Tag::Discontinued,
            "sync" => Tag::Sync,
            "phoneservices" => Tag::PhoneServices,
            "games" => Tag::Games,
            "speed" => Tag::Speed,
            "system" => Tag::System,
            "dependency" => {
                let of: Vec<String> = args.split(',').map(str::trim).filter(|p| !p.is_empty()).map(str::to_string).collect();
                if of.is_empty() {
                    return Err("dependency needs the packages that need it: dependency:<pkg>[,<pkg>...]".into());
                }
                Tag::Dependency { of }
            }
            _ => return Err(format!("unknown tag '{id}' (one of: {})", Tag::IDS.join(", "))),
        })
    }

    pub fn title(&self) -> &'static str {
        match self {
            Tag::Telemetry => "Telemetry and usage tracking",
            Tag::Voice => "Voice assistant",
            Tag::Discontinued => "Discontinued services",
            Tag::Sync => "Account sync",
            Tag::PhoneServices => "Phone services (no telephony on a TV)",
            Tag::Games => "Games",
            Tag::Speed => "Speed (low-spec TVs)",
            Tag::System => "Core system parts (never changed)",
            Tag::Dependency { .. } => "Needed by other apps (never changed)",
        }
    }

    /// What you give up by applying this group.
    pub fn loses(&self) -> &'static str {
        match self {
            Tag::Telemetry => "nothing visible; less data sent to the maker",
            Tag::Voice => "voice search, and the assistant if a remote with a mic is paired",
            Tag::Discontinued => "nothing: the services behind them have shut down",
            Tag::Sync => "nothing on a TV",
            Tag::PhoneServices => "nothing: there's no phone hardware",
            Tag::Games => "game sign-in and achievements, if a game uses them",
            Tag::Speed => "smooth animations (instant instead)",
            Tag::System | Tag::Dependency { .. } => "",
        }
    }

    /// Kept entries are never changed.
    pub fn is_keep(&self) -> bool {
        matches!(self, Tag::System | Tag::Dependency { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Disable { pkg: String },
    Uninstall { pkg: String },
    Keep { pkg: String },
    Setting { ns: String, key: String, value: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub action: Action,
    pub tag: Tag,
    pub why: String,
}

impl Entry {
    fn target(&self) -> String {
        match &self.action {
            Action::Disable { pkg } | Action::Uninstall { pkg } | Action::Keep { pkg } => pkg.clone(),
            Action::Setting { ns, key, .. } => format!("{ns}/{key}"),
        }
    }

    fn kind(&self) -> &'static str {
        match self.action {
            Action::Disable { .. } => "disable",
            Action::Uninstall { .. } => "uninstall",
            Action::Keep { .. } => "keep",
            Action::Setting { .. } => "setting",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Profile {
    pub name: String,
    pub model: Option<String>,
    pub entries: Vec<Entry>,
    pub notes: Vec<String>,
}

/// Split off the first whitespace-separated word.
fn word(s: &str) -> (&str, &str) {
    let s = s.trim();
    match s.split_once(char::is_whitespace) {
        Some((w, rest)) => (w, rest.trim()),
        None => (s, ""),
    }
}

/// Parse a profile, checking it makes sense: unknown lines or tags are
/// errors, `keep` takes only `system` / `dependency` tags (and changes can't
/// use them), and a package can't be both changed and kept.
pub fn parse_profile(text: &str) -> std::result::Result<Profile, String> {
    let mut p = Profile::default();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let at = |msg: &str| format!("line {}: {msg}: {line}", n + 1);
        let (kind, rest) = word(line);
        match kind {
            "profile" => p.name = rest.to_string(),
            "model" => p.model = Some(rest.to_string()),
            "note" => p.notes.push(rest.to_string()),
            "disable" | "uninstall" | "keep" | "setting" => {
                let (tag, rest) = word(rest);
                let tag = Tag::parse(tag).map_err(|e| at(&e))?;
                if (kind == "keep") != tag.is_keep() {
                    return Err(at(if kind == "keep" {
                        "keep entries are tagged system or dependency:<pkgs>"
                    } else {
                        "system / dependency tags are only for keep entries"
                    }));
                }
                let action = if kind == "setting" {
                    let (ns, rest) = word(rest);
                    let (key, rest) = word(rest);
                    let (value, why) = word(rest);
                    if !["global", "secure", "system"].contains(&ns) || key.is_empty() || value.is_empty() {
                        return Err(at("expected: setting <tag> <global|secure|system> <key> <value> <reason>"));
                    }
                    p.entries.push(Entry { action: Action::Setting { ns: ns.into(), key: key.into(), value: value.into() }, tag, why: why.into() });
                    continue;
                } else {
                    let (pkg, why) = word(rest);
                    if !pkg.contains('.') {
                        return Err(at("expected a package name"));
                    }
                    let pkg = pkg.to_string();
                    let action = match kind {
                        "disable" => Action::Disable { pkg },
                        "uninstall" => Action::Uninstall { pkg },
                        _ => Action::Keep { pkg },
                    };
                    (action, why.to_string())
                };
                p.entries.push(Entry { action: action.0, tag, why: action.1 });
            }
            _ => return Err(at("unknown entry")),
        }
    }
    let kept: BTreeSet<String> = p.entries.iter().filter(|e| e.tag.is_keep()).map(Entry::target).collect();
    for e in &p.entries {
        if matches!(e.action, Action::Disable { .. } | Action::Uninstall { .. }) && kept.contains(&e.target()) {
            return Err(format!("{} is listed to change but also to keep (other apps depend on it); remove one", e.target()));
        }
    }
    if p.name.is_empty() {
        p.name = "unnamed profile".into();
    }
    Ok(p)
}

/// What the TV says about its hardware, to judge whether it's low-spec.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Spec {
    pub make: String,
    pub model: String,
    pub ram_mb: Option<u64>,
    pub sdk: Option<u32>,
    pub release: String,
    pub abilist: String,
    pub low_ram_flag: bool,
    pub cores: Option<u32>,
    pub platform: String,
}

const SPEC_CMD: &str = "getprop ro.product.manufacturer; echo @@; getprop ro.product.model; echo @@; \
grep MemTotal /proc/meminfo; echo @@; getprop ro.build.version.sdk; echo @@; getprop ro.build.version.release; \
echo @@; getprop ro.product.cpu.abilist; echo @@; getprop ro.config.low_ram; echo @@; \
grep -c ^processor /proc/cpuinfo; echo @@; getprop ro.board.platform";

pub fn parse_spec(out: &str) -> Spec {
    let f: Vec<&str> = out.split("@@").map(str::trim).collect();
    let get = |i: usize| f.get(i).copied().unwrap_or("").to_string();
    Spec {
        make: get(0),
        model: get(1),
        ram_mb: get(2).split_whitespace().nth(1).and_then(|k| k.parse::<u64>().ok()).map(|k| k / 1024),
        sdk: get(3).parse().ok(),
        release: get(4),
        abilist: get(5),
        low_ram_flag: get(6) == "true",
        cores: get(7).parse().ok(),
        platform: get(8),
    }
}

impl Spec {
    /// Why this TV counts as low-spec (empty: it doesn't).
    pub fn low_spec_reasons(&self) -> Vec<String> {
        let mut why = Vec::new();
        if let Some(mb) = self.ram_mb.filter(|mb| *mb < 2048) {
            why.push(format!("{:.1} GB RAM", mb as f64 / 1024.0));
        }
        if !self.abilist.is_empty() && !self.abilist.contains("64") {
            why.push("32-bit CPU".into());
        }
        if self.low_ram_flag {
            why.push("Android's low-RAM flag".into());
        }
        if let Some(sdk) = self.sdk.filter(|s| *s < 28) {
            why.push(format!("Android {} (API {sdk})", self.release));
        }
        why
    }

    pub fn summary(&self) -> String {
        let ram = self.ram_mb.map_or("? RAM".into(), |mb| format!("{:.1} GB RAM", mb as f64 / 1024.0));
        let cores = self.cores.map_or(String::new(), |c| format!(", {c} cores"));
        let bits = if self.abilist.contains("64") {
            "64-bit"
        } else if self.abilist.is_empty() {
            "?"
        } else {
            "32-bit"
        };
        let api = self.sdk.map_or(String::new(), |s| format!(" (API {s})"));
        format!("{ram}{cores}, {bits} {}, Android {}{api}", self.platform, self.release)
    }
}

/// A package's state for user 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Enabled,
    Disabled,
    /// On the system image, but removed for user 0.
    Uninstalled,
    Absent,
}

impl State {
    fn word(self) -> &'static str {
        match self {
            State::Enabled => "enabled",
            State::Disabled => "disabled",
            State::Uninstalled => "removed",
            State::Absent => "absent",
        }
    }
}

/// Package states from one round trip (see [`STATE_CMD`]).
#[derive(Debug, Default)]
pub struct States {
    installed: BTreeSet<String>,
    disabled: BTreeSet<String>,
    known: BTreeSet<String>,
}

// `cmd package`, not `pm`: the same lists without starting a Java VM each time
const STATE_CMD: &str = "cmd package list packages --user 0; echo @@; cmd package list packages -d --user 0; echo @@; cmd package list packages -u";

pub fn parse_states(out: &str) -> States {
    let mut parts = out.split("@@").map(|p| p.lines().filter_map(|l| l.trim().strip_prefix("package:")).map(str::to_string).collect::<BTreeSet<_>>());
    States { installed: parts.next().unwrap_or_default(), disabled: parts.next().unwrap_or_default(), known: parts.next().unwrap_or_default() }
}

impl States {
    pub fn of(&self, pkg: &str) -> State {
        if self.disabled.contains(pkg) {
            State::Disabled
        } else if self.installed.contains(pkg) {
            State::Enabled
        } else if self.known.contains(pkg) {
            State::Uninstalled
        } else {
            State::Absent
        }
    }
}

/// Which groups to act on: `--only a,b` and `--skip c`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Selection {
    pub only: Option<Vec<String>>,
    pub skip: Vec<String>,
}

impl Selection {
    /// Parse a comma-separated tag list, checking the names.
    pub fn tags(s: &str) -> std::result::Result<Vec<String>, String> {
        s.split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(|t| if Tag::IDS.contains(&t) { Ok(t.to_string()) } else { Err(format!("unknown group '{t}' (one of: {})", Tag::IDS.join(", "))) })
            .collect()
    }

    fn picks(&self, tag: &Tag) -> bool {
        !self.skip.iter().any(|s| s == tag.id()) && self.only.as_ref().is_none_or(|o| o.iter().any(|s| s == tag.id()))
    }

    fn named(&self, tag: &Tag) -> bool {
        self.only.as_ref().is_some_and(|o| o.iter().any(|s| s == tag.id()))
    }
}

/// One line of a plan: what it is, how it is now, and the change (if any).
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub tag: Tag,
    pub kind: &'static str,
    pub target: String,
    pub now: String,
    /// What will happen, e.g. "disable"; None when nothing needs doing.
    pub change: Option<String>,
    /// Why (from the profile), or why not.
    pub note: String,
    /// Shell commands that make the change.
    pub cmds: Vec<String>,
    /// For settings: (namespace, key, value before) to record on first change.
    pub record: Option<(String, String, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Apply,
    Restore,
}

/// What the plan needs to know about the TV.
pub struct Tvstate<'a> {
    pub packages: &'a States,
    pub setting: &'a dyn Fn(&str, &str) -> String,
    pub originals: &'a [(String, String, String)],
    /// Why it's low-spec (empty: it isn't).
    pub low_spec: &'a [String],
}

/// `org.droidtv.nettv.market` -> `nettv.market`: the package without its
/// vendor prefix (the first two parts), for notes.
pub fn short_pkg(pkg: &str) -> &str {
    match pkg.match_indices('.').nth(1) {
        Some((i, _)) if i + 1 < pkg.len() => &pkg[i + 1..],
        _ => pkg,
    }
}

/// The plan for `apply` or `restore`.
pub fn plan(p: &Profile, tv: &Tvstate, sel: &Selection, dir: Direction) -> Vec<Step> {
    let mut out = Vec::new();
    for e in &p.entries {
        if !e.tag.is_keep() && !sel.picks(&e.tag) {
            continue;
        }
        let mut s = Step {
            tag: e.tag.clone(),
            kind: e.kind(),
            target: e.target(),
            now: String::new(),
            change: None,
            note: e.why.clone(),
            cmds: Vec::new(),
            record: None,
        };
        let set = |s: &mut Step, change: &str, cmds: Vec<String>| {
            s.change = Some(change.into());
            s.cmds = cmds;
        };
        match (&e.action, dir) {
            (Action::Disable { pkg }, Direction::Apply) => {
                let st = tv.packages.of(pkg);
                s.now = st.word().into();
                match st {
                    State::Enabled => set(&mut s, "disable", vec![format!("pm disable-user --user 0 {pkg}"), format!("am force-stop {pkg}")]),
                    State::Absent => s.note = "not on this TV".into(),
                    _ => {}
                }
            }
            (Action::Disable { pkg }, Direction::Restore) => {
                let st = tv.packages.of(pkg);
                s.now = st.word().into();
                if st == State::Disabled {
                    set(&mut s, "enable", vec![format!("pm enable {pkg}")]);
                }
            }
            (Action::Uninstall { pkg }, Direction::Apply) => {
                let st = tv.packages.of(pkg);
                s.now = st.word().into();
                if matches!(st, State::Enabled | State::Disabled) {
                    set(&mut s, "remove for user 0", vec![format!("pm uninstall --user 0 {pkg}")]);
                }
            }
            (Action::Uninstall { pkg }, Direction::Restore) => {
                let st = tv.packages.of(pkg);
                s.now = st.word().into();
                if st == State::Uninstalled {
                    set(&mut s, "reinstall for user 0", vec![format!("cmd package install-existing {pkg}")]);
                }
            }
            // kept packages must be on, whichever way we're going
            (Action::Keep { pkg }, _) => {
                let st = tv.packages.of(pkg);
                s.now = st.word().into();
                if let Tag::Dependency { of } = &e.tag {
                    let needed: Vec<&str> = of.iter().map(|d| short_pkg(d)).collect();
                    let present = of.iter().filter(|d| tv.packages.of(d) != State::Absent).count();
                    let none = if present == 0 { "; none installed here" } else { "" };
                    s.note = format!("{} [needed by {}{none}]", e.why, needed.join(", "));
                }
                match st {
                    State::Disabled => set(&mut s, "re-enable (needed)", vec![format!("pm enable {pkg}")]),
                    State::Uninstalled => set(&mut s, "reinstall (needed)", vec![format!("cmd package install-existing {pkg}")]),
                    _ => {}
                }
            }
            (Action::Setting { ns, key, value }, Direction::Apply) => {
                let cur = (tv.setting)(ns, key);
                s.now = cur.clone();
                if cur == *value {
                } else if e.tag == Tag::Speed && tv.low_spec.is_empty() && !sel.named(&e.tag) {
                    s.note = "skipped: this TV isn't low-spec (--only speed to apply anyway)".into();
                } else {
                    set(&mut s, &format!("set to {value}"), vec![format!("settings put {ns} {key} {value}")]);
                    s.record = Some((ns.clone(), key.clone(), cur));
                }
            }
            (Action::Setting { ns, key, .. }, Direction::Restore) => {
                let cur = (tv.setting)(ns, key);
                s.now = cur.clone();
                match tv.originals.iter().find(|(n, k, _)| n == ns && k == key) {
                    None => s.note = "no original recorded".into(),
                    Some((_, _, orig)) if *orig == cur => s.note = "already the original".into(),
                    Some((_, _, orig)) if orig == "null" => set(&mut s, "delete (was unset)", vec![format!("settings delete {ns} {key}")]),
                    Some((_, _, orig)) => set(&mut s, &format!("set back to {orig}"), vec![format!("settings put {ns} {key} {orig}")]),
                }
            }
        }
        out.push(s);
    }
    out
}

/// `ns key value` lines.
pub fn parse_originals(text: &str) -> Vec<(String, String, String)> {
    text.lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            Some((f.next()?.to_string(), f.next()?.to_string(), f.next()?.to_string()))
        })
        .collect()
}

/// `~/.config/tveee` (or the platform's equivalent).
fn config_dir() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    crate::dotenv::files(&cwd).into_iter().nth(1)?.parent().map(PathBuf::from)
}

fn originals_file(ip: &str) -> Option<PathBuf> {
    Some(config_dir()?.join(format!("debloat-settings-{ip}.txt")))
}

/// Print a plan grouped by tag; returns how many steps change something.
fn print_plan(steps: &[Step]) -> usize {
    let wide = steps.iter().map(|s| s.target.len()).max().unwrap_or(0).min(44);
    let mut changes = 0;
    for id in Tag::IDS {
        let group: Vec<&Step> = steps.iter().filter(|s| s.tag.id() == *id).collect();
        let Some(first) = group.first() else { continue };
        let loses = first.tag.loses();
        println!("\n {id}: {}{}", first.tag.title(), if loses.is_empty() { String::new() } else { format!("  (you lose: {loses})") });
        for s in group {
            let (mark, what) = match &s.change {
                Some(c) => {
                    changes += 1;
                    ("*", c.clone())
                }
                None => (" ", "-".to_string()),
            };
            println!("  {mark} {:<9} {:<wide$}  {:<9} {:<22} {}", s.kind, s.target, s.now, what, s.note);
        }
    }
    changes
}

/// The profile to use: `--profile FILE`, else `~/.config/tveee/debloat.txt`,
/// else a built-in one for `model`.
fn load_profile(file: Option<&str>, model: Option<&str>) -> Result<(Profile, String)> {
    let user = config_dir().map(|d| d.join("debloat.txt")).filter(|f| f.is_file());
    let (text, from) = match (file, user) {
        (Some(f), _) => (std::fs::read_to_string(f).map_err(|e| crate::error::Error::Failed(format!("debloat: {f}: {e}")))?, f.to_string()),
        (None, Some(f)) => (std::fs::read_to_string(&f)?, f.display().to_string()),
        (None, None) => {
            let text = match model {
                Some(m) => BUILTIN.iter().find(|(bm, _)| *bm == m).map(|(_, t)| *t).ok_or_else(|| {
                    crate::error::Error::Failed(format!(
                        "debloat: no built-in profile for a {m}; start one with 'tveee debloat list --raw > ~/.config/tveee/debloat.txt'"
                    ))
                })?,
                None => BUILTIN[0].1,
            };
            (text.to_string(), "built-in".to_string())
        }
    };
    let p = parse_profile(&text).map_err(|e| crate::error::Error::Failed(format!("debloat: {from}: {e}")))?;
    Ok((p, from))
}

fn setting_values(tv: &Tv, p: &Profile) -> Result<Vec<((String, String), String)>> {
    let keys: Vec<(String, String)> =
        p.entries.iter().filter_map(|e| if let Action::Setting { ns, key, .. } = &e.action { Some((ns.clone(), key.clone())) } else { None }).collect();
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let cmd = keys.iter().map(|(n, k)| format!("settings get {n} {k}")).collect::<Vec<_>>().join("; echo @@; ");
    let out = tv.shell(&[&cmd])?;
    let vals: Vec<String> = out.split("@@").map(|v| v.trim().to_string()).collect();
    Ok(keys.into_iter().zip(vals.into_iter().chain(std::iter::repeat(String::new()))).map(|(k, v)| (k, if v.is_empty() { "null".into() } else { v })).collect())
}

fn free_data(tv: &Tv) -> String {
    let sys = super::info::read_system(tv).unwrap_or_default();
    super::info::interesting_disks(&sys.disks)
        .into_iter()
        .find(|(l, _)| *l == "internal")
        .map_or_else(|| "unknown".into(), |(l, d)| super::info::disk_summary(l, d))
}

/// Ask before changing the TV. `--yes` skips the question; without a
/// terminal there's no one to ask, so it refuses.
fn confirm(question: &str, yes: bool) -> Result<()> {
    if yes {
        return Ok(());
    }
    if !std::io::stdin().is_terminal() {
        return usage("debloat: this changes the TV; run it in a terminal to confirm, or pass --yes");
    }
    print!("{question} [type yes]: ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    if answer.trim().eq_ignore_ascii_case("yes") { Ok(()) } else { failed("debloat: not confirmed, nothing changed") }
}

struct Opts {
    raw: bool,
    dry: bool,
    yes: bool,
    force: bool,
    profile: Option<String>,
    sel: Selection,
}

fn parse_opts(args: &[String]) -> Result<Opts> {
    let mut o = Opts { raw: false, dry: false, yes: false, force: false, profile: None, sel: Selection::default() };
    let mut it = args.iter();
    let need = |v: Option<&String>, flag: &str| v.cloned().ok_or_else(|| crate::error::Error::Usage(format!("debloat: {flag} needs a value")));
    while let Some(a) = it.next() {
        match a.as_str() {
            "-n" | "--dry-run" => o.dry = true,
            "--yes" | "-y" => o.yes = true,
            "--force" => o.force = true,
            "--raw" => o.raw = true,
            "--profile" => o.profile = Some(need(it.next(), "--profile")?),
            "--only" => o.sel.only = Some(Selection::tags(&need(it.next(), "--only")?).map_err(|e| crate::error::Error::Usage(format!("debloat: {e}")))?),
            "--skip" => o.sel.skip = Selection::tags(&need(it.next(), "--skip")?).map_err(|e| crate::error::Error::Usage(format!("debloat: {e}")))?,
            x => return usage(format!("debloat: unknown option '{x}'")),
        }
    }
    Ok(o)
}

fn list(o: &Opts) -> Result<()> {
    if o.raw && o.profile.is_none() {
        // the built-in profile as text, comments and all: a starting point
        print!("{}", BUILTIN[0].1);
        return Ok(());
    }
    let (p, from) = load_profile(o.profile.as_deref(), None)?;
    println!("{} ({from}){}", p.name, p.model.as_ref().map_or(String::new(), |m| format!(", for {m}")));
    for id in Tag::IDS {
        let group: Vec<&Entry> = p.entries.iter().filter(|e| e.tag.id() == *id).collect();
        let Some(first) = group.first() else { continue };
        let loses = first.tag.loses();
        println!("\n {id}: {}{}", first.tag.title(), if loses.is_empty() { String::new() } else { format!("  (you lose: {loses})") });
        for e in group {
            let what = match &e.action {
                Action::Setting { value, .. } => format!("{} = {value}", e.target()),
                _ => e.target(),
            };
            let needed = match &e.tag {
                Tag::Dependency { of } => format!("  [needed by {}]", of.join(", ")),
                _ => String::new(),
            };
            println!("    {:<9} {what:<44} {}{needed}", e.kind(), e.why);
        }
    }
    for n in &p.notes {
        println!("\n  note: {n}");
    }
    if o.profile.is_none() {
        println!("\nA profile of your own in ~/.config/tveee/debloat.txt (or --profile FILE) replaces this one.");
        println!("To start one: tveee debloat list --raw > ~/.config/tveee/debloat.txt (the format is in its comments).");
    }
    Ok(())
}

fn run(c: &Ctx, dir: Option<Direction>, o: &Opts) -> Result<()> {
    let tv = &c.tv;
    let spec = parse_spec(&tv.shell(&[SPEC_CMD])?);
    let (p, from) = load_profile(o.profile.as_deref(), Some(&spec.model))?;
    let other_tv = p.model.as_ref().is_some_and(|m| *m != spec.model);
    let packages = parse_states(&tv.shell(&[STATE_CMD])?);
    let vals = setting_values(tv, &p)?;
    let setting = |ns: &str, key: &str| vals.iter().find(|((n, k), _)| n == ns && k == key).map_or("null".into(), |(_, v)| v.clone());
    let backup = originals_file(&tv.ip);
    let originals = backup.as_ref().and_then(|f| std::fs::read_to_string(f).ok()).map(|t| parse_originals(&t)).unwrap_or_default();
    let low = spec.low_spec_reasons();

    println!("TV:      {} {} {}", tv.ip, spec.make, spec.model);
    println!("spec:    {}{}", spec.summary(), if low.is_empty() { String::new() } else { format!("  -> low-spec ({})", low.join(", ")) });
    println!("profile: {} ({from})", p.name);
    if other_tv {
        println!("WARNING: this profile is for a {}, not this {}.", p.model.as_deref().unwrap_or("?"), spec.model);
    }
    println!("/data:   {}", free_data(tv));
    let state = Tvstate { packages: &packages, setting: &setting, originals: &originals, low_spec: &low };
    let steps = plan(&p, &state, &o.sel, dir.unwrap_or(Direction::Apply));
    let changes = print_plan(&steps);
    if dir == Some(Direction::Restore) {
        for n in &p.notes {
            println!("\n  note: {n}");
        }
    }
    let Some(dir) = dir else {
        println!("\n{changes} item(s) marked '*' would change with 'tveee debloat apply'.");
        return Ok(());
    };
    if changes == 0 {
        println!("\nnothing to change");
        return Ok(());
    }
    if o.dry {
        println!("\ndry run: {changes} item(s) marked '*' would change; nothing was touched.");
        return Ok(());
    }
    if other_tv && !o.force {
        return failed("debloat: profile made for a different TV; check the list above, then pass --force to use it anyway");
    }

    let saved = backup.as_ref().map_or("(nowhere: no config dir)".into(), |f| f.display().to_string());
    let groups: BTreeSet<&str> = steps.iter().filter(|s| s.change.is_some()).map(|s| s.tag.id()).collect();
    let groups: Vec<&str> = groups.into_iter().collect();
    println!();
    match dir {
        Direction::Apply => {
            println!("WARNING: this will change {changes} thing(s) on the TV ({}):", groups.join(", "));
            println!("it disables or removes system apps and changes settings.");
            println!("  - It's reversible ('tveee debloat restore'), but a disabled app stops working, and so");
            println!("    may anything that quietly depends on it. Check the list above.");
            println!("  - 'system' and 'dependency' apps are ones the TV or other apps need; they're never disabled.");
            println!("  - Original setting values are saved in {saved}.");
        }
        Direction::Restore => {
            println!("WARNING: this will change {changes} thing(s) on the TV ({}):", groups.join(", "));
            println!("it re-enables and reinstalls the apps above and sets settings back to their originals.");
            println!("  - The re-enabled apps (assistant, telemetry, ...) start running again.");
            println!("  - 'tveee debloat apply' puts the debloat back.");
        }
    }
    confirm(&format!("Change {changes} item(s) on {}?", tv.ip), o.yes)?;

    // record setting originals before the first change (first value wins)
    if let Some(f) = &backup {
        let mut known = originals.clone();
        for (ns, key, old) in steps.iter().filter_map(|s| s.record.clone()) {
            if !known.iter().any(|(n, k, _)| *n == ns && *k == key) {
                known.push((ns, key, old));
            }
        }
        if let Some(dir) = f.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(f, known.iter().map(|(n, k, v)| format!("{n} {k} {v}\n")).collect::<String>())?;
    }

    let mut failures = 0;
    for s in steps.iter().filter(|s| s.change.is_some()) {
        let mut err = None;
        for cmd in &s.cmds {
            match tv.shell(&[cmd]) {
                Ok(out) => {
                    if let Some(l) = out.lines().find(|l| l.contains("Error") || l.contains("Failure") || l.contains("Exception")) {
                        err = Some(l.trim().to_string());
                        break;
                    }
                }
                Err(e) => {
                    err = Some(e.to_string());
                    break;
                }
            }
        }
        match err {
            None => println!("  done    {} {}", s.change.as_deref().unwrap_or(""), s.target),
            Some(e) => {
                failures += 1;
                println!("  FAILED  {} {}: {e}", s.change.as_deref().unwrap_or(""), s.target);
            }
        }
    }
    if dir == Direction::Apply {
        tv.shell(&["pm trim-caches 999999999999"])?;
        println!("\ntrimmed caches; /data: {}", free_data(tv));
    }
    if failures > 0 {
        return failed(format!("debloat: {failures} change(s) failed"));
    }
    Ok(())
}

fn debloat(c: &Ctx, args: &[String]) -> Result<()> {
    let (sub, rest) = match args.first().map(String::as_str) {
        Some(s) if !s.starts_with('-') => (s, &args[1..]),
        _ => ("status", args),
    };
    let o = parse_opts(rest)?;
    match sub {
        "list" => list(&o),
        "status" => run(c, None, &o),
        "apply" => run(c, Some(Direction::Apply), &o),
        "restore" => run(c, Some(Direction::Restore), &o),
        "trim" => {
            if o.dry {
                println!("would run: pm trim-caches (frees app caches on /data)");
                return Ok(());
            }
            c.tv.shell(&["pm trim-caches 999999999999"])?;
            println!("trimmed caches; /data: {}", free_data(&c.tv));
            Ok(())
        }
        x => usage(format!("debloat: unknown action '{x}' (status, list, apply, restore, trim)")),
    }
}

pub fn actions() -> Vec<ActionDescriptor> {
    vec![
        ActionDescriptor::terminal("debloat.debloat", CAT, "disable unwanted system apps (reversible)", debloat)
            .cli(
                "debloat",
                "[status|list|apply|restore|trim] [-n] [--only G,..] [--skip G,..] [--yes] [--force] [--profile FILE]",
                "disable unwanted system apps (reversible)",
            )
            .long(
                "Disables or removes (for user 0 only) system apps a profile lists, and pins a\n\
         few settings; 'restore' undoes it. Nothing is deleted and no root is needed.\n\
         \n\
         \x20 status    what the profile covers and its state on the TV (the default)\n\
         \x20 list      the profile itself, by group, with reasons (no TV needed);\n\
         \x20           --raw prints the built-in one as a file to start your own from\n\
         \x20 apply     make the changes, after a warning and a 'yes'; trims caches after\n\
         \x20 restore   re-enable / reinstall everything, set settings back\n\
         \x20 trim      just free app caches on /data\n\
         \n\
         Groups: telemetry, voice, discontinued, sync, phoneservices, games, speed\n\
         (only on low-spec TVs), and system / dependency (never changed).\n\
         \x20 --only G,..     just these groups, e.g. --only telemetry,discontinued\n\
         \x20 --skip G,..     everything but these, e.g. --skip voice\n\
         \x20 -n, --dry-run   show what would change; touch nothing\n\
         \x20 --yes           don't ask (for scripts)\n\
         \x20 --force         use a profile made for a different TV model\n\
         \x20 --profile FILE  use FILE instead of ~/.config/tveee/debloat.txt or the built-in\n\
         \n\
         'dependency' entries record what needs them (on Philips TVs, disabling\n\
         nettvregistration breaks iPlayer, Arte and the app gallery). 'speed' changes\n\
         apply when the TV looks low-spec: under 2 GB RAM, a 32-bit CPU, Android's\n\
         low-RAM flag, or Android older than 9; '--only speed' applies them anyway.",
            ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_profiles_parse() {
        for (model, text) in BUILTIN {
            let p = parse_profile(text).unwrap();
            assert_eq!(p.model.as_deref(), Some(*model));
            let count = |id: &str| p.entries.iter().filter(|e| e.tag.id() == id).count();
            assert_eq!(
                Tag::IDS.iter().map(|id| count(id)).collect::<Vec<_>>(),
                vec![2, 3, 3, 2, 2, 1, 3, 4, 3],
                "telemetry, voice, discontinued, sync, phoneservices, games, speed, system, dependency"
            );
            assert_eq!(p.notes.len(), 2);
            assert!(p.entries.iter().all(|e| !e.why.is_empty()));
        }
    }

    #[test]
    fn tags() {
        assert_eq!(Tag::parse("voice"), Ok(Tag::Voice));
        assert_eq!(Tag::parse("dependency:a.b, c.d"), Ok(Tag::Dependency { of: vec!["a.b".into(), "c.d".into()] }));
        assert!(Tag::parse("dependency").is_err());
        assert!(Tag::parse("bloat").unwrap_err().contains("unknown tag"));
        for id in Tag::IDS {
            let t = Tag::parse(if *id == "dependency" { "dependency:x.y" } else { id }).unwrap();
            assert_eq!(t.id(), *id);
            assert!(!t.title().is_empty());
        }
    }

    #[test]
    fn profiles_are_checked() {
        // the mistake the dependency entries are there to stop
        let bad = "disable telemetry org.droidtv.nettvregistration x\nkeep dependency:org.droidtv.nettv_launcherapp org.droidtv.nettvregistration needed\n";
        assert!(parse_profile(bad).unwrap_err().contains("also to keep"));
        assert!(parse_profile("disable voice com.x.y reason\nkeep voice com.a.b why").unwrap_err().contains("keep entries are tagged"));
        assert!(parse_profile("disable system com.x.y reason").unwrap_err().contains("only for keep"));
        assert!(parse_profile("disabel voice com.x.y oops").unwrap_err().contains("line 1"));
        assert!(parse_profile("setting speed nowhere k v why").is_err());
        assert!(parse_profile("disable voice notapackage").is_err());
    }

    #[test]
    fn judges_low_spec() {
        // this TV
        let out = "PHILIPS @@ QM163E @@ MemTotal:        1375036 kB @@ 26 @@ 8.0.0 @@ armeabi-v7a,armeabi @@  @@ 4 @@ mt5890";
        let s = parse_spec(out);
        assert_eq!((s.model.as_str(), s.ram_mb, s.sdk, s.cores), ("QM163E", Some(1342), Some(26), Some(4)));
        assert_eq!(s.low_spec_reasons(), vec!["1.3 GB RAM", "32-bit CPU", "Android 8.0.0 (API 26)"]);
        assert_eq!(s.summary(), "1.3 GB RAM, 4 cores, 32-bit mt5890, Android 8.0.0 (API 26)");
        // a recent Google TV
        let s = parse_spec("X @@ Y @@ MemTotal: 3900000 kB @@ 33 @@ 13 @@ arm64-v8a,armeabi-v7a,armeabi @@ false @@ 4 @@ amlogic");
        assert!(s.low_spec_reasons().is_empty());
        let s = parse_spec("X @@ Y @@ MemTotal: 3900000 kB @@ 33 @@ 13 @@ arm64-v8a @@ true @@ 4 @@ z");
        assert_eq!(s.low_spec_reasons(), vec!["Android's low-RAM flag"]);
    }

    fn states() -> States {
        parse_states(
            "package:a.enabled\npackage:a.disabled\npackage:keep.off\npackage:gone.notyet\npackage:needs.it\n@@\npackage:a.disabled\npackage:keep.off\n@@\npackage:a.enabled\npackage:a.disabled\npackage:keep.off\npackage:gone.notyet\npackage:gone.already\npackage:needs.it\n",
        )
    }

    fn profile() -> Profile {
        parse_profile(
            "profile t\n\
             disable telemetry a.enabled r1\n\
             disable voice a.disabled r2\n\
             disable games not.here r3\n\
             uninstall discontinued gone.notyet r4\n\
             uninstall discontinued gone.already r5\n\
             keep dependency:needs.it keep.off needed\n\
             setting speed global anim 0.5 faster\n",
        )
        .unwrap()
    }

    fn changes(steps: &[Step]) -> Vec<(String, String)> {
        steps.iter().filter_map(|s| s.change.clone().map(|c| (s.target.clone(), c))).collect()
    }

    fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    #[test]
    fn reads_package_states() {
        let s = states();
        assert_eq!(s.of("a.enabled"), State::Enabled);
        assert_eq!(s.of("a.disabled"), State::Disabled);
        assert_eq!(s.of("gone.already"), State::Uninstalled);
        assert_eq!(s.of("not.here"), State::Absent);
    }

    #[test]
    fn plans_apply_and_restore() {
        let st = states();
        let one = |_: &str, _: &str| "1.0".to_string();
        let low = vec!["1.3 GB RAM".to_string()];
        let tv = Tvstate { packages: &st, setting: &one, originals: &[], low_spec: &low };
        let ap = plan(&profile(), &tv, &Selection::default(), Direction::Apply);
        assert_eq!(
            changes(&ap),
            pairs(&[("a.enabled", "disable"), ("gone.notyet", "remove for user 0"), ("keep.off", "re-enable (needed)"), ("global/anim", "set to 0.5")])
        );
        assert_eq!(ap[0].cmds, vec!["pm disable-user --user 0 a.enabled", "am force-stop a.enabled"]);
        assert_eq!(ap[5].note, "needed [needed by needs.it]");
        assert_eq!(short_pkg("org.droidtv.nettv.market"), "nettv.market");
        assert_eq!(short_pkg("com.android.bluetooth"), "bluetooth");
        assert_eq!(short_pkg("a.b"), "a.b");
        assert_eq!(ap[6].record, Some(("global".into(), "anim".into(), "1.0".into())));

        let orig = vec![("global".to_string(), "anim".to_string(), "null".to_string())];
        let half = |_: &str, _: &str| "0.5".to_string();
        let tv = Tvstate { packages: &st, setting: &half, originals: &orig, low_spec: &low };
        let back = plan(&profile(), &tv, &Selection::default(), Direction::Restore);
        assert_eq!(
            changes(&back),
            pairs(&[
                ("a.disabled", "enable"),
                ("gone.already", "reinstall for user 0"),
                ("keep.off", "re-enable (needed)"),
                ("global/anim", "delete (was unset)")
            ])
        );
    }

    #[test]
    fn groups_and_low_spec() {
        let st = states();
        let one = |_: &str, _: &str| "1.0".to_string();
        let fast: Vec<String> = vec![];
        let tv = Tvstate { packages: &st, setting: &one, originals: &[], low_spec: &fast };
        // a capable TV: the speed setting is skipped unless asked for
        let ap = plan(&profile(), &tv, &Selection::default(), Direction::Apply);
        let speed = ap.iter().find(|s| s.tag == Tag::Speed).unwrap();
        assert!(speed.change.is_none() && speed.note.contains("isn't low-spec"));
        let only_speed = Selection { only: Some(vec!["speed".into()]), skip: vec![] };
        let ap = plan(&profile(), &tv, &only_speed, Direction::Apply);
        // only speed (and the always-checked keep entries)
        assert_eq!(changes(&ap), pairs(&[("keep.off", "re-enable (needed)"), ("global/anim", "set to 0.5")]));
        // --skip leaves a group out
        let skip = Selection { only: None, skip: vec!["telemetry".into(), "discontinued".into()] };
        let ap = plan(&profile(), &tv, &skip, Direction::Apply);
        assert!(ap.iter().all(|s| !matches!(s.tag, Tag::Telemetry | Tag::Discontinued)));
        assert!(Selection::tags("telemetry,voice").is_ok());
        assert!(Selection::tags("telemetry,bloat").is_err());
    }

    #[test]
    fn reads_recorded_originals() {
        // the old script's settings-backup.txt has the same format
        let t = "global window_animation_scale 0.0\nglobal animator_duration_scale 1.0\n";
        assert_eq!(parse_originals(t)[1], ("global".into(), "animator_duration_scale".into(), "1.0".into()));
    }
}

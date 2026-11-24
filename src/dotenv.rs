//! Settings from `.env` files, so `TV_IP` and friends needn't be exported.
//!
//! Looked for in order: `./.env`, then `$XDG_CONFIG_HOME/tveee/env`
//! (`~/.config/tveee/env`; `%APPDATA%\tveee\env` on Windows). The first file to set a key wins, and anything
//! already in the real environment wins over both. Only `TV_*` and `TVEEE_*`
//! keys are read: a project's `.env` may hold unrelated secrets, and tveee
//! shouldn't pick those up or pass them on to scripts it runs.

use std::path::{Path, PathBuf};

/// `KEY=value` pairs from one file's text, in order. Accepts `export KEY=...`,
/// `#` comments (whole-line, or after an unquoted value), and single or double
/// quotes around the value.
pub fn parse(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let line = line.strip_prefix("export ").map_or(line, str::trim_start);
            let (key, value) = line.split_once('=')?;
            let key = key.trim();
            if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                return None;
            }
            Some((key.to_string(), unquote(value.trim())))
        })
        .collect()
}

fn unquote(v: &str) -> String {
    for q in ['"', '\''] {
        if let Some((inner, end)) = v.strip_prefix(q).and_then(|i| i.find(q).map(|e| (i, e))) {
            return inner[..end].to_string();
        }
    }
    // unquoted: a ` #` starts a comment
    v.split(" #").next().unwrap_or(v).trim().to_string()
}

/// Keys tveee reads; everything else in a `.env` is left alone. (`TVBEE_*`
/// are the same settings under tveee's old name, see [`old_name`].)
pub fn ours(key: &str) -> bool {
    key.starts_with("TV_") || key.starts_with("TVEEE_") || key.starts_with(OLD_PREFIX)
}

/// tveee was called tvbee: its settings were `TVBEE_*`.
const OLD_PREFIX: &str = "TVBEE_";

/// The current name of a setting under the old name (`TVBEE_PLUGINS` ->
/// `TVEEE_PLUGINS`), if it is one.
pub fn old_name(key: &str) -> Option<String> {
    key.strip_prefix(OLD_PREFIX).map(|rest| format!("TVEEE_{rest}"))
}

/// The folders tveee keeps things in (config, cache), each with its parent.
fn base_dirs() -> Vec<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .or_else(|| std::env::var_os("APPDATA").map(PathBuf::from));
    let cache = crate::plugin::cache_path("x").and_then(|p| p.parent()?.parent().map(PathBuf::from));
    config.into_iter().chain(cache).collect()
}

/// tveee was called tvbee: move its folders (`~/.config/tvbee`,
/// `~/.cache/tvbee`) to the new name, once, if there's nothing there yet.
pub fn move_old_folders() {
    for base in base_dirs() {
        let (old, new) = (base.join("tvbee"), base.join("tveee"));
        if old.is_dir() && !new.exists() {
            let _ = std::fs::rename(&old, &new);
            // the cache database, under its new name too
            let _ = std::fs::rename(new.join("tvbee.sqlite"), new.join("tveee.sqlite"));
        }
    }
}

/// The files to read, in order of preference.
pub fn files(cwd: &Path) -> Vec<PathBuf> {
    // XDG first, then ~/.config, then %APPDATA% on Windows
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .or_else(|| std::env::var_os("APPDATA").map(PathBuf::from));
    std::iter::once(cwd.join(".env")).chain(config.map(|c| c.join("tveee").join("env"))).collect()
}

/// tveee's config folder (`~/.config/tveee`, or the platform's equivalent),
/// where the user-wide env file, debloat settings and pairings live.
pub fn config_dir() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    files(&cwd).into_iter().nth(1)?.parent().map(PathBuf::from)
}

/// What to set: our keys from `files`, first file winning, skipping keys
/// `is_set` says the real environment already has.
pub fn settings(files: &[PathBuf], is_set: impl Fn(&str) -> bool) -> Vec<(String, String, PathBuf)> {
    let mut out: Vec<(String, String, PathBuf)> = Vec::new();
    for f in files {
        let Ok(text) = std::fs::read_to_string(f) else { continue };
        for (k, v) in parse(&text) {
            if ours(&k) && !is_set(&k) && !out.iter().any(|(seen, _, _)| *seen == k) {
                out.push((k, v, f.clone()));
            }
        }
    }
    out
}

/// Which key came from which file, as loaded (for `tveee diagnose`).
static LOADED: std::sync::OnceLock<Vec<(String, PathBuf)>> = std::sync::OnceLock::new();

/// Load `.env` settings into the environment. Call first thing in `main`,
/// before any threads exist.
pub fn load() {
    let Ok(cwd) = std::env::current_dir() else { return };
    let mut loaded = Vec::new();
    for (k, v, file) in settings(&files(&cwd), |k| std::env::var_os(k).is_some()) {
        // SAFETY: called at the very start of main, while the process is
        // still single-threaded, so nothing else can be reading the
        // environment concurrently.
        unsafe { std::env::set_var(&k, v) };
        loaded.push((k, file));
    }
    // settings under the old name count as the new ones (unless those are set)
    let old: Vec<(String, std::ffi::OsString)> =
        std::env::vars_os().filter_map(|(k, v)| Some((old_name(k.to_str()?)?, v))).collect();
    for (k, v) in old {
        if std::env::var_os(&k).is_none() {
            // SAFETY: as above, still single-threaded
            unsafe { std::env::set_var(&k, v) };
        }
    }
    let _ = LOADED.set(loaded);
}

/// The file `key` was loaded from, if it came from one.
pub fn source_of(key: &str) -> Option<PathBuf> {
    LOADED.get()?.iter().find(|(k, _)| k == key).map(|(_, f)| f.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_names() {
        assert_eq!(old_name("TVBEE_PLUGINS").as_deref(), Some("TVEEE_PLUGINS"));
        assert_eq!(old_name("TVEEE_PLUGINS"), None);
        assert_eq!(old_name("TV_IP"), None);
        assert!(ours("TVBEE_PLUGINS") && ours("TVEEE_PLUGINS") && ours("TV_IP") && !ours("SECRET"));
    }

    #[test]
    fn parses_env_files() {
        let text = "
# the TV
TV_IP=192.168.1.50
export TVEEE_PLUGINS = philips
TV_DEBLOAT=\"/opt/my scripts/tv-debloat.sh\"  # quoted
TV_X='single # not a comment'
TV_Y=plain # a comment
not a setting
=nokey
BAD KEY=1
";
        assert_eq!(
            parse(text),
            vec![
                ("TV_IP".into(), "192.168.1.50".into()),
                ("TVEEE_PLUGINS".into(), "philips".into()),
                ("TV_DEBLOAT".into(), "/opt/my scripts/tv-debloat.sh".into()),
                ("TV_X".into(), "single # not a comment".into()),
                ("TV_Y".into(), "plain".into()),
            ]
        );
    }

    #[test]
    fn precedence() {
        let dir = std::env::temp_dir().join(format!("tveee-dotenv-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (local, user) = (dir.join("local.env"), dir.join("user.env"));
        std::fs::write(&local, "TV_IP=1.1.1.1\nAWS_SECRET=nope\n").unwrap();
        std::fs::write(&user, "TV_IP=2.2.2.2\nTV_DEBLOAT=/x.sh\nTVEEE_PLUGINS=philips\n").unwrap();
        let files = vec![local.clone(), dir.join("missing"), user.clone()];
        // the first file wins; other people's keys are ignored; the real
        // environment wins over both
        let got = settings(&files, |k| k == "TVEEE_PLUGINS");
        let keys: Vec<_> = got.iter().map(|(k, v, f)| (k.as_str(), v.as_str(), f == &local)).collect();
        assert_eq!(keys, vec![("TV_IP", "1.1.1.1", true), ("TV_DEBLOAT", "/x.sh", false)]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn looks_in_cwd_then_config() {
        let f = files(Path::new("/some/dir"));
        assert_eq!(f[0], PathBuf::from("/some/dir/.env"));
        assert!(f.get(1).is_some_and(|p| p.ends_with("tveee/env")));
    }
}

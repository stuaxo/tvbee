//! Settings from `.env` files, so `TV_IP` and friends needn't be exported.
//!
//! Looked for in order: `./.env`, then `$XDG_CONFIG_HOME/tvbee/env`
//! (`~/.config/tvbee/env`). The first file to set a key wins, and anything
//! already in the real environment wins over both. Only `TV_*` and `TVBEE_*`
//! keys are read: a project's `.env` may hold unrelated secrets, and tvbee
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
        if let Some(inner) = v.strip_prefix(q)
            && let Some(end) = inner.find(q)
        {
            return inner[..end].to_string();
        }
    }
    // unquoted: a ` #` starts a comment
    v.split(" #").next().unwrap_or(v).trim().to_string()
}

/// Keys tvbee reads; everything else in a `.env` is left alone.
pub fn ours(key: &str) -> bool {
    key.starts_with("TV_") || key.starts_with("TVBEE_")
}

/// The files to read, in order of preference.
pub fn files(cwd: &Path) -> Vec<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
    std::iter::once(cwd.join(".env")).chain(config.map(|c| c.join("tvbee").join("env"))).collect()
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

/// Load `.env` settings into the environment. Call first thing in `main`,
/// before any threads exist.
pub fn load() {
    let Ok(cwd) = std::env::current_dir() else { return };
    for (k, v, _) in settings(&files(&cwd), |k| std::env::var_os(k).is_some()) {
        // SAFETY: called at the very start of main, while the process is
        // still single-threaded, so nothing else can be reading the
        // environment concurrently.
        unsafe { std::env::set_var(k, v) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_env_files() {
        let text = "
# the TV
TV_IP=192.168.1.50
export TVBEE_PLUGINS = philips
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
                ("TVBEE_PLUGINS".into(), "philips".into()),
                ("TV_DEBLOAT".into(), "/opt/my scripts/tv-debloat.sh".into()),
                ("TV_X".into(), "single # not a comment".into()),
                ("TV_Y".into(), "plain".into()),
            ]
        );
    }

    #[test]
    fn precedence() {
        let dir = std::env::temp_dir().join(format!("tvbee-dotenv-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (local, user) = (dir.join("local.env"), dir.join("user.env"));
        std::fs::write(&local, "TV_IP=1.1.1.1\nAWS_SECRET=nope\n").unwrap();
        std::fs::write(&user, "TV_IP=2.2.2.2\nTV_DEBLOAT=/x.sh\nTVBEE_PLUGINS=philips\n").unwrap();
        let files = vec![local.clone(), dir.join("missing"), user.clone()];
        // the first file wins; other people's keys are ignored; the real
        // environment wins over both
        let got = settings(&files, |k| k == "TVBEE_PLUGINS");
        let keys: Vec<_> = got.iter().map(|(k, v, f)| (k.as_str(), v.as_str(), f == &local)).collect();
        assert_eq!(keys, vec![("TV_IP", "1.1.1.1", true), ("TV_DEBLOAT", "/x.sh", false)]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn looks_in_cwd_then_config() {
        let f = files(Path::new("/some/dir"));
        assert_eq!(f[0], PathBuf::from("/some/dir/.env"));
        assert!(f.get(1).is_some_and(|p| p.ends_with("tvbee/env")));
    }
}

//! Favorites (bookmarked folders/archives), stored as one UTF-8 path per line in the
//! per-user config directory:
//! * Windows: `%APPDATA%\Archiver\favorites.txt`
//! * macOS: `~/Library/Application Support/Archiver/favorites.txt`
//! * other Unix: `$XDG_CONFIG_HOME/archiver/favorites.txt` or `~/.config/archiver/favorites.txt`
//!
//! `ARCHIVER_CONFIG_DIR` overrides the directory (tests, portable installs).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

const FILE: &str = "favorites.txt";

/// Config directory computed from an environment lookup (injectable for tests).
pub fn config_dir_from(env: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let get = |k: &str| env(k).filter(|v| !v.is_empty());
    if let Some(d) = get("ARCHIVER_CONFIG_DIR") {
        return Some(PathBuf::from(d));
    }
    if cfg!(windows) {
        get("APPDATA").map(|d| PathBuf::from(d).join("Archiver"))
    } else if cfg!(target_os = "macos") {
        get("HOME").map(|h| PathBuf::from(h).join("Library/Application Support/Archiver"))
    } else {
        get("XDG_CONFIG_HOME")
            .map(|d| PathBuf::from(d).join("archiver"))
            .or_else(|| get("HOME").map(|h| PathBuf::from(h).join(".config/archiver")))
    }
}

pub fn config_dir() -> Option<PathBuf> {
    config_dir_from(|k| std::env::var_os(k))
}

fn key(p: &Path) -> String {
    let s = p.to_string_lossy();
    let s = s.trim_end_matches(['/', '\\']);
    if cfg!(windows) { s.to_lowercase() } else { s.to_string() }
}

fn no_dir() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::NotFound, "no per-user config directory available")
}

/// Favorites stored in `dir` (de-duplicated, order kept; blank lines ignored).
pub fn load_from(dir: &Path) -> Vec<PathBuf> {
    let text = std::fs::read(dir.join(FILE)).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
    let mut seen = std::collections::HashSet::new();
    text.lines()
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.trim().is_empty())
        .map(PathBuf::from)
        .filter(|p| seen.insert(key(p)))
        .collect()
}

fn save_to(dir: &Path, list: &[PathBuf]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut text = String::new();
    for p in list {
        let s = p.to_string_lossy();
        if s.contains(['\n', '\r']) {
            continue; // cannot be stored one per line
        }
        text.push_str(&s);
        text.push('\n');
    }
    let tmp = dir.join(format!("{FILE}.tmp"));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, dir.join(FILE))
}

/// Add `path` (no-op when already present).
pub fn add_to(dir: &Path, path: &Path) -> std::io::Result<()> {
    let mut list = load_from(dir);
    if list.iter().any(|p| key(p) == key(path)) {
        return Ok(());
    }
    list.push(path.to_path_buf());
    save_to(dir, &list)
}

/// Remove `path` (no-op when absent).
pub fn remove_from(dir: &Path, path: &Path) -> std::io::Result<()> {
    let list = load_from(dir);
    let kept: Vec<PathBuf> = list.iter().filter(|p| key(p) != key(path)).cloned().collect();
    if kept.len() == list.len() {
        return Ok(());
    }
    save_to(dir, &kept)
}

pub fn load() -> Vec<PathBuf> {
    config_dir().map(|d| load_from(&d)).unwrap_or_default()
}

pub fn add(path: &Path) -> std::io::Result<()> {
    add_to(&config_dir().ok_or_else(no_dir)?, path)
}

pub fn remove(path: &Path) -> std::io::Result<()> {
    remove_from(&config_dir().ok_or_else(no_dir)?, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_remove_dedup_roundtrip() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("cfg"); // created on first save
        assert!(load_from(&d).is_empty());
        add_to(&d, Path::new("/data/專案 一")).unwrap();
        add_to(&d, Path::new("/data/b.zip")).unwrap();
        add_to(&d, Path::new("/data/專案 一")).unwrap();
        add_to(&d, Path::new("/data/專案 一/")).unwrap();
        assert_eq!(load_from(&d), vec![PathBuf::from("/data/專案 一"), PathBuf::from("/data/b.zip")]);
        let raw = std::fs::read_to_string(d.join(FILE)).unwrap();
        assert_eq!(raw, "/data/專案 一\n/data/b.zip\n");
        remove_from(&d, Path::new("/data/專案 一")).unwrap();
        remove_from(&d, Path::new("/never/added")).unwrap();
        assert_eq!(load_from(&d), vec![PathBuf::from("/data/b.zip")]);
        remove_from(&d, Path::new("/data/b.zip")).unwrap();
        assert!(load_from(&d).is_empty());
    }

    #[test]
    fn load_tolerates_blank_lines_crlf_and_duplicates() {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join(FILE), "\u{feff}".trim_start_matches('\u{feff}').to_string() + "/a\r\n\r\n  \n/b\n/a\n").unwrap();
        assert_eq!(load_from(t.path()), vec![PathBuf::from("/a"), PathBuf::from("/b")]);
    }

    #[cfg(windows)]
    #[test]
    fn windows_paths_compare_case_insensitively() {
        let t = tempfile::tempdir().unwrap();
        add_to(t.path(), Path::new(r"C:DataDocs")).unwrap();
        add_to(t.path(), Path::new(r"c:datadocs")).unwrap();
        assert_eq!(load_from(t.path()).len(), 1);
        remove_from(t.path(), Path::new(r"C:DATADOCS")).unwrap();
        assert!(load_from(t.path()).is_empty());
    }

    #[test]
    fn config_dir_resolution() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| pairs.iter().find(|p| p.0 == k).map(|p| OsString::from(p.1))
        };
        assert_eq!(config_dir_from(env(&[("ARCHIVER_CONFIG_DIR", "/x/y"), ("APPDATA", "/z"), ("HOME", "/h")])), Some(PathBuf::from("/x/y")));
        assert_eq!(config_dir_from(env(&[("ARCHIVER_CONFIG_DIR", ""), ("APPDATA", "/z"), ("HOME", "/h")])).is_some(), true);
        assert_eq!(config_dir_from(env(&[])), None);
        let d = config_dir_from(env(&[("APPDATA", "/ad"), ("HOME", "/h"), ("XDG_CONFIG_HOME", "/xdg")])).unwrap();
        if cfg!(windows) {
            assert_eq!(d, Path::new("/ad").join("Archiver"));
        } else if cfg!(target_os = "macos") {
            assert_eq!(d, Path::new("/h").join("Library/Application Support/Archiver"));
        } else {
            assert_eq!(d, Path::new("/xdg").join("archiver"));
            assert_eq!(config_dir_from(env(&[("HOME", "/h")])).unwrap(), Path::new("/h").join(".config/archiver"));
        }
    }
}

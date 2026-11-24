//! Persistent GUI settings (`settings.json` next to `favorites.txt` in the per-user config
//! directory, see [`crate::favorites::config_dir`]). Unknown or invalid values fall back to the
//! defaults, so an old or hand-edited file never prevents start-up.

use std::path::{Path, PathBuf};

use archiver_core::Overwrite;
use serde_json::{Value, json};

pub const FILE: &str = "settings.json";
/// Entries kept in the File menu's recent-archives list.
pub const MAX_RECENT: usize = 8;

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// Index into `Format::writable()` last used in the create dialog.
    pub last_format: usize,
    pub preview_visible: bool,
    /// Overwrite policy for every extraction started from the GUI or the shell menu.
    pub overwrite: Overwrite,
    /// Create dialog: test the new archive after creating it.
    pub test_after: bool,
    /// Most recently opened archives, newest first, at most [`MAX_RECENT`].
    pub recent: Vec<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { last_format: 0, preview_visible: true, overwrite: Overwrite::Replace, test_after: false, recent: vec![] }
    }
}

/// Same identity rule as the favorites list: trailing slashes ignored, case-insensitive on Windows.
fn key(p: &Path) -> String {
    let s = p.to_string_lossy();
    let s = s.trim_end_matches(['/', '\\']);
    if cfg!(windows) { s.to_lowercase() } else { s.to_string() }
}

/// Settings stored in `dir` (defaults when missing or unreadable).
pub fn load_from(dir: &Path) -> Settings {
    let Ok(text) = std::fs::read_to_string(dir.join(FILE)) else { return Settings::default() };
    let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(text.trim_start_matches('\u{feff}')) else {
        return Settings::default();
    };
    let d = Settings::default();
    let mut recent: Vec<PathBuf> = Vec::new();
    for s in obj.get("recent").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
        if !s.trim().is_empty() && !recent.iter().any(|q| key(q) == key(Path::new(s))) {
            recent.push(PathBuf::from(s));
        }
    }
    recent.truncate(MAX_RECENT);
    Settings {
        last_format: obj.get("last_format").and_then(Value::as_u64).and_then(|n| usize::try_from(n).ok()).unwrap_or(d.last_format),
        preview_visible: obj.get("preview_visible").and_then(Value::as_bool).unwrap_or(d.preview_visible),
        overwrite: obj.get("overwrite").and_then(Value::as_str).and_then(Overwrite::from_key).unwrap_or(d.overwrite),
        test_after: obj.get("test_after").and_then(Value::as_bool).unwrap_or(d.test_after),
        recent,
    }
}

/// Write `s` to `dir` (created when missing), atomically (temp file + rename).
pub fn save_to(dir: &Path, s: &Settings) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let recent: Vec<Value> = s.recent.iter().take(MAX_RECENT).map(|p| Value::String(p.to_string_lossy().into_owned())).collect();
    let v = json!({
        "last_format": s.last_format,
        "preview_visible": s.preview_visible,
        "overwrite": s.overwrite.key(),
        "test_after": s.test_after,
        "recent": recent,
    });
    let text = serde_json::to_string_pretty(&v).map_err(std::io::Error::other)?;
    // unique temp name: concurrent processes (or threads) must not clobber each other's temp file
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let tmp = dir.join(format!("{FILE}.{}-{nanos}.tmp", std::process::id()));
    let r = std::fs::write(&tmp, text).and_then(|()| std::fs::rename(&tmp, dir.join(FILE)));
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r
}

pub fn load() -> Settings {
    crate::favorites::config_dir().map(|d| load_from(&d)).unwrap_or_default()
}

pub fn save(s: &Settings) -> std::io::Result<()> {
    let dir = crate::favorites::config_dir().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no per-user config directory available"))?;
    save_to(&dir, s)
}

/// Move `p` to the front of `list` (removing duplicates; case-insensitive on Windows) and
/// keep at most [`MAX_RECENT`] entries.
pub fn push_recent(list: &mut Vec<PathBuf>, p: &Path) {
    let k = key(p);
    list.retain(|q| key(q) != k);
    list.insert(0, p.to_path_buf());
    list.truncate(MAX_RECENT);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Settings {
        Settings {
            last_format: 2,
            preview_visible: false,
            overwrite: Overwrite::Rename,
            test_after: true,
            recent: vec![PathBuf::from("/a/b.zip"), PathBuf::from("/c/專案 一.7z")],
        }
    }

    #[test]
    fn round_trip() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("cfg"); // created on first save
        save_to(&d, &sample()).unwrap();
        assert_eq!(load_from(&d), sample());
        let leftovers = |d: &Path| std::fs::read_dir(d).unwrap().filter_map(Result::ok).filter(|e| e.file_name().to_string_lossy().ends_with(".tmp")).count();
        assert_eq!(leftovers(&d), 0);
        // second save replaces the first file
        save_to(&d, &Settings::default()).unwrap();
        assert_eq!(load_from(&d), Settings::default());
    }

    #[test]
    fn missing_file_gives_defaults() {
        let t = tempfile::tempdir().unwrap();
        assert_eq!(load_from(t.path()), Settings::default());
    }

    #[test]
    fn garbage_file_gives_defaults() {
        for text in ["not json {", "[1,2,3]", "", "\"just a string\"", "\u{0}\u{1}"] {
            let t = tempfile::tempdir().unwrap();
            std::fs::write(t.path().join(FILE), text).unwrap();
            assert_eq!(load_from(t.path()), Settings::default(), "input {text:?}");
        }
    }

    #[test]
    fn bad_keys_fall_back_one_by_one() {
        let t = tempfile::tempdir().unwrap();
        let text = r#"{"last_format":"x","preview_visible":1,"overwrite":"bogus","test_after":true,"recent":"nope"}"#;
        std::fs::write(t.path().join(FILE), text).unwrap();
        let s = load_from(t.path());
        assert_eq!(s.last_format, 0);
        assert!(s.preview_visible);
        assert_eq!(s.overwrite, Overwrite::Replace);
        assert!(s.test_after);
        assert!(s.recent.is_empty());
    }

    #[test]
    fn unknown_overwrite_key_is_replace_and_known_key_parses() {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join(FILE), r#"{"overwrite":"  SKIP "}"#).unwrap();
        assert_eq!(load_from(t.path()).overwrite, Overwrite::Skip);
        std::fs::write(t.path().join(FILE), r#"{"overwrite":"unknown"}"#).unwrap();
        assert_eq!(load_from(t.path()).overwrite, Overwrite::Replace);
    }

    #[test]
    fn bom_is_tolerated() {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join(FILE), "\u{feff}{\"test_after\":true}").unwrap();
        assert!(load_from(t.path()).test_after);
    }

    #[test]
    fn recent_loaded_deduped_and_capped() {
        let t = tempfile::tempdir().unwrap();
        let mut items: Vec<String> = (0..12).map(|i| format!("/x/{i}.zip")).collect();
        items.insert(1, "/x/0.zip".to_string()); // duplicate
        items.insert(2, String::new()); // blank entries are dropped
        items.insert(3, "  ".to_string());
        let text = json!({ "recent": items }).to_string();
        std::fs::write(t.path().join(FILE), text).unwrap();
        let s = load_from(t.path());
        assert_eq!(s.recent.len(), MAX_RECENT);
        assert_eq!(s.recent[0], PathBuf::from("/x/0.zip"));
        assert_eq!(s.recent[1], PathBuf::from("/x/1.zip"));
    }

    #[test]
    fn push_recent_moves_to_front_and_caps() {
        let mut list = Vec::new();
        for i in 0..(MAX_RECENT + 3) {
            push_recent(&mut list, Path::new(&format!("/f/{i}.zip")));
        }
        assert_eq!(list.len(), MAX_RECENT);
        assert_eq!(list[0], PathBuf::from(format!("/f/{}.zip", MAX_RECENT + 2)));
        // re-opening an entry moves it to the front without duplicating it
        let old = PathBuf::from(format!("/f/{}.zip", MAX_RECENT + 1));
        push_recent(&mut list, &old);
        assert_eq!(list.len(), MAX_RECENT);
        assert_eq!(list[0], old);
        assert_eq!(list.iter().filter(|p| **p == old).count(), 1);
    }

    #[cfg(windows)]
    #[test]
    fn push_recent_ignores_case_and_trailing_slash_on_windows() {
        let mut list = vec![PathBuf::from(r"C:\Data\A.zip")];
        push_recent(&mut list, Path::new(r"c:\data\a.zip"));
        assert_eq!(list, vec![PathBuf::from(r"c:\data\a.zip")]);
        push_recent(&mut list, Path::new(r"C:\Dir\"));
        push_recent(&mut list, Path::new(r"c:\dir"));
        assert_eq!(list, vec![PathBuf::from(r"c:\dir"), PathBuf::from(r"c:\data\a.zip")]);
    }
}

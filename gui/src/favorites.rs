//! Favorites (bookmarked folders/archives), stored as one path per line in the
//! per-user config directory. CONTRACT STUB (workstream B implements).

use std::path::{Path, PathBuf};

pub fn load() -> Vec<PathBuf> {
    Vec::new()
}

pub fn add(path: &Path) -> std::io::Result<()> {
    let _ = path;
    Ok(())
}

pub fn remove(path: &Path) -> std::io::Result<()> {
    let _ = path;
    Ok(())
}

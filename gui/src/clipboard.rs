//! System clipboard: files (pasted as a copy in Explorer / Finder / file managers) and text.

use std::path::PathBuf;

/// Long-lived clipboard handle. Keep one for the whole session: on X11/Wayland the owning
/// process has to keep serving the selection, so the handle must not be dropped after a copy.
pub struct Clip {
    inner: Option<arboard::Clipboard>,
}

impl Default for Clip {
    fn default() -> Self {
        Self::new()
    }
}

fn clip_err(e: arboard::Error) -> String {
    format!("無法存取剪貼簿：{e}")
}

impl Clip {
    pub fn new() -> Self {
        Clip { inner: arboard::Clipboard::new().ok() }
    }

    /// The live handle, created again when the first attempt failed (e.g. no display yet).
    fn handle(&mut self) -> Result<&mut arboard::Clipboard, String> {
        if self.inner.is_none() {
            self.inner = Some(arboard::Clipboard::new().map_err(clip_err)?);
        }
        self.inner.as_mut().ok_or_else(|| "無法存取剪貼簿".to_string())
    }

    /// Put `paths` on the clipboard as a file list.
    pub fn copy_files(&mut self, paths: &[PathBuf]) -> Result<(), String> {
        if paths.is_empty() {
            return Err("沒有可複製的檔案".into());
        }
        // `absolute` (not `canonicalize`): no `\\?\` prefix on Windows, which file managers reject.
        let abs: Vec<PathBuf> = paths.iter().map(|p| std::path::absolute(p).unwrap_or_else(|_| p.clone())).collect();
        self.handle()?.set().file_list(&abs).map_err(clip_err)
    }

    pub fn copy_text(&mut self, text: &str) -> Result<(), String> {
        self.handle()?.set_text(text).map_err(clip_err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_list_is_rejected_without_clipboard() {
        let mut clip = Clip { inner: None };
        assert_eq!(clip.copy_files(&[]), Err("沒有可複製的檔案".to_string()));
    }

    #[test]
    #[ignore = "needs a display and touches the system clipboard"]
    fn file_list_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.txt");
        std::fs::write(&f, b"x").unwrap();
        let mut clip = Clip::new();
        clip.copy_files(std::slice::from_ref(&f)).unwrap();
        let got = arboard::Clipboard::new().unwrap().get().file_list().unwrap();
        assert_eq!(got, vec![f]);
    }
}

//! In-place editing of existing archives: comments and adding files.
//! CONTRACT STUB (workstream B implements the bodies; signatures are fixed).

use std::path::{Path, PathBuf};

use crate::{CreateOptions, Error, Progress, Result};

/// Archive comment (ZIP only today). `Ok(None)` when the archive has no comment
/// or the format cannot store one.
pub fn read_comment(archive: &Path) -> Result<Option<String>> {
    let _ = archive;
    Err(Error::Unsupported("read_comment: not implemented yet".into()))
}

/// Replace the archive comment (ZIP only; other formats return `Error::Unsupported`).
pub fn set_comment(archive: &Path, comment: &str, p: &Progress) -> Result<()> {
    let _ = (archive, comment, p);
    Err(Error::Unsupported("set_comment: not implemented yet".into()))
}

/// Add files/folders to an existing archive under the in-archive folder `prefix`
/// ("" = root, otherwise "dir/sub" without trailing slash). ZIP appends in place;
/// other writable formats are rewritten through a temp file. RAR: Unsupported.
pub fn add(archive: &Path, inputs: &[PathBuf], prefix: &str, opts: &CreateOptions, p: &Progress) -> Result<()> {
    let _ = (archive, inputs, prefix, opts, p);
    Err(Error::Unsupported("add: not implemented yet".into()))
}

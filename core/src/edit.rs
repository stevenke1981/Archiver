//! In-place editing of existing archives: comments and adding files.

use std::path::{Path, PathBuf};

use crate::archive_ops::tempdir_near;
use crate::create::{collect, zip_write_items};
use crate::*;

/// Archive comment (ZIP only today). `Ok(None)` when the archive has no comment
/// or the format cannot store one.
pub fn read_comment(archive: &Path) -> Result<Option<String>> {
    if detect_or_err(archive)? != Format::Zip {
        return Ok(None);
    }
    let z = zip::ZipArchive::new(BufReader::new(File::open(archive)?))?;
    let raw = z.comment();
    if raw.is_empty() {
        return Ok(None);
    }
    Ok(Some(String::from_utf8_lossy(raw).into_owned()))
}

/// Replace the archive comment (ZIP only; other formats return `Error::Unsupported`).
/// An empty `comment` removes it. Only the central directory is rewritten.
pub fn set_comment(archive: &Path, comment: &str, p: &Progress) -> Result<()> {
    let fmt = detect_or_err(archive)?;
    if fmt != Format::Zip {
        return Err(Error::Unsupported(format!("{} cannot store an archive comment", fmt.label())));
    }
    p.check()?;
    let f = fs::OpenOptions::new().read(true).write(true).open(archive)?;
    let mut z = zip::ZipWriter::new_append(f)?;
    z.set_raw_comment(comment.as_bytes().to_vec().into_boxed_slice())?;
    let mut f = z.finish()?;
    // the new central directory may be shorter than the old one
    use io::Seek;
    let end = f.stream_position()?;
    f.set_len(end)?;
    f.flush()?;
    Ok(())
}

/// Add files/folders to an existing archive under the in-archive folder `prefix`
/// ("" = root, otherwise "dir/sub" without trailing slash).
///
/// * ZIP: existing entries are raw-copied (no recompression, existing encryption and the archive
///   comment are kept) into a temp file next to the archive, the new entries are written with
///   `opts.level` / `opts.password`, then the temp file atomically replaces the archive.
/// * tar.*, 7z: rewritten through a temp file in the same directory (existing entries are
///   decompressed with `opts.password` and re-encoded with the archive's own format).
/// * single-file formats (gz, bz2, ...) and RAR: `Error::Unsupported`.
///
/// An added entry with the same in-archive name as an existing one replaces it.
/// `opts.format` is ignored: the archive keeps its own format.
pub fn add(archive: &Path, inputs: &[PathBuf], prefix: &str, opts: &CreateOptions, p: &Progress) -> Result<()> {
    let fmt = detect_or_err(archive)?;
    if !fmt.can_create() || fmt.is_single_file() {
        return Err(Error::Unsupported(format!("cannot add files to {}", fmt.label())));
    }
    if inputs.is_empty() {
        return Ok(());
    }
    let prefix = norm_name(prefix);
    if prefix.split('/').any(|c| c == "..") {
        return Err(Error::Unsupported("invalid prefix".into()));
    }
    let mut items = collect(inputs)?;
    if !prefix.is_empty() {
        for it in &mut items {
            it.1 = format!("{prefix}/{}", it.1);
        }
    }
    let total: u64 = items.iter().filter(|i| !i.2).map(|i| fs::metadata(&i.0).map(|m| m.len()).unwrap_or(0)).sum();
    let done = AtomicU64::new(0);
    let tmp = {
        let mut n = archive.as_os_str().to_owned();
        n.push(".add.tmp");
        PathBuf::from(n)
    };
    let opts = CreateOptions { format: fmt, level: opts.level, password: opts.password.clone() };
    let res = if fmt == Format::Zip {
        add_zip(archive, &tmp, &items, &opts, &done, total, p)
    } else {
        add_rewrite(archive, &tmp, &items, &opts, p)
    };
    match res {
        Ok(()) => {
            fs::rename(&tmp, archive)?;
            Ok(())
        }
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

fn add_zip(archive: &Path, tmp: &Path, items: &[(PathBuf, String, bool)], opts: &CreateOptions, done: &AtomicU64, total: u64, p: &Progress) -> Result<()> {
    let mut src = zip::ZipArchive::new(BufReader::with_capacity(256 << 10, File::open(archive)?))?;
    let mut out = zip::ZipWriter::new(wr(File::create(tmp)?));
    if !src.comment().is_empty() {
        out.set_raw_comment(src.comment().to_vec().into_boxed_slice())?;
    }
    let new_names: std::collections::HashSet<String> = items.iter().map(|i| norm_name(&i.1)).collect();
    for i in 0..src.len() {
        p.check()?;
        let f = src.by_index_raw(i)?;
        if !new_names.contains(&norm_name(f.name()?.as_ref())) {
            out.raw_copy_file(f)?;
        }
    }
    let refs: Vec<&(PathBuf, String, bool)> = items.iter().collect();
    zip_write_items(&mut out, &refs, opts, done, total, p)?;
    out.finish()?.flush()?;
    Ok(())
}

fn add_rewrite(archive: &Path, tmp: &Path, items: &[(PathBuf, String, bool)], opts: &CreateOptions, p: &Progress) -> Result<()> {
    let work = tempdir_near(archive)?;
    let res = (|| -> Result<()> {
        extract(archive, &work, opts.password.as_deref(), None, p)?;
        for (path, name, is_dir) in items {
            p.check()?;
            let Some(target) = safe_join(&work, name) else { continue };
            if *is_dir {
                fs::create_dir_all(&target)?;
            } else {
                if let Some(par) = target.parent() {
                    fs::create_dir_all(par)?;
                }
                fs::copy(path, &target)?;
            }
        }
        let mut inputs: Vec<PathBuf> = fs::read_dir(&work)?.filter_map(|e| e.ok().map(|e| e.path())).collect();
        inputs.sort();
        create(tmp, &inputs, opts, p)
    })();
    let _ = fs::remove_dir_all(&work);
    res
}

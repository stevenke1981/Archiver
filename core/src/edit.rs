//! In-place editing of existing archives: comments and adding files.

use std::path::{Path, PathBuf};

use crate::archive_ops::{ZIPCRYPTO_UNSUPPORTED, has_zipcrypto, tempdir_near, unique_tmp};
use crate::create::{collect, zip_write_items};
use crate::rename::copy_tar_entry;
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
    // work on a copy so a crash mid-write cannot corrupt the original
    let tmp = unique_tmp(archive, "comment")?;
    let res = (|| -> Result<()> {
        fs::copy(archive, &tmp)?;
        let f = fs::OpenOptions::new().read(true).write(true).open(&tmp)?;
        let mut z = zip::ZipWriter::new_append(f)?;
        z.set_raw_comment(comment.as_bytes().to_vec().into_boxed_slice())?;
        let mut f = z.finish()?;
        // the new central directory may be shorter than the old one
        use io::Seek;
        let end = f.stream_position()?;
        f.set_len(end)?;
        f.flush()?;
        drop(f);
        fs::rename(&tmp, archive)?;
        Ok(())
    })();
    if res.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    res
}

fn is_tar(fmt: Format) -> bool {
    matches!(fmt, Format::Tar | Format::TarGz | Format::TarBz2 | Format::TarXz | Format::TarZst)
}

/// Add files/folders to an existing archive under the in-archive folder `prefix`
/// ("" = root, otherwise "dir/sub" without trailing slash).
///
/// * ZIP: existing entries are raw-copied (no recompression, existing encryption and the archive
///   comment are kept) into a temp file next to the archive, the new entries are written with
///   `opts.level` / `opts.password`, then the temp file atomically replaces the archive.
///   Archives with ZipCrypto (legacy) entries are appended to instead (a copy of the archive gets
///   the new entries and a new central directory), because raw copies would corrupt those entries;
///   replacing an existing file there is `Error::Unsupported`.
/// * tar.*: existing entries are streamed (headers, modes, symlinks preserved) into a new tar in a
///   temp file next to the archive, then the new entries are appended.
/// * 7z: extracted to a temp dir next to the archive and re-created.
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
    let tmp = unique_tmp(archive, "add")?;
    let opts = CreateOptions { format: fmt, level: opts.level, password: opts.password.clone() };
    let res = if fmt == Format::Zip {
        add_zip(archive, &tmp, &items, &opts, &done, total, p)
    } else if is_tar(fmt) {
        add_tar(archive, &tmp, &items, &opts, &done, total, p)
    } else {
        add_rewrite(archive, &tmp, &items, &opts, p)
    };
    match res {
        Ok(()) => {
            if let Err(e) = fs::rename(&tmp, archive) {
                let _ = fs::remove_file(&tmp);
                return Err(e.into());
            }
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
    if has_zipcrypto(&mut src)? {
        // raw copies would corrupt ZipCrypto entries: append to a copy, leaving them untouched
        let mut existing = std::collections::HashSet::new();
        for i in 0..src.len() {
            existing.insert(norm_name(src.by_index_raw(i)?.name()?.as_ref()));
        }
        if items.iter().any(|i| !i.2 && existing.contains(&norm_name(&i.1))) {
            return Err(Error::Unsupported(ZIPCRYPTO_UNSUPPORTED.into()));
        }
        drop(src);
        // folders that already exist are kept as they are
        let fresh: Vec<&(PathBuf, String, bool)> = items.iter().filter(|i| !existing.contains(&norm_name(&i.1))).collect();
        fs::copy(archive, tmp)?;
        let f = fs::OpenOptions::new().read(true).write(true).open(tmp)?;
        let mut out = zip::ZipWriter::new_append(f)?;
        zip_write_items(&mut out, &fresh, opts, done, total, p)?;
        let mut f = out.finish()?;
        use io::Seek;
        let end = f.stream_position()?;
        f.set_len(end)?;
        f.flush()?;
        return Ok(());
    }
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

fn add_tar(archive: &Path, tmp: &Path, items: &[(PathBuf, String, bool)], opts: &CreateOptions, done: &AtomicU64, total: u64, p: &Progress) -> Result<()> {
    let fmt = opts.format;
    let new_names: std::collections::HashSet<String> = items.iter().map(|i| norm_name(&i.1)).collect();
    let src = decoder(fmt, rd(File::open(archive)?))?;
    let mut ar = tar::Archive::new(src);
    let enc = encoder(fmt, wr(File::create(tmp)?), opts.level)?;
    let mut t = tar::Builder::new(enc);
    t.follow_symlinks(false);
    for entry in ar.entries()? {
        p.check()?;
        let mut e = entry?;
        let et = e.header().entry_type();
        if et.is_pax_global_extensions() {
            continue;
        }
        let path = e.path()?.into_owned();
        if new_names.contains(&norm_name(&path.to_string_lossy())) {
            continue;
        }
        let link = if et.is_symlink() || et.is_hard_link() { Some(e.link_name()?.map(|l| l.into_owned()).unwrap_or_default()) } else { None };
        copy_tar_entry(&mut t, &mut e, &path, link.as_deref())?;
    }
    for (path, name, is_dir) in items {
        p.check()?;
        if *is_dir {
            t.append_dir(name, path)?;
        } else {
            let file = File::open(path)?;
            let mut h = tar::Header::new_gnu();
            h.set_metadata(&file.metadata()?);
            let pr = ProgReader::new(rd(file), done, total, name, p);
            t.append_data(&mut h, name, pr).map_err(|e| {
                if p.cancel.load(Ordering::Relaxed) { Error::Cancelled } else { Error::Io(e) }
            })?;
        }
    }
    t.into_inner()?.finish_box()?;
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

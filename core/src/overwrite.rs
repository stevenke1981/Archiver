//! Extraction with an overwrite policy (WinRAR's "overwrite mode"): replace, skip or
//! auto-rename files that already exist in the destination.

use super::*;

/// What to do when an extracted file already exists in the destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum Overwrite {
    /// Overwrite existing files (the behaviour of [`extract`]).
    #[default]
    Replace,
    /// Keep existing files; the archive's copy is not written.
    Skip,
    /// Write the archive's copy as `name (2).ext`, `name (3).ext`, …
    Rename,
}

impl Overwrite {
    pub const ALL: [Overwrite; 3] = [Overwrite::Replace, Overwrite::Skip, Overwrite::Rename];

    /// Traditional Chinese label (menus).
    pub fn label(self) -> &'static str {
        match self {
            Overwrite::Replace => "覆寫現有檔案",
            Overwrite::Skip => "略過現有檔案",
            Overwrite::Rename => "自動重新命名",
        }
    }

    /// Stable key (settings file, `rarc x --overwrite`).
    pub fn key(self) -> &'static str {
        match self {
            Overwrite::Replace => "replace",
            Overwrite::Skip => "skip",
            Overwrite::Rename => "rename",
        }
    }

    pub fn from_key(s: &str) -> Option<Overwrite> {
        Self::ALL.into_iter().find(|o| o.key().eq_ignore_ascii_case(s.trim()))
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExtractOptions {
    pub overwrite: Overwrite,
}

/// What [`extract_with`] did besides writing files.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExtractStats {
    /// Files not written because they already existed ([`Overwrite::Skip`]).
    pub skipped: u64,
    /// Files written under a new name ([`Overwrite::Rename`]).
    pub renamed: u64,
}

/// [`extract`] with an overwrite policy. `Overwrite::Replace` behaves exactly like [`extract`].
///
/// `Skip` / `Rename` extract into a fresh hidden staging folder inside `dest` (same volume, any
/// format, no races with the parallel ZIP workers), then merge it into `dest`: folders are merged,
/// new files are moved in (mtimes kept), and a name that already exists (as a file or a folder) is
/// kept (`Skip`) or the archive's copy gets the first free `name (2).ext` (`Rename`). The staging
/// folder is always removed, also on errors and cancellation; a failed extraction leaves `dest`
/// untouched.
pub fn extract_with(archive: &Path, dest: &Path, password: Option<&str>, selected: Option<&[String]>, opts: &ExtractOptions, p: &Progress) -> Result<ExtractStats> {
    if opts.overwrite == Overwrite::Replace {
        return extract(archive, dest, password, selected, p).map(|()| ExtractStats::default());
    }
    detect_or_err(archive)?;
    fs::create_dir_all(dest)?;
    let staging = staging_dir(dest)?;
    let res = extract(archive, &staging, password, selected, p).and_then(|()| {
        let mut st = ExtractStats::default();
        merge_dir(&staging, dest, opts.overwrite == Overwrite::Rename, &mut st)?;
        Ok(st)
    });
    let _ = fs::remove_dir_all(&staging);
    res
}

/// [`extract_each`] with an overwrite policy (per-item results, one failure does not stop the batch).
pub fn extract_each_with(archives: &[PathBuf], dest: &BatchDest, password: Option<&str>, opts: &ExtractOptions, p: &Progress) -> Vec<BatchResult> {
    batch::extract_each_opts(archives, dest, password, opts, p)
}

const STAGING: &str = ".archiver-extract-";

fn staging_dir(dest: &Path) -> Result<PathBuf> {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    for i in 0..1000u32 {
        let d = dest.join(format!("{STAGING}{}-{nanos}-{i}", std::process::id()));
        match fs::create_dir(&d) {
            Ok(()) => return Ok(d),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(Error::Io(io::Error::other("cannot create staging dir")))
}

/// Moves the contents of `src` into `dst` (an existing folder).
fn merge_dir(src: &Path, dst: &Path, rename: bool, st: &mut ExtractStats) -> Result<()> {
    let mut entries = fs::read_dir(src)?.collect::<io::Result<Vec<_>>>()?;
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let (from, to) = (e.path(), dst.join(e.file_name()));
        let ft = e.file_type()?;
        if ft.is_dir() {
            if to.is_dir() {
                merge_dir(&from, &to, rename, st)?;
                continue;
            }
            if put_dir(&from, &to, rename, st)? { continue; }
        } else if put_file(&from, &to, ft.is_symlink())? {
            continue;
        }
        // conflict: `to` exists (file vs file, or file vs folder)
        let n = if ft.is_dir() { count_files(&from) } else { 1 };
        if !rename {
            st.skipped += n;
            continue;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        let (stem, ext) = if ft.is_dir() { (name.clone(), String::new()) } else { split_name(&name) };
        let mut placed = false;
        for k in 2..100_000u32 {
            let cand = dst.join(format!("{stem} ({k}){ext}"));
            placed = if ft.is_dir() { put_dir(&from, &cand, rename, st)? } else { put_file(&from, &cand, ft.is_symlink())? };
            if placed { break; }
        }
        if !placed {
            return Err(Error::Io(io::Error::other(format!("no free name for {name}"))));
        }
        st.renamed += n;
    }
    Ok(())
}

/// Puts folder `from` at `to` if nothing exists there. `false`: `to` is taken.
fn put_dir(from: &Path, to: &Path, rename: bool, st: &mut ExtractStats) -> Result<bool> {
    if fs::symlink_metadata(to).is_ok() { return Ok(false); }
    // fast path: move the whole subtree
    if fs::rename(from, to).is_ok() { return Ok(true); }
    match fs::create_dir(to) {
        Ok(()) => merge_dir(from, to, rename, st).map(|()| true),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// Puts file `from` at `to` without ever replacing an existing `to`. `false`: `to` is taken.
fn put_file(from: &Path, to: &Path, symlink: bool) -> Result<bool> {
    if !symlink {
        // a hard link is an atomic "create if absent" that keeps the file (and its mtime)
        match fs::hard_link(from, to) {
            Ok(()) => {
                let _ = fs::remove_file(from); // may fail for read-only files; the staging dir goes anyway
                return Ok(true);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Ok(false),
            Err(_) => {} // no hard links here (FAT, some network shares, other volume): fall back
        }
    }
    if fs::symlink_metadata(to).is_ok() { return Ok(false); }
    match fs::rename(from, to) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::CrossesDevices => copy_new(from, to),
        Err(e) => Err(e.into()),
    }
}

/// Copy + remove (`to` on another volume, e.g. below a junction or mount point).
fn copy_new(from: &Path, to: &Path) -> Result<bool> {
    let mut w = match fs::OpenOptions::new().write(true).create_new(true).open(to) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Ok(false),
        Err(e) => return Err(e.into()),
    };
    let res = (|| -> Result<()> {
        io::copy(&mut File::open(from)?, &mut w)?;
        let meta = fs::metadata(from)?;
        if let Ok(t) = meta.modified() { let _ = w.set_modified(t); }
        let _ = w.set_permissions(meta.permissions());
        Ok(())
    })();
    if let Err(e) = res {
        drop(w);
        let _ = fs::remove_file(to);
        return Err(e);
    }
    let _ = fs::remove_file(from);
    Ok(true)
}

/// Files below `dir` (at least 1, so a skipped empty folder still shows up in the stats).
fn count_files(dir: &Path) -> u64 {
    (WalkDir::new(dir).into_iter().filter_map(|e| e.ok()).filter(|e| !e.file_type().is_dir()).count() as u64).max(1)
}

/// `a.txt` -> (`a`, `.txt`); `a.tar.gz` -> (`a`, `.tar.gz`); `.bashrc`, `README` -> (name, empty).
fn split_name(name: &str) -> (String, String) {
    let stem = archive_stem(Path::new(name));
    if !stem.is_empty() && stem.len() < name.len() && name.is_char_boundary(stem.len()) {
        return (stem.clone(), name[stem.len()..].to_string());
    }
    match name.rfind('.') {
        Some(i) if i > 0 => (name[..i].to_string(), name[i..].to_string()),
        _ => (name.to_string(), String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arc(t: &Path, fmt: Format, password: Option<&str>) -> PathBuf {
        let s = t.join("src/in");
        fs::create_dir_all(s.join("sub")).unwrap();
        fs::write(s.join("a.txt"), "new a").unwrap();
        fs::write(s.join("sub/b.txt"), "new b").unwrap();
        fs::write(s.join("c.tar.gz"), "new c").unwrap();
        let out = t.join(format!("arc.{}", fmt.extension()));
        create(&out, &[s], &CreateOptions { format: fmt, level: None, password: password.map(Into::into) }, &Progress::none()).unwrap();
        out
    }

    /// dest/in/a.txt and dest/in/c.tar.gz exist, dest/in/sub/ holds an unrelated file.
    fn dest(t: &Path) -> PathBuf {
        let d = t.join("dest");
        fs::create_dir_all(d.join("in/sub")).unwrap();
        fs::write(d.join("in/a.txt"), "old a").unwrap();
        fs::write(d.join("in/c.tar.gz"), "old c").unwrap();
        fs::write(d.join("in/sub/old.txt"), "old").unwrap();
        d
    }

    fn read(p: PathBuf) -> String {
        fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
    }

    fn no_staging(d: &Path) {
        for e in WalkDir::new(d).into_iter().filter_map(|e| e.ok()) {
            assert!(!e.file_name().to_string_lossy().starts_with(STAGING), "staging left: {}", e.path().display());
        }
    }

    fn with(o: Overwrite) -> ExtractOptions {
        ExtractOptions { overwrite: o }
    }

    #[test]
    fn replace_overwrites() {
        let t = tempfile::tempdir().unwrap();
        let (a, d) = (arc(t.path(), Format::Zip, None), dest(t.path()));
        let st = extract_with(&a, &d, None, None, &with(Overwrite::Replace), &Progress::none()).unwrap();
        assert_eq!(st, ExtractStats::default());
        assert_eq!(read(d.join("in/a.txt")), "new a");
        assert_eq!(read(d.join("in/c.tar.gz")), "new c");
        assert_eq!(read(d.join("in/sub/b.txt")), "new b");
        assert_eq!(read(d.join("in/sub/old.txt")), "old");
    }

    #[test]
    fn skip_keeps_existing_and_merges_folders() {
        for fmt in [Format::Zip, Format::SevenZ, Format::TarGz] {
            let t = tempfile::tempdir().unwrap();
            let (a, d) = (arc(t.path(), fmt, None), dest(t.path()));
            let st = extract_with(&a, &d, None, None, &with(Overwrite::Skip), &Progress::none()).unwrap();
            assert_eq!(st, ExtractStats { skipped: 2, renamed: 0 }, "{fmt:?}");
            assert_eq!(read(d.join("in/a.txt")), "old a");
            assert_eq!(read(d.join("in/c.tar.gz")), "old c");
            assert_eq!(read(d.join("in/sub/b.txt")), "new b");
            assert_eq!(read(d.join("in/sub/old.txt")), "old");
            no_staging(&d);
            // a fresh destination is filled completely (whole-subtree move)
            let fresh = t.path().join("fresh");
            let st = extract_with(&a, &fresh, None, None, &with(Overwrite::Skip), &Progress::none()).unwrap();
            assert_eq!(st, ExtractStats::default());
            assert_eq!(read(fresh.join("in/sub/b.txt")), "new b");
            no_staging(&fresh);
        }
    }

    #[test]
    fn rename_counts_up() {
        for fmt in [Format::Zip, Format::SevenZ, Format::TarGz] {
            let t = tempfile::tempdir().unwrap();
            let (a, d) = (arc(t.path(), fmt, None), dest(t.path()));
            let st = extract_with(&a, &d, None, None, &with(Overwrite::Rename), &Progress::none()).unwrap();
            assert_eq!(st, ExtractStats { skipped: 0, renamed: 2 }, "{fmt:?}");
            assert_eq!(read(d.join("in/a.txt")), "old a");
            assert_eq!(read(d.join("in/a (2).txt")), "new a");
            assert_eq!(read(d.join("in/c (2).tar.gz")), "new c");
            assert_eq!(read(d.join("in/sub/b.txt")), "new b");
            let st = extract_with(&a, &d, None, None, &with(Overwrite::Rename), &Progress::none()).unwrap();
            assert_eq!(st, ExtractStats { skipped: 0, renamed: 3 }, "{fmt:?}");
            assert_eq!(read(d.join("in/a (3).txt")), "new a");
            assert_eq!(read(d.join("in/c (3).tar.gz")), "new c");
            assert_eq!(read(d.join("in/sub/b (2).txt")), "new b");
            no_staging(&d);
            // mtimes are those a plain extraction writes
            let plain = t.path().join("plain");
            extract(&a, &plain, None, None, &Progress::none()).unwrap();
            let mt = |p: PathBuf| fs::metadata(p).unwrap().modified().unwrap();
            assert_eq!(mt(d.join("in/a (3).txt")), mt(plain.join("in/a.txt")), "{fmt:?}");
        }
    }

    #[test]
    fn file_folder_conflicts() {
        let t = tempfile::tempdir().unwrap();
        let a = arc(t.path(), Format::Zip, None);
        let d = t.path().join("dest");
        fs::create_dir_all(d.join("in/a.txt")).unwrap(); // folder where the archive has a file
        fs::write(d.join("in/sub"), "file").unwrap(); // file where the archive has a folder
        let st = extract_with(&a, &d, None, None, &with(Overwrite::Skip), &Progress::none()).unwrap();
        assert_eq!(st, ExtractStats { skipped: 2, renamed: 0 });
        assert!(d.join("in/a.txt").is_dir());
        assert_eq!(read(d.join("in/sub")), "file");
        assert_eq!(read(d.join("in/c.tar.gz")), "new c");
        let st = extract_with(&a, &d, None, None, &with(Overwrite::Rename), &Progress::none()).unwrap();
        assert_eq!(st, ExtractStats { skipped: 0, renamed: 3 });
        assert_eq!(read(d.join("in/a (2).txt")), "new a");
        assert_eq!(read(d.join("in/sub (2)/b.txt")), "new b");
        assert_eq!(read(d.join("in/c (2).tar.gz")), "new c");
        no_staging(&d);
    }

    #[test]
    fn selected_subset() {
        let t = tempfile::tempdir().unwrap();
        let (a, d) = (arc(t.path(), Format::Zip, None), dest(t.path()));
        let sel = vec!["in/sub".to_string(), "in/a.txt".to_string()];
        let st = extract_with(&a, &d, None, Some(&sel), &with(Overwrite::Rename), &Progress::none()).unwrap();
        assert_eq!(st, ExtractStats { skipped: 0, renamed: 1 });
        assert_eq!(read(d.join("in/a (2).txt")), "new a");
        assert_eq!(read(d.join("in/sub/b.txt")), "new b");
        assert!(!d.join("in/c (2).tar.gz").exists());
        no_staging(&d);
    }

    #[test]
    fn single_file_format() {
        let t = tempfile::tempdir().unwrap();
        let f = t.path().join("a.txt");
        fs::write(&f, "new a").unwrap();
        let gz = t.path().join("a.txt.gz");
        create(&gz, &[f], &CreateOptions { format: Format::Gz, level: None, password: None }, &Progress::none()).unwrap();
        let d = t.path().join("dest");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("a.txt"), "old a").unwrap();
        let st = extract_with(&gz, &d, None, None, &with(Overwrite::Skip), &Progress::none()).unwrap();
        assert_eq!(st.skipped, 1);
        assert_eq!(read(d.join("a.txt")), "old a");
        let st = extract_with(&gz, &d, None, None, &with(Overwrite::Rename), &Progress::none()).unwrap();
        assert_eq!(st.renamed, 1);
        assert_eq!(read(d.join("a (2).txt")), "new a");
        no_staging(&d);
    }

    #[test]
    fn staging_removed_on_error_and_cancel() {
        let t = tempfile::tempdir().unwrap();
        let a = arc(t.path(), Format::Zip, Some("secret"));
        let d = dest(t.path());
        for pw in [Some("wrong"), None] {
            for o in [Overwrite::Skip, Overwrite::Rename] {
                assert!(extract_with(&a, &d, pw, None, &with(o), &Progress::none()).is_err());
                no_staging(&d);
                assert!(!d.join("in/a (2).txt").exists());
            }
        }
        let st = extract_with(&a, &d, Some("secret"), None, &with(Overwrite::Rename), &Progress::none()).unwrap();
        assert_eq!(st.renamed, 2);
        assert_eq!(read(d.join("in/a (2).txt")), "new a");

        let on = |_: u64, _: u64, _: &str| {};
        let p = Progress { on: &on, cancel: Arc::new(AtomicBool::new(true)) };
        let r = extract_with(&a, &d, Some("secret"), None, &with(Overwrite::Skip), &p);
        assert!(matches!(r, Err(Error::Cancelled)), "{r:?}");
        no_staging(&d);
    }

    #[test]
    fn split_names() {
        let s = |n: &str| split_name(n);
        assert_eq!(s("a.txt"), ("a".into(), ".txt".into()));
        assert_eq!(s("a.tar.gz"), ("a".into(), ".tar.gz".into()));
        assert_eq!(s("report.final.txt"), ("report.final".into(), ".txt".into()));
        assert_eq!(s(".bashrc"), (".bashrc".into(), "".into()));
        assert_eq!(s("README"), ("README".into(), "".into()));
        assert_eq!(s("中文.TXT"), ("中文".into(), ".TXT".into()));
    }

    #[test]
    fn extract_each_with_policy() {
        let t = tempfile::tempdir().unwrap();
        let a = arc(t.path(), Format::Zip, None);
        let b = t.path().join("arc.7z");
        create(&b, &[t.path().join("src/in")], &CreateOptions { format: Format::SevenZ, level: None, password: None }, &Progress::none()).unwrap();
        let arcs = vec![a, b];
        let rs = extract_each_with(&arcs, &BatchDest::BesideInFolder, None, &with(Overwrite::Skip), &Progress::none());
        assert!(rs.iter().all(|r| r.result.is_ok()), "{rs:?}");
        assert_eq!(rs[0].output, t.path().join("arc"));
        assert_eq!(rs[1].output, t.path().join("arc (2)"));
        fs::write(t.path().join("arc/in/a.txt"), "edited").unwrap();
        // second run into the same folders: Skip keeps the edit, Rename adds copies
        let rs = extract_each_with(&arcs, &BatchDest::BesideInFolder, None, &with(Overwrite::Skip), &Progress::none());
        assert!(rs.iter().all(|r| r.result.is_ok()), "{rs:?}");
        assert_eq!(read(t.path().join("arc/in/a.txt")), "edited");
        let rs = extract_each_with(&arcs, &BatchDest::BesideInFolder, None, &with(Overwrite::Rename), &Progress::none());
        assert!(rs.iter().all(|r| r.result.is_ok()), "{rs:?}");
        assert_eq!(read(t.path().join("arc/in/a (2).txt")), "new a");
        assert_eq!(read(t.path().join("arc (2)/in/sub/b (2).txt")), "new b");
        let rs = extract_each_with(&arcs, &BatchDest::BesideInFolder, None, &with(Overwrite::Replace), &Progress::none());
        assert!(rs.iter().all(|r| r.result.is_ok()), "{rs:?}");
        assert_eq!(read(t.path().join("arc/in/a.txt")), "new a");
        no_staging(t.path());
    }
}

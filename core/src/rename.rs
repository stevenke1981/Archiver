//! Rename an entry (file or folder) inside an archive (WinRAR F2).

use crate::archive_ops::{reject_zipcrypto, tempdir_near, unique_tmp};

use super::*;

fn invalid(msg: &str) -> Error {
    Error::Io(io::Error::new(io::ErrorKind::InvalidInput, msg.to_string()))
}

/// Rename the entry `from` to `to` inside `archive`. Both are full entry names with `/`
/// separators and no trailing slash (`\`, a leading `./` or `/` and a trailing `/` are normalised
/// away). When `from` is a folder, every entry below `from/` moves to `to/`.
///
/// Errors: `Error::Unsupported` for RAR and single-file formats; an `Error::Io(InvalidInput)` for an
/// empty `to`, `to == from`, or `to` inside `from` (or an ancestor of `from`);
/// [`Error::NotFound`] when nothing matches `from`; `Error::Io(AlreadyExists)` when `to` (or
/// anything below `to/`) already exists.
/// `password`: needed only for formats that must be recompressed (7z); ZIP and tar.* entries
/// are copied without decompressing.
pub fn rename(archive: &Path, from: &str, to: &str, password: Option<&str>, p: &Progress) -> Result<()> {
    let fmt = detect_or_err(archive)?;
    if fmt == Format::Rar || fmt.is_single_file() {
        return Err(Error::Unsupported(format!("cannot rename entries in {}", fmt.label())));
    }
    let from = norm_name(from);
    let to = norm_name(to);
    if from.is_empty() {
        return Err(Error::NotFound(from));
    }
    if to.is_empty() {
        return Err(invalid("new name is empty"));
    }
    if to == from {
        return Err(invalid("new name equals the old name"));
    }
    if to.starts_with(&format!("{from}/")) {
        return Err(invalid("cannot move a folder into itself"));
    }
    if from.starts_with(&format!("{to}/")) {
        return Err(invalid("new name is a parent folder of the old name"));
    }
    if to.split('/').any(|c| c == ".." || c.is_empty()) {
        return Err(invalid("invalid new name"));
    }
    p.check()?;
    let sc = Scan { from: &from, to: &to, found: false, exists: false };
    if fmt == Format::Zip {
        // before anything is written: raw copies would corrupt ZipCrypto entries
        reject_zipcrypto(&mut zip::ZipArchive::new(BufReader::new(File::open(archive)?))?)?;
    }
    let tmp = unique_tmp(archive, "rename")?;
    let res = match fmt {
        Format::Zip => rename_zip(archive, &tmp, sc, p),
        f if matches!(f, Format::Tar | Format::TarGz | Format::TarBz2 | Format::TarXz | Format::TarZst) => rename_tar(archive, &tmp, f, sc, p),
        _ => rename_7z(archive, &tmp, sc, password, p),
    };
    match res {
        Ok(()) => fs::rename(&tmp, archive).map_err(|e| {
            let _ = fs::remove_file(&tmp);
            e.into()
        }),
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Tracks whether `from` exists and `to` is already taken while entries stream past.
struct Scan<'a> {
    from: &'a str,
    to: &'a str,
    found: bool,
    exists: bool,
}

impl Scan<'_> {
    /// New name for `name` (None when it is not renamed); records what was seen.
    fn see(&mut self, name: &str) -> Option<String> {
        let dir_slash = name.ends_with('/');
        let n = norm_name(name);
        let mapped = if n == self.from {
            Some(self.to.to_string())
        } else {
            n.strip_prefix(self.from).and_then(|r| r.strip_prefix('/')).map(|r| format!("{}/{r}", self.to))
        };
        match mapped {
            Some(m) => {
                self.found = true;
                Some(if dir_slash { format!("{m}/") } else { m })
            }
            None => {
                if n == self.to || n.starts_with(&format!("{}/", self.to)) {
                    self.exists = true;
                }
                None
            }
        }
    }
    fn verdict(&self) -> Result<()> {
        if !self.found {
            return Err(Error::NotFound(self.from.to_string()));
        }
        if self.exists {
            return Err(Error::Io(io::Error::new(io::ErrorKind::AlreadyExists, format!("{} already exists", self.to))));
        }
        Ok(())
    }
}

fn rename_zip(archive: &Path, tmp: &Path, mut sc: Scan, p: &Progress) -> Result<()> {
    let mut src = zip::ZipArchive::new(BufReader::with_capacity(256 << 10, File::open(archive)?))?;
    for i in 0..src.len() {
        let f = src.by_index_raw(i)?;
        sc.see(f.name()?.as_ref());
    }
    sc.verdict()?;
    let mut out = zip::ZipWriter::new(wr(File::create(tmp)?));
    if !src.comment().is_empty() {
        out.set_raw_comment(src.comment().to_vec().into_boxed_slice())?;
    }
    for i in 0..src.len() {
        p.check()?;
        let f = src.by_index_raw(i)?;
        let name = f.name()?.to_string();
        match sc.see(&name) {
            Some(new) => out.raw_copy_file_rename(f, new)?,
            None => out.raw_copy_file(f)?,
        }
    }
    out.finish()?.flush()?;
    Ok(())
}

fn rename_tar(archive: &Path, tmp: &Path, fmt: Format, mut sc: Scan, p: &Progress) -> Result<()> {
    let mut ar = tar::Archive::new(decoder(fmt, rd(File::open(archive)?))?);
    let enc = encoder(fmt, wr(File::create(tmp)?), None)?;
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
        let old = path.to_string_lossy().to_string();
        let new = sc.see(&old).map(PathBuf::from).unwrap_or(path);
        let mut link = None;
        if et.is_symlink() || et.is_hard_link() {
            let mut l = e.link_name()?.map(|l| l.into_owned()).unwrap_or_default();
            if et.is_hard_link() {
                // hard link targets are archive paths; keep them pointing at the moved entry
                let mut s2 = Scan { from: sc.from, to: sc.to, found: false, exists: false };
                if let Some(m) = s2.see(&l.to_string_lossy()) {
                    l = PathBuf::from(m);
                }
            }
            link = Some(l);
        }
        copy_tar_entry(&mut t, &mut e, &new, link.as_deref())?;
    }
    sc.verdict()?;
    t.into_inner()?.finish_box()?;
    Ok(())
}

/// Copy one tar entry as `path` (a link to `link` for symlinks and hard links) with a fresh GNU
/// header: entry type, mode, uid, gid, mtime, user/group names and device numbers are carried
/// over, the size is the entry's true size (pax `size` included; GNU base-256 holds big values),
/// and long paths or link targets get GNU long-name records. Rebuilding the header (rather than
/// cloning it) drops a stale ustar `prefix`. The entry's pax extensions (sub-second times, xattrs,
/// big uid/gid, ...) are re-emitted, except `path`, `linkpath` and `size`, which are re-derived.
/// GNU sparse entries are written expanded, as regular files.
pub(crate) fn copy_tar_entry<W: Write, R: Read>(t: &mut tar::Builder<W>, e: &mut tar::Entry<'_, R>, path: &Path, link: Option<&Path>) -> io::Result<()> {
    let pax: Vec<(String, Vec<u8>)> = match e.pax_extensions()? {
        Some(exts) => exts
            .filter_map(|x| x.ok())
            .filter_map(|x| {
                let k = x.key().ok()?;
                (!matches!(k, "path" | "linkpath" | "size")).then(|| (k.to_string(), x.value_bytes().to_vec()))
            })
            .collect(),
        None => Vec::new(),
    };
    let mut h = tar::Header::new_gnu();
    {
        let old = e.header();
        let et = old.entry_type();
        h.set_entry_type(if et.is_gnu_sparse() { tar::EntryType::Regular } else { et });
        h.set_mode(old.mode().unwrap_or(if et.is_dir() { 0o755 } else { 0o644 }));
        h.set_uid(old.uid().unwrap_or(0));
        h.set_gid(old.gid().unwrap_or(0));
        h.set_mtime(old.mtime().unwrap_or(0));
        if let Ok(Some(u)) = old.username() {
            let _ = h.set_username(u);
        }
        if let Ok(Some(g)) = old.groupname() {
            let _ = h.set_groupname(g);
        }
        if et.is_character_special() || et.is_block_special() {
            if let Ok(Some(m)) = old.device_major() {
                let _ = h.set_device_major(m);
            }
            if let Ok(Some(m)) = old.device_minor() {
                let _ = h.set_device_minor(m);
            }
        }
    }
    h.set_size(if link.is_some() { 0 } else { e.size() });
    if !pax.is_empty() {
        t.append_pax_extensions(pax.iter().map(|(k, v)| (k.as_str(), v.as_slice())))?;
    }
    match link {
        Some(l) => t.append_link(&mut h, path, l),
        None => t.append_data(&mut h, path, e),
    }
}

/// Whether `a` and `b` name the same existing file (e.g. paths differing only in case on a
/// case-insensitive file system).
fn same_file(a: &Path, b: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        matches!((fs::symlink_metadata(a), fs::symlink_metadata(b)), (Ok(x), Ok(y)) if x.dev() == y.dev() && x.ino() == y.ino())
    }
    #[cfg(not(unix))]
    {
        matches!((fs::canonicalize(a), fs::canonicalize(b)), (Ok(x), Ok(y)) if x == y)
    }
}

fn rename_7z(archive: &Path, tmp: &Path, mut sc: Scan, password: Option<&str>, p: &Progress) -> Result<()> {
    // decide from the listing, with the same exact-name rules as ZIP and tar
    for e in list(archive, password)? {
        sc.see(&e.name);
    }
    sc.verdict()?;
    let (from, to) = (sc.from, sc.to);
    let work = tempdir_near(archive)?;
    let res = (|| -> Result<()> {
        extract(archive, &work, password, None, p)?;
        let src = safe_join(&work, from).filter(|s| s.exists()).ok_or_else(|| Error::NotFound(from.to_string()))?;
        let dst = safe_join(&work, to).ok_or_else(|| invalid("invalid new name"))?;
        if dst.exists() {
            if !same_file(&src, &dst) {
                return Err(Error::Io(io::Error::new(io::ErrorKind::AlreadyExists, format!("{to} already exists"))));
            }
            // case-only rename on a case-insensitive file system: move through a temp name
            let mid = (0..1000u32).map(|i| work.join(format!(".archiver-rename-{i}"))).find(|m| !m.exists()).ok_or_else(|| invalid("cannot pick a temp name"))?;
            fs::rename(&src, &mid)?;
            fs::rename(&mid, &dst)?;
        } else {
            if let Some(par) = dst.parent() {
                fs::create_dir_all(par)?;
            }
            fs::rename(&src, &dst)?;
        }
        p.check()?;
        let mut inputs: Vec<PathBuf> = fs::read_dir(&work)?.filter_map(|e| e.ok().map(|e| e.path())).collect();
        inputs.sort();
        let opts = CreateOptions { format: Format::SevenZ, level: None, password: password.map(String::from) };
        create(tmp, &inputs, &opts, p)
    })();
    let _ = fs::remove_dir_all(&work);
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zip_with(dir: &Path, name: &str, pw: Option<&str>, comment: Option<&str>) -> PathBuf {
        let src = dir.join("src");
        fs::create_dir_all(src.join("top/sub")).unwrap();
        fs::write(src.join("top/a.txt"), "alpha").unwrap();
        fs::write(src.join("top/sub/b.txt"), "beta").unwrap();
        fs::write(src.join("c.txt"), "gamma").unwrap();
        let out = dir.join(name);
        let inputs = vec![src.join("top"), src.join("c.txt")];
        let fmt = Format::detect(&out).unwrap();
        create(&out, &inputs, &CreateOptions { format: fmt, level: None, password: pw.map(String::from) }, &Progress::none()).unwrap();
        if let Some(c) = comment {
            set_comment(&out, c, &Progress::none()).unwrap();
        }
        out
    }

    fn names(a: &Path, pw: Option<&str>) -> Vec<String> {
        let mut v: Vec<String> = list(a, pw).unwrap().into_iter().map(|e| norm_name(&e.name)).collect();
        v.sort();
        v
    }

    fn content(a: &Path, name: &str, pw: Option<&str>) -> String {
        String::from_utf8(read_entry(a, name, pw, 1 << 20, &Progress::none()).unwrap().bytes).unwrap()
    }

    fn ren(a: &Path, f: &str, t: &str, pw: Option<&str>) -> Result<()> {
        rename(a, f, t, pw, &Progress::none())
    }

    fn leftovers(dir: &Path) -> Vec<String> {
        fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_string()).filter(|n| n.starts_with(".archiver") || n.ends_with(".tmp")).collect()
    }

    /// (path, pax extensions) of every entry in a plain tar.
    fn tar_entries(t: &Path) -> Vec<(String, Vec<(String, String)>)> {
        let mut ar = tar::Archive::new(File::open(t).unwrap());
        let mut v = Vec::new();
        for e in ar.entries().unwrap() {
            let mut e = e.unwrap();
            let pax = e.pax_extensions().unwrap().map(|x| x.map(|x| x.unwrap()).map(|x| (x.key().unwrap().to_string(), x.value().unwrap().to_string())).collect()).unwrap_or_default();
            v.push((e.path().unwrap().to_string_lossy().replace('\\', "/"), pax));
        }
        v
    }

    #[test]
    fn zip_rename_file_and_folder() {
        let d = tempfile::tempdir().unwrap();
        let z = zip_with(d.path(), "t.zip", None, None);
        ren(&z, "c.txt", "renamed.txt", None).unwrap();
        assert!(names(&z, None).contains(&"renamed.txt".to_string()));
        assert!(!names(&z, None).contains(&"c.txt".to_string()));
        assert_eq!(content(&z, "renamed.txt", None), "gamma");
        ren(&z, "top", "moved/deep", None).unwrap();
        let n = names(&z, None);
        assert!(n.contains(&"moved/deep".to_string()), "{n:?}");
        assert!(n.contains(&"moved/deep/a.txt".to_string()));
        assert!(n.contains(&"moved/deep/sub/b.txt".to_string()));
        assert!(!n.iter().any(|x| x.starts_with("top")));
        assert_eq!(content(&z, "moved/deep/sub/b.txt", None), "beta");
        // directory entries keep their trailing slash
        let raw = zip::ZipArchive::new(File::open(&z).unwrap()).unwrap();
        assert!(raw.file_names().any(|n| n.as_deref().is_ok_and(|n| n == "moved/deep/sub/")));
        assert!(leftovers(d.path()).is_empty(), "{:?}", leftovers(d.path()));
    }

    #[test]
    fn zip_rename_keeps_comment() {
        let d = tempfile::tempdir().unwrap();
        let z = zip_with(d.path(), "t.zip", None, Some("hello comment"));
        ren(&z, "c.txt", "d.txt", None).unwrap();
        assert_eq!(read_comment(&z).unwrap().as_deref(), Some("hello comment"));
    }

    #[test]
    fn zip_rename_encrypted_without_password() {
        let d = tempfile::tempdir().unwrap();
        let z = zip_with(d.path(), "t.zip", Some("pw1"), None);
        ren(&z, "top/a.txt", "top/z.txt", None).unwrap();
        assert_eq!(content(&z, "top/z.txt", Some("pw1")), "alpha");
        assert!(read_entry(&z, "top/z.txt", None, 100, &Progress::none()).is_err());
    }

    #[test]
    fn tar_gz_rename_content_and_mode() {
        let d = tempfile::tempdir().unwrap();
        let t = zip_with(d.path(), "t.tar.gz", None, None);
        ren(&t, "top", "other", None).unwrap();
        let n = names(&t, None);
        assert!(n.contains(&"other/sub/b.txt".to_string()), "{n:?}");
        assert!(!n.iter().any(|x| x.starts_with("top")));
        assert_eq!(content(&t, "other/a.txt", None), "alpha");
        assert_eq!(content(&t, "c.txt", None), "gamma");
        assert!(leftovers(d.path()).is_empty(), "{:?}", leftovers(d.path()));
    }

    #[cfg(unix)]
    #[test]
    fn tar_gz_rename_keeps_mode() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("run.sh");
        fs::write(&src, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&src, fs::Permissions::from_mode(0o755)).unwrap();
        let t = d.path().join("m.tar.gz");
        create(&t, &[src], &CreateOptions { format: Format::TarGz, level: None, password: None }, &Progress::none()).unwrap();
        ren(&t, "run.sh", "go.sh", None).unwrap();
        let out = d.path().join("out");
        extract(&t, &out, None, None, &Progress::none()).unwrap();
        assert_eq!(fs::metadata(out.join("go.sh")).unwrap().permissions().mode() & 0o777, 0o755);
    }

    #[test]
    fn tar_long_name() {
        let d = tempfile::tempdir().unwrap();
        let t = zip_with(d.path(), "t.tar", None, None);
        let long = format!("{}/file.txt", "d".repeat(150));
        ren(&t, "c.txt", &long, None).unwrap();
        assert_eq!(content(&t, &long, None), "gamma");
    }

    #[test]
    fn tar_ustar_prefix_is_not_kept() {
        // a ustar entry whose path lives in name + prefix; the new short name must not inherit the prefix
        let d = tempfile::tempdir().unwrap();
        let t = d.path().join("u.tar");
        let long = format!("{}/sub/file.txt", "L".repeat(90));
        {
            let mut b = tar::Builder::new(File::create(&t).unwrap());
            let mut h = tar::Header::new_ustar();
            h.set_size(5);
            h.set_mode(0o644);
            b.append_data(&mut h, &long, &b"hello"[..]).unwrap();
            assert!(h.as_ustar().unwrap().prefix.iter().any(|&c| c != 0), "fixture must use the ustar prefix");
            let mut h = tar::Header::new_ustar();
            h.set_size(3);
            b.append_data(&mut h, "short.txt", &b"big"[..]).unwrap();
            b.finish().unwrap();
        }
        ren(&t, &long, "new.txt", None).unwrap();
        let n: Vec<String> = tar_entries(&t).into_iter().map(|e| e.0).collect();
        assert_eq!(n, ["new.txt", "short.txt"]);
        assert_eq!(content(&t, "new.txt", None), "hello");
        // and back to a long name (GNU long-name record)
        ren(&t, "new.txt", &long, None).unwrap();
        assert_eq!(content(&t, &long, None), "hello");
        ren(&t, &format!("{}/sub", "L".repeat(90)), "s", None).unwrap();
        assert_eq!(content(&t, "s/file.txt", None), "hello");
        assert!(leftovers(d.path()).is_empty());
    }

    #[test]
    fn tar_keeps_pax_extensions() {
        let d = tempfile::tempdir().unwrap();
        let t = d.path().join("p.tar");
        let pax_long = format!("{}/deep.txt", "P".repeat(120));
        {
            let mut b = tar::Builder::new(File::create(&t).unwrap());
            b.append_pax_extensions([("mtime", &b"1700000000.5"[..]), ("SCHILY.xattr.user.k", &b"v"[..])]).unwrap();
            let mut h = tar::Header::new_ustar();
            h.set_size(4);
            h.set_mtime(1_700_000_000);
            b.append_data(&mut h, "p.txt", &b"data"[..]).unwrap();
            // an entry whose name exists only in a pax `path` record
            b.append_pax_extensions([("path", pax_long.as_bytes())]).unwrap();
            let mut h = tar::Header::new_ustar();
            h.set_size(2);
            h.set_path("short-stub").unwrap();
            h.set_cksum();
            b.append(&h, &b"pp"[..]).unwrap();
            b.finish().unwrap();
        }
        ren(&t, "p.txt", "q.txt", None).unwrap();
        let es = tar_entries(&t);
        assert_eq!(es[0].0, "q.txt");
        assert!(es[0].1.contains(&("mtime".into(), "1700000000.5".into())), "{:?}", es[0].1);
        assert!(es[0].1.contains(&("SCHILY.xattr.user.k".into(), "v".into())), "{:?}", es[0].1);
        assert!(!es[0].1.iter().any(|(k, _)| k == "path" || k == "size"));
        assert_eq!(es[1].0, pax_long);
        assert!(!es[1].1.iter().any(|(k, _)| k == "path"), "{:?}", es[1].1);
        assert_eq!(content(&t, "q.txt", None), "data");
        assert_eq!(content(&t, &pax_long, None), "pp");
    }

    #[test]
    fn case_only_rename() {
        for ext in ["zip", "tar", "7z"] {
            let d = tempfile::tempdir().unwrap();
            let z = zip_with(d.path(), &format!("t.{ext}"), None, None);
            ren(&z, "c.txt", "C.TXT", None).unwrap();
            let n = names(&z, None);
            assert!(n.contains(&"C.TXT".to_string()) && !n.contains(&"c.txt".to_string()), "{ext}: {n:?}");
            assert_eq!(content(&z, "C.TXT", None), "gamma", "{ext}");
            ren(&z, "top", "Top", None).unwrap();
            let n = names(&z, None);
            assert!(n.contains(&"Top/sub/b.txt".to_string()) && !n.iter().any(|x| x.starts_with("top")), "{ext}: {n:?}");
            assert_eq!(content(&z, "Top/a.txt", None), "alpha", "{ext}");
            assert!(leftovers(d.path()).is_empty(), "{ext}: {:?}", leftovers(d.path()));
        }
    }

    #[test]
    fn temp_name_does_not_clobber_user_file() {
        for ext in ["zip", "tar.gz", "7z"] {
            let d = tempfile::tempdir().unwrap();
            let z = zip_with(d.path(), &format!("t.{ext}"), None, None);
            let mine = d.path().join(format!("t.{ext}.rename.tmp"));
            fs::write(&mine, "user data").unwrap();
            ren(&z, "c.txt", "d.txt", None).unwrap();
            assert_eq!(fs::read_to_string(&mine).unwrap(), "user data", "{ext}");
            fs::remove_file(&mine).unwrap();
            // failures clean up too
            assert!(ren(&z, "nope", "x", None).is_err());
            assert!(leftovers(d.path()).is_empty(), "{ext}: {:?}", leftovers(d.path()));
        }
    }

    #[test]
    fn sevenz_rename_plain_and_password() {
        for pw in [None, Some("s3")] {
            let d = tempfile::tempdir().unwrap();
            let z = zip_with(d.path(), "t.7z", pw, None);
            ren(&z, "top", "t2", pw).unwrap();
            let n = names(&z, pw);
            assert!(n.contains(&"t2/sub/b.txt".to_string()), "{n:?}");
            assert_eq!(content(&z, "t2/a.txt", pw), "alpha");
            assert_eq!(content(&z, "c.txt", pw), "gamma");
            if pw.is_some() {
                let e = ren(&z, "c.txt", "q.txt", None).unwrap_err();
                assert!(e.to_string().to_lowercase().contains("password"), "{e}");
                assert!(!names(&z, pw).is_empty());
            }
            assert_eq!(fs::read_dir(d.path()).unwrap().filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().starts_with(".archiver")).count(), 0);
        }
    }

    #[test]
    fn errors() {
        for ext in ["zip", "tar.gz", "7z"] {
            let d = tempfile::tempdir().unwrap();
            let z = zip_with(d.path(), &format!("t.{ext}"), None, None);
            assert!(matches!(ren(&z, "nope", "x", None), Err(Error::NotFound(_))), "{ext}");
            match ren(&z, "c.txt", "top", None) {
                Err(Error::Io(e)) => assert_eq!(e.kind(), io::ErrorKind::AlreadyExists, "{ext}"),
                r => panic!("{ext}: {r:?}"),
            }
            match ren(&z, "c.txt", "top/a.txt", None) {
                Err(Error::Io(e)) => assert_eq!(e.kind(), io::ErrorKind::AlreadyExists, "{ext}"),
                r => panic!("{ext}: {r:?}"),
            }
            for (f, t) in [("top", "top/inner"), ("c.txt", "c.txt"), ("c.txt", ""), ("c.txt", "./c.txt/")] {
                match ren(&z, f, t, None) {
                    Err(Error::Io(e)) => assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{ext} {f}->{t}"),
                    r => panic!("{ext} {f}->{t}: {r:?}"),
                }
            }
            // untouched after failures
            assert_eq!(content(&z, "c.txt", None), "gamma");
        }
    }

    #[test]
    fn unsupported_formats() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("x.txt");
        fs::write(&f, "hi").unwrap();
        let gz = d.path().join("x.txt.gz");
        create(&gz, &[f], &CreateOptions { format: Format::Gz, level: None, password: None }, &Progress::none()).unwrap();
        assert!(matches!(ren(&gz, "x.txt", "y.txt", None), Err(Error::Unsupported(_))));
        let rar = d.path().join("a.rar");
        fs::write(&rar, b"Rar!").unwrap();
        assert!(matches!(ren(&rar, "a", "b", None), Err(Error::Unsupported(_))));
    }
}

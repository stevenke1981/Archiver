//! Convert an archive to another format (WinRAR Alt+Q "Convert archives").

use crate::archive_ops::{tempdir_near, unique_tmp};

use super::*;

/// Re-pack `src` into a new archive `dst` written with `opts` (format, level, password).
/// `src_password` opens an encrypted source. The folder structure is kept. Fails when `dst`
/// equals `src`, when `opts.format` cannot be created (RAR), or when a single-file format
/// (gz, bz2, xz, zst) would need to hold anything but exactly one file.
/// An existing `dst` is replaced only after the new archive was written completely.
pub fn convert(src: &Path, dst: &Path, opts: &CreateOptions, src_password: Option<&str>, p: &Progress) -> Result<()> {
    detect_or_err(src)?;
    if !opts.format.can_create() {
        return Err(Error::Unsupported(format!("cannot create {} archives", opts.format.label())));
    }
    let same = match (fs::canonicalize(src), fs::canonicalize(dst)) {
        (Ok(a), Ok(b)) => a == b,
        _ => src == dst,
    };
    if same {
        return Err(Error::Unsupported("output file must differ from the source archive".into()));
    }
    p.check()?;
    let work = tempdir_near(dst)?;
    let tmp = match unique_tmp(dst, "convert") {
        Ok(t) => t,
        Err(e) => {
            let _ = fs::remove_dir_all(&work);
            return Err(e);
        }
    };
    let res = (|| -> Result<()> {
        extract(src, &work, src_password, None, p)?;
        let mut inputs: Vec<PathBuf> = fs::read_dir(&work)?.filter_map(|e| e.ok().map(|e| e.path())).collect();
        inputs.sort();
        if opts.format.is_single_file() {
            let mut files = Vec::new();
            for e in WalkDir::new(&work) {
                let e = e.map_err(|e| io::Error::other(e.to_string()))?;
                if e.file_type().is_file() {
                    files.push(e.path().to_path_buf());
                }
            }
            if files.len() != 1 {
                return Err(Error::Unsupported(format!(
                    "{} can only hold exactly one file, but the source contains {}; use a TAR format instead",
                    opts.format.label(),
                    files.len()
                )));
            }
            inputs = files;
        }
        p.check()?;
        create(&tmp, &inputs, opts, p)?;
        fs::rename(&tmp, dst)?;
        Ok(())
    })();
    let _ = fs::remove_dir_all(&work);
    if res.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    res
}

/// Suggested output path for converting `src` to `fmt`: same folder, same stem, new extension.
/// When that would be `src` itself (same format) the stem gets " (converted)" appended.
pub fn convert_output(src: &Path, fmt: Format) -> PathBuf {
    let stem = archive_stem(src);
    let dir = src.parent().map(Path::to_path_buf).unwrap_or_default();
    let out = dir.join(format!("{stem}.{}", fmt.extension()));
    if out == src {
        dir.join(format!("{stem} (converted).{}", fmt.extension()))
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make(dir: &Path, name: &str, pw: Option<&str>) -> PathBuf {
        let src = dir.join("src");
        fs::create_dir_all(src.join("top/sub")).unwrap();
        fs::write(src.join("top/a.txt"), "alpha").unwrap();
        fs::write(src.join("top/sub/b.txt"), "beta").unwrap();
        fs::write(src.join("c.txt"), "gamma").unwrap();
        let out = dir.join(name);
        let fmt = Format::detect(&out).unwrap();
        create(&out, &[src.join("top"), src.join("c.txt")], &CreateOptions { format: fmt, level: None, password: pw.map(String::from) }, &Progress::none()).unwrap();
        out
    }

    fn o(fmt: Format, pw: Option<&str>) -> CreateOptions {
        CreateOptions { format: fmt, level: None, password: pw.map(String::from) }
    }

    fn verify(a: &Path, pw: Option<&str>) {
        let mut n: Vec<String> = list(a, pw).unwrap().into_iter().filter(|e| !e.is_dir).map(|e| norm_name(&e.name)).collect();
        n.sort();
        assert_eq!(n, ["c.txt", "top/a.txt", "top/sub/b.txt"], "{}", a.display());
        let rd = |name: &str| String::from_utf8(read_entry(a, name, pw, 1 << 20, &Progress::none()).unwrap().bytes).unwrap();
        assert_eq!(rd("top/a.txt"), "alpha");
        assert_eq!(rd("top/sub/b.txt"), "beta");
        assert_eq!(rd("c.txt"), "gamma");
    }

    fn conv(src: &Path, fmt: Format, spw: Option<&str>, dpw: Option<&str>) -> PathBuf {
        let dst = convert_output(src, fmt);
        convert(src, &dst, &o(fmt, dpw), spw, &Progress::none()).unwrap();
        dst
    }

    fn leftovers(dir: &Path) -> Vec<String> {
        fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_string()).filter(|n| n.starts_with(".archiver") || n.ends_with(".tmp")).collect()
    }

    #[test]
    fn zip_to_7z_to_targz_to_zip() {
        let d = tempfile::tempdir().unwrap();
        let z = make(d.path(), "t.zip", None);
        let s = conv(&z, Format::SevenZ, None, None);
        assert_eq!(s.file_name().unwrap(), "t.7z");
        verify(&s, None);
        let t = conv(&s, Format::TarGz, None, None);
        verify(&t, None);
        fs::remove_file(&z).unwrap();
        let z2 = conv(&t, Format::Zip, None, None);
        assert_eq!(z2, z);
        verify(&z2, None);
        assert!(leftovers(d.path()).is_empty(), "{:?}", leftovers(d.path()));
    }

    #[test]
    fn encrypted_source_and_target() {
        let d = tempfile::tempdir().unwrap();
        let z = make(d.path(), "t.zip", Some("pw"));
        let dst = convert_output(&z, Format::SevenZ);
        let e = convert(&z, &dst, &o(Format::SevenZ, None), None, &Progress::none()).unwrap_err();
        assert!(e.to_string().to_lowercase().contains("password"), "{e}");
        assert!(!dst.exists());
        assert!(leftovers(d.path()).is_empty());
        let s = conv(&z, Format::SevenZ, Some("pw"), Some("other"));
        verify(&s, Some("other"));
    }

    #[test]
    fn single_file_targets() {
        let d = tempfile::tempdir().unwrap();
        let one = d.path().join("only.txt");
        fs::write(&one, "solo").unwrap();
        let z = d.path().join("one.zip");
        create(&z, &[one], &o(Format::Zip, None), &Progress::none()).unwrap();
        let gz = conv(&z, Format::Gz, None, None);
        let out = d.path().join("out");
        extract(&gz, &out, None, None, &Progress::none()).unwrap();
        assert_eq!(fs::read_to_string(out.join("one")).unwrap(), "solo");
        // and back: single-file source into a zip
        let z2 = d.path().join("back.zip");
        convert(&gz, &z2, &o(Format::Zip, None), None, &Progress::none()).unwrap();
        assert_eq!(list(&z2, None).unwrap().len(), 1);

        let two = make(d.path(), "two.zip", None);
        let dst = d.path().join("two.gz");
        let e = convert(&two, &dst, &o(Format::Gz, None), None, &Progress::none()).unwrap_err();
        assert!(matches!(e, Error::Unsupported(_)), "{e}");
        assert!(!dst.exists());
        assert!(leftovers(d.path()).is_empty(), "{:?}", leftovers(d.path()));
    }

    #[test]
    fn rejects_bad_requests() {
        let d = tempfile::tempdir().unwrap();
        let z = make(d.path(), "t.zip", None);
        let r = convert(&z, &z, &o(Format::SevenZ, None), None, &Progress::none());
        assert!(matches!(r, Err(Error::Unsupported(_))));
        let r = convert(&z, &d.path().join("x.rar"), &o(Format::Rar, None), None, &Progress::none());
        assert!(matches!(r, Err(Error::Unsupported(_))));
        let odd = d.path().join("data.bin");
        fs::write(&odd, "x").unwrap();
        let r = convert(&odd, &d.path().join("o.zip"), &o(Format::Zip, None), None, &Progress::none());
        assert!(matches!(r, Err(Error::Unsupported(_))));
        verify(&z, None);
    }

    #[test]
    fn replaces_existing_dst() {
        let d = tempfile::tempdir().unwrap();
        let z = make(d.path(), "t.zip", None);
        let dst = d.path().join("t.7z");
        fs::write(&dst, "junk").unwrap();
        convert(&z, &dst, &o(Format::SevenZ, None), None, &Progress::none()).unwrap();
        verify(&dst, None);
    }

    #[test]
    fn output_names() {
        let p = Path::new("/a/b/foo.tar.gz");
        assert_eq!(convert_output(p, Format::Zip), Path::new("/a/b").join("foo.zip"));
        assert_eq!(convert_output(p, Format::TarGz), Path::new("/a/b").join("foo (converted).tar.gz"));
        assert_eq!(convert_output(Path::new("x.zip"), Format::SevenZ), PathBuf::from("x.7z"));
        assert_eq!(convert_output(Path::new("x.zip"), Format::Zip), PathBuf::from("x (converted).zip"));
    }
}

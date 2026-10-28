use super::*;

    fn sample(dir: &Path) -> PathBuf {
        let root = dir.join("data");
        fs::create_dir_all(root.join("sub/deeper")).unwrap();
        fs::write(root.join("hello.txt"), b"hello world\n".repeat(100)).unwrap();
        fs::write(root.join("sub/a.bin"), (0..50_000u32).map(|i| (i % 251) as u8).collect::<Vec<_>>()).unwrap();
        fs::write(root.join("sub/deeper/中文.txt"), "測試內容".as_bytes()).unwrap();
        fs::write(root.join("sub/empty.txt"), b"").unwrap();
        fs::create_dir_all(root.join("emptydir")).unwrap();
        root
    }

    fn assert_same(a: &Path, b: &Path) {
        for e in WalkDir::new(a) {
            let e = e.unwrap();
            let rel = e.path().strip_prefix(a).unwrap();
            let other = b.join(rel);
            if e.file_type().is_file() {
                assert_eq!(fs::read(e.path()).unwrap(), fs::read(&other).unwrap(), "{rel:?}");
            } else {
                assert!(other.is_dir(), "{rel:?} missing");
            }
        }
    }

    fn roundtrip(fmt: Format, password: Option<&str>) {
        let t = tempfile::tempdir().unwrap();
        let src = sample(t.path());
        let out = t.path().join(format!("out.{}", fmt.extension()));
        let opts = CreateOptions { format: fmt, level: None, password: password.map(String::from) };
        create(&out, &[src.clone()], &opts, &Progress::none()).unwrap();
        assert_eq!(Format::detect(&out), Some(fmt));
        let entries = list(&out, password).unwrap();
        assert!(entries.iter().any(|e| e.name.ends_with("hello.txt")), "{entries:?}");
        let dest = t.path().join("x");
        extract(&out, &dest, password, None, &Progress::none()).unwrap();
        assert_same(&src, &dest.join("data"));

        // selective extraction
        let dest2 = t.path().join("y");
        extract(&out, &dest2, password, Some(&["data/sub".to_string()]), &Progress::none()).unwrap();
        assert!(dest2.join("data/sub/a.bin").exists());
        assert!(!dest2.join("data/hello.txt").exists());
    }

    #[test] fn zip() { roundtrip(Format::Zip, None) }
    #[test] fn zip_password() { roundtrip(Format::Zip, Some("s3cret")) }
    #[test] fn sevenz() { roundtrip(Format::SevenZ, None) }
    #[test] fn sevenz_password() { roundtrip(Format::SevenZ, Some("s3cret")) }
    #[test] fn tar() { roundtrip(Format::Tar, None) }
    #[test] fn tar_gz() { roundtrip(Format::TarGz, None) }
    #[test] fn tar_bz2() { roundtrip(Format::TarBz2, None) }
    #[test] fn tar_xz() { roundtrip(Format::TarXz, None) }
    #[test] fn tar_zst() { roundtrip(Format::TarZst, None) }

    #[test]
    fn dot_slash_names_match_selection_and_preview() {
        // `tar czf x.tgz .` stores "./a.txt"; the GUI shows and selects "a.txt"
        let t = tempfile::tempdir().unwrap();
        let out = t.path().join("dot.tar.gz");
        let mut b = tar::Builder::new(flate2::write::GzEncoder::new(File::create(&out).unwrap(), flate2::Compression::default()));
        for (n, d) in [("./a.txt", &b"alpha"[..]), ("./d/b.txt", &b"beta"[..])] {
            let mut h = tar::Header::new_gnu();
            h.set_size(d.len() as u64);
            h.set_mode(0o644);
            h.as_old_mut().name[..n.len()].copy_from_slice(n.as_bytes()); // raw: set_path drops "./"
            h.set_cksum();
            b.append(&h, d).unwrap();
        }
        b.into_inner().unwrap().finish().unwrap();
        assert!(list(&out, None).unwrap().iter().any(|e| e.name.starts_with("./")));
        assert_eq!(read_entry(&out, "d/b.txt", None, 100, &Progress::none()).unwrap().bytes, b"beta");
        let dest = t.path().join("x");
        extract(&out, &dest, None, Some(&["a.txt".to_string()]), &Progress::none()).unwrap();
        assert_eq!(fs::read(dest.join("a.txt")).unwrap(), b"alpha");
        assert!(!dest.join("d").exists());
    }

    #[test]
    fn parallel_zip_same_path_entries_do_not_interleave() {
        let t = tempfile::tempdir().unwrap();
        let out = t.path().join("case.zip");
        let mut w = zip::ZipWriter::new(File::create(&out).unwrap());
        let o: zip::write::SimpleFileOptions = Default::default();
        let (big_a, big_b) = (vec![b'A'; 3 << 20], vec![b'b'; 3 << 20]);
        w.start_file("README.txt", o).unwrap();
        w.write_all(&big_a).unwrap();
        w.start_file("other.bin", o).unwrap();
        w.write_all(&vec![7u8; 5 << 20]).unwrap();
        w.start_file("readme.txt", o).unwrap();
        w.write_all(&big_b).unwrap();
        w.finish().unwrap();
        let dest = t.path().join("x");
        extract_zip(&out, &dest, None, None, 4, &Progress::none()).unwrap();
        let got = fs::read(dest.join("readme.txt")).unwrap();
        assert!(got == big_a || got == big_b, "interleaved content");
    }

    #[test]
    fn zip_wrong_password_fails() {
        let t = tempfile::tempdir().unwrap();
        let src = sample(t.path());
        let out = t.path().join("p.zip");
        create(&out, &[src], &CreateOptions { format: Format::Zip, level: Some(9), password: Some("right".into()) }, &Progress::none()).unwrap();
        assert!(extract(&out, &t.path().join("x"), Some("wrong"), None, &Progress::none()).is_err());
    }

    #[test]
    fn single_files() {
        for fmt in [Format::Gz, Format::Bz2, Format::Xz, Format::Zst] {
            let t = tempfile::tempdir().unwrap();
            let f = t.path().join("note.txt");
            fs::write(&f, b"single file payload ".repeat(500)).unwrap();
            let out = t.path().join(format!("note.txt.{}", fmt.extension()));
            create(&out, &[f.clone()], &CreateOptions { format: fmt, level: Some(fmt.level_range().unwrap().1), password: None }, &Progress::none()).unwrap();
            let l = list(&out, None).unwrap();
            assert_eq!(l[0].name, "note.txt");
            assert_eq!(l[0].size, 10_000);
            let dest = t.path().join("x");
            extract(&out, &dest, None, None, &Progress::none()).unwrap();
            assert_eq!(fs::read(dest.join("note.txt")).unwrap(), fs::read(&f).unwrap(), "{fmt:?}");
        }
    }

    #[test]
    fn crc_and_test_and_delete() {
        for fmt in [Format::Zip, Format::SevenZ, Format::TarGz, Format::TarZst] {
            let t = tempfile::tempdir().unwrap();
            let src = sample(t.path());
            let out = t.path().join(format!("a.{}", fmt.extension()));
            create(&out, &[src], &CreateOptions { format: fmt, level: None, password: None }, &Progress::none()).unwrap();
            let l = list(&out, None).unwrap();
            let hello = l.iter().find(|e| e.name.ends_with("hello.txt")).unwrap();
            if matches!(fmt, Format::Zip | Format::SevenZ) {
                assert_eq!(hello.crc32, Some(crc32fast::hash(&b"hello world\n".repeat(100))), "{fmt:?}");
            }
            assert_eq!(test(&out, None, &Progress::none()).unwrap(), 4, "{fmt:?}");
            delete(&out, &["data/sub".to_string()], None, &Progress::none()).unwrap();
            let names: Vec<String> = list(&out, None).unwrap().into_iter().map(|e| e.name).collect();
            assert!(names.iter().any(|n| n.ends_with("hello.txt")), "{fmt:?} {names:?}");
            assert!(!names.iter().any(|n| n.contains("sub/")), "{fmt:?} {names:?}");
            assert!(!t.path().join(format!("a.{}.rewrite.tmp", fmt.extension())).exists());
        }
    }

    #[test]
    fn test_detects_corruption() {
        let t = tempfile::tempdir().unwrap();
        let src = sample(t.path());
        let out = t.path().join("c.zip");
        create(&out, &[src], &CreateOptions { format: Format::Zip, level: Some(0), password: None }, &Progress::none()).unwrap();
        let mut b = fs::read(&out).unwrap();
        let pos = b.windows(11).position(|w| w == b"hello world").unwrap();
        b[pos] ^= 0xFF;
        fs::write(&out, b).unwrap();
        assert!(test(&out, None, &Progress::none()).is_err());
    }

    #[test]
    fn cancel_removes_partial_output() {
        for fmt in [Format::Zip, Format::SevenZ, Format::TarGz, Format::TarXz, Format::TarZst] {
            let t = tempfile::tempdir().unwrap();
            let src = sample(t.path());
            let out = t.path().join(format!("c.{}", fmt.extension()));
            let p = Progress { on: &|_, _, _| {}, cancel: Arc::new(AtomicBool::new(true)) };
            let r = create(&out, &[src], &CreateOptions { format: fmt, level: None, password: None }, &p);
            assert!(matches!(r, Err(Error::Cancelled)), "{fmt:?}: {r:?}");
            assert!(!out.exists(), "{fmt:?}");
            assert_eq!(fs::read_dir(t.path()).unwrap().count(), 1, "{fmt:?}: temp files left");
        }
    }

    #[test]
    fn parallel_gzip_multi_member() {
        let t = tempfile::tempdir().unwrap();
        let f = t.path().join("big.bin");
        let data: Vec<u8> = (0..(GZ_BLOCK * 2 + 12345)).map(|i| ((i * 31) ^ (i >> 7)) as u8).collect();
        fs::write(&f, &data).unwrap();
        let out = t.path().join("big.bin.gz");
        create(&out, &[f], &CreateOptions { format: Format::Gz, level: Some(6), password: None }, &Progress::none()).unwrap();
        // standard decoder must read all members
        let mut d = flate2::read::MultiGzDecoder::new(File::open(&out).unwrap());
        let mut back = Vec::new();
        d.read_to_end(&mut back).unwrap();
        assert_eq!(back, data);
    }

    #[test]
    fn xz_concatenated_streams() {
        let t = tempfile::tempdir().unwrap();
        let mut all = Vec::new();
        let mut want = Vec::new();
        for part in [b"first part ".repeat(1000), b"second part ".repeat(2000)] {
            let f = t.path().join("p.txt");
            fs::write(&f, &part).unwrap();
            let x = t.path().join("p.txt.xz");
            create(&x, &[f], &CreateOptions { format: Format::Xz, level: Some(1), password: None }, &Progress::none()).unwrap();
            all.extend(fs::read(&x).unwrap());
            all.extend([0u8; 4]); // stream padding
            want.extend(part);
        }
        let cat = t.path().join("cat.txt.xz");
        fs::write(&cat, &all).unwrap();
        let dest = t.path().join("out");
        extract(&cat, &dest, None, None, &Progress::none()).unwrap();
        assert_eq!(fs::read(dest.join("cat.txt")).unwrap(), want);
    }

    #[test]
    fn single_file_rejects_folder() {
        let t = tempfile::tempdir().unwrap();
        let src = sample(t.path());
        let out = t.path().join("x.gz");
        assert!(create(&out, &[src], &CreateOptions { format: Format::Gz, level: None, password: None }, &Progress::none()).is_err());
        assert!(!out.exists());
    }

    #[test]
    fn rar_create_unsupported() {
        assert!(!Format::Rar.can_create());
        assert!(!Format::writable().contains(&Format::Rar));
    }

    #[test]
    fn detect_formats() {
        assert_eq!(Format::detect(Path::new("a.TGZ")), Some(Format::TarGz));
        assert_eq!(Format::detect(Path::new("a.tar.zst")), Some(Format::TarZst));
        assert_eq!(Format::detect(Path::new("a.rar")), Some(Format::Rar));
        assert_eq!(Format::detect(Path::new("a.txt")), None);
    }

    #[test]
    fn stems() {
        assert_eq!(archive_stem(Path::new("/x/My.Backup.tar.GZ")), "My.Backup");
        assert_eq!(archive_stem(Path::new("a.zip")), "a");
        assert_eq!(archive_stem(Path::new("notes.txt")), "notes.txt");
        assert!(archive_extensions().contains(&"tar.zst"));
    }

    fn read(a: &Path, name: &str, pw: Option<&str>, max: usize) -> Result<EntryData> {
        read_entry(a, name, pw, max, &Progress::none())
    }

    fn read_entry_cases(fmt: Format, pw: Option<&str>) {
        let t = tempfile::tempdir().unwrap();
        let src = sample(t.path());
        let out = t.path().join(format!("r.{}", fmt.extension()));
        create(&out, &[src.clone()], &CreateOptions { format: fmt, level: None, password: pw.map(String::from) }, &Progress::none()).unwrap();
        let hello = fs::read(src.join("hello.txt")).unwrap();
        assert_eq!(read(&out, "data/hello.txt", pw, 1 << 20).unwrap(), EntryData { bytes: hello.clone(), truncated: false }, "{fmt:?}");
        assert_eq!(read(&out, "data/sub/deeper/中文.txt", pw, 100).unwrap().bytes, "測試內容".as_bytes(), "{fmt:?}");
        let bin = fs::read(src.join("sub/a.bin")).unwrap();
        let r = read(&out, "data/sub/a.bin", pw, 1000).unwrap();
        assert!(r.truncated && r.bytes == bin[..1000], "{fmt:?}");
        assert_eq!(read(&out, "data/sub/a.bin", pw, bin.len()).unwrap(), EntryData { bytes: bin.clone(), truncated: false }, "{fmt:?}");
        assert_eq!(read(&out, "data/hello.txt", pw, 0).unwrap(), EntryData { bytes: vec![], truncated: true }, "{fmt:?}");
        assert_eq!(read(&out, "data/sub/empty.txt", pw, 0).unwrap(), EntryData { bytes: vec![], truncated: false }, "{fmt:?}");
        // every listed file entry is readable by its listed name; directories are not
        for e in list(&out, pw).unwrap() {
            let r = read(&out, &e.name, pw, usize::MAX);
            if e.is_dir {
                assert!(matches!(r, Err(Error::NotFound(_))), "{fmt:?} {}: {r:?}", e.name);
            } else {
                assert_eq!(r.unwrap().bytes.len() as u64, e.size, "{fmt:?} {}", e.name);
            }
        }
        for missing in ["data/nope.txt", "data/sub", "data/sub/", "data", "hello.txt"] {
            assert!(matches!(read(&out, missing, pw, 10), Err(Error::NotFound(_))), "{fmt:?} {missing}");
        }
        let p = Progress { on: &|_, _, _| {}, cancel: Arc::new(AtomicBool::new(true)) };
        assert!(matches!(read_entry(&out, "data/hello.txt", pw, 10, &p), Err(Error::Cancelled)), "{fmt:?}");
    }

    #[test] fn read_entry_zip() { read_entry_cases(Format::Zip, None) }
    #[test] fn read_entry_zip_password() { read_entry_cases(Format::Zip, Some("s3cret")) }
    #[test] fn read_entry_7z() { read_entry_cases(Format::SevenZ, None) }
    #[test] fn read_entry_7z_password() { read_entry_cases(Format::SevenZ, Some("s3cret")) }
    #[test] fn read_entry_tar() { read_entry_cases(Format::Tar, None) }
    #[test] fn read_entry_tar_gz() { read_entry_cases(Format::TarGz, None) }
    #[test] fn read_entry_tar_zst() { read_entry_cases(Format::TarZst, None) }

    #[test]
    fn read_entry_wrong_password() {
        for fmt in [Format::Zip, Format::SevenZ] {
            let t = tempfile::tempdir().unwrap();
            let src = sample(t.path());
            let out = t.path().join(format!("w.{}", fmt.extension()));
            create(&out, &[src], &CreateOptions { format: fmt, level: None, password: Some("right".into()) }, &Progress::none()).unwrap();
            assert!(read(&out, "data/hello.txt", Some("wrong"), 100).is_err(), "{fmt:?}");
            assert!(read(&out, "data/hello.txt", None, 100).is_err(), "{fmt:?}");
        }
    }

    #[test]
    fn read_entry_7z_multi_block() {
        // non-solid archive: one block per file, only the entry's block is decoded
        let t = tempfile::tempdir().unwrap();
        let out = t.path().join("m.7z");
        let mut w = sevenz_rust2::ArchiveWriter::create(&out).unwrap();
        let parts: Vec<(String, Vec<u8>)> = (0..4).map(|k| (format!("f{k}.txt"), format!("block {k} ").repeat(1000 * (k + 1)).into_bytes())).collect();
        for (name, data) in &parts {
            w.push_archive_entry(sevenz_rust2::ArchiveEntry::new_file(name), Some(data.as_slice())).unwrap();
        }
        w.finish().unwrap();
        assert_eq!(sevenz_rust2::Archive::open(&out).unwrap().blocks.len(), 4);
        for (name, data) in parts.iter().rev() {
            assert_eq!(&read(&out, name, None, usize::MAX).unwrap().bytes, data);
            let r = read(&out, name, None, 7).unwrap();
            assert!(r.truncated && r.bytes == data[..7]);
        }
    }

    #[test]
    fn read_entry_single_files() {
        for fmt in [Format::Gz, Format::Bz2, Format::Xz, Format::Zst] {
            let t = tempfile::tempdir().unwrap();
            let f = t.path().join("note.txt");
            let data = b"single file payload ".repeat(500);
            fs::write(&f, &data).unwrap();
            let out = t.path().join(format!("note.txt.{}", fmt.extension()));
            create(&out, &[f], &CreateOptions { format: fmt, level: None, password: None }, &Progress::none()).unwrap();
            assert_eq!(read(&out, "note.txt", None, data.len()).unwrap(), EntryData { bytes: data.clone(), truncated: false }, "{fmt:?}");
            let r = read(&out, "note.txt", None, 33).unwrap();
            assert!(r.truncated && r.bytes == data[..33], "{fmt:?}");
            assert_eq!(read(&out, "note.txt", None, 0).unwrap(), EntryData { bytes: vec![], truncated: true }, "{fmt:?}");
            assert!(matches!(read(&out, "other.txt", None, 10), Err(Error::NotFound(_))), "{fmt:?}");
        }
    }

    /// Many files of mixed sizes (0 B .. ~1.5 MiB) in nested folders.
    fn mixed_tree(dir: &Path) -> PathBuf {
        let root = dir.join("mix");
        for k in 0..60usize {
            let d = root.join(format!("d{}/e{}", k % 5, k % 3));
            fs::create_dir_all(&d).unwrap();
            let len = match k % 6 { 0 => 0, 1 => 17, 2 => 4096, 3 => 70_000, 4 => 300_000, _ => 1_500_000 } + k;
            let data: Vec<u8> = (0..len).map(|i| ((i * 7 + k * 13) % 251) as u8 ^ (i >> 9) as u8).collect();
            fs::write(d.join(format!("f{k}.bin")), data).unwrap();
        }
        fs::create_dir_all(root.join("empty/inner")).unwrap();
        root
    }

    #[test]
    fn zip_parallel_extraction() {
        let t = tempfile::tempdir().unwrap();
        let src = mixed_tree(t.path());
        let out = t.path().join("m.zip");
        create(&out, &[src.clone()], &CreateOptions { format: Format::Zip, level: Some(1), password: None }, &Progress::none()).unwrap();
        for th in [1, 2, 4, 8] {
            let dest = t.path().join(format!("all{th}"));
            fs::create_dir_all(&dest).unwrap();
            extract_zip(&out, &dest, None, None, th, &Progress::none()).unwrap();
            assert_same(&src, &dest.join("mix"));
            // subset: one folder plus one single file
            let dest = t.path().join(format!("sub{th}"));
            fs::create_dir_all(&dest).unwrap();
            let sel = ["mix/d1".to_string(), "mix/d2/e2/f2.bin".to_string()];
            extract_zip(&out, &dest, None, Some(&sel), th, &Progress::none()).unwrap();
            assert_same(&src.join("d1"), &dest.join("mix/d1"));
            assert_eq!(fs::read(dest.join("mix/d2/e2/f2.bin")).unwrap(), fs::read(src.join("d2/e2/f2.bin")).unwrap());
            assert!(!dest.join("mix/d0").exists() && !dest.join("mix/d2/e0").exists() && !dest.join("mix/empty").exists());
            // progress reaches the total once all bytes are written
            let last = std::sync::Mutex::new((0, 0));
            let on = |d, tot, _: &str| *last.lock().unwrap() = (d, tot);
            let p = Progress { on: &on, cancel: Arc::new(AtomicBool::new(false)) };
            let dest = t.path().join(format!("p{th}"));
            fs::create_dir_all(&dest).unwrap();
            extract_zip(&out, &dest, None, None, th, &p).unwrap();
            let (d, tot) = *last.lock().unwrap();
            assert!(d == tot && tot > 0, "{th}: {d}/{tot}");
            // cancellation
            let p = Progress { on: &|_, _, _| {}, cancel: Arc::new(AtomicBool::new(true)) };
            assert!(matches!(extract_zip(&out, &dest, None, None, th, &p), Err(Error::Cancelled)));
        }
    }

    #[test]
    fn zip_parallel_wrong_password() {
        let t = tempfile::tempdir().unwrap();
        let src = mixed_tree(t.path());
        let out = t.path().join("m.zip");
        create(&out, &[src.clone()], &CreateOptions { format: Format::Zip, level: Some(1), password: Some("right".into()) }, &Progress::none()).unwrap();
        for d in ["x", "y"] { fs::create_dir_all(t.path().join(d)).unwrap(); }
        assert!(extract_zip(&out, &t.path().join("x"), Some("wrong"), None, 4, &Progress::none()).is_err());
        extract_zip(&out, &t.path().join("y"), Some("right"), None, 4, &Progress::none()).unwrap();
        assert_same(&src, &t.path().join("y/mix"));
    }

    #[test]
    fn zip_partial_skips_unselected_encrypted() {
        let t = tempfile::tempdir().unwrap();
        let out = t.path().join("mixed.zip");
        let mut z = zip::ZipWriter::new(File::create(&out).unwrap());
        let opt = zip::write::SimpleFileOptions::default();
        z.start_file("a.txt", opt.with_aes_encryption(zip::AesMode::Aes256, "pw1")).unwrap();
        z.write_all(b"alpha").unwrap();
        z.start_file("b.txt", opt.with_aes_encryption(zip::AesMode::Aes256, "other")).unwrap();
        z.write_all(b"bravo").unwrap();
        z.start_file("c.txt", opt).unwrap();
        z.write_all(b"charlie").unwrap();
        z.finish().unwrap();
        let sel = ["a.txt".to_string(), "c.txt".to_string()];
        for th in [1, 4] {
            let dest = t.path().join(format!("x{th}"));
            fs::create_dir_all(&dest).unwrap();
            extract_zip(&out, &dest, Some("pw1"), Some(&sel), th, &Progress::none()).unwrap();
            assert_eq!(fs::read(dest.join("a.txt")).unwrap(), b"alpha");
            assert_eq!(fs::read(dest.join("c.txt")).unwrap(), b"charlie");
            assert!(!dest.join("b.txt").exists());
            let all = t.path().join(format!("all{th}"));
            fs::create_dir_all(&all).unwrap();
            assert!(extract_zip(&out, &all, Some("pw1"), None, th, &Progress::none()).is_err());
        }
        extract(&out, &t.path().join("e"), Some("pw1"), Some(&sel), &Progress::none()).unwrap();
        assert_eq!(read(&out, "b.txt", Some("other"), 100).unwrap().bytes, b"bravo");
        assert_eq!(read(&out, "c.txt", None, 100).unwrap().bytes, b"charlie");
    }

    #[test]
    fn mtime_preserved() {
        let when = std::time::SystemTime::now() - std::time::Duration::from_secs(3 * 86_400 + 1234);
        for fmt in [Format::Zip, Format::SevenZ, Format::TarGz] {
            let t = tempfile::tempdir().unwrap();
            let src = sample(t.path());
            for f in ["hello.txt", "sub/a.bin", "sub/empty.txt"] {
                File::options().write(true).open(src.join(f)).unwrap().set_modified(when).unwrap();
            }
            let out = t.path().join(format!("t.{}", fmt.extension()));
            create(&out, &[src], &CreateOptions { format: fmt, level: None, password: None }, &Progress::none()).unwrap();
            let dest = t.path().join("x");
            extract(&out, &dest, None, None, &Progress::none()).unwrap();
            for f in ["hello.txt", "sub/a.bin", "sub/empty.txt"] {
                let m = fs::metadata(dest.join("data").join(f)).unwrap().modified().unwrap();
                let diff = m.duration_since(when).unwrap_or_else(|e| e.duration());
                assert!(diff <= std::time::Duration::from_secs(2), "{fmt:?} {f}: off by {diff:?}");
            }
        }
    }

    #[test]
    fn sevenz_lists_encryption() {
        for pw in [None, Some("s3cret")] {
            let t = tempfile::tempdir().unwrap();
            let src = sample(t.path());
            let out = t.path().join("e.7z");
            create(&out, &[src], &CreateOptions { format: Format::SevenZ, level: None, password: pw.map(String::from) }, &Progress::none()).unwrap();
            let l = list(&out, pw).unwrap();
            let hello = l.iter().find(|e| e.name.ends_with("hello.txt")).unwrap();
            assert_eq!(hello.encrypted, pw.is_some());
            assert!(l.iter().filter(|e| e.is_dir).all(|e| !e.encrypted));
        }
    }

    #[test]
    fn safe_join_blocks_traversal() {
        assert!(safe_join(Path::new("/d"), "../etc/passwd").is_none());
        assert!(safe_join(Path::new("/d"), "/abs").is_none());
        assert_eq!(safe_join(Path::new("/d"), "a/b").unwrap(), Path::new("/d/a/b"));
    }

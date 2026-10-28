use super::*;

    use archiver_core::Error;

    fn r(input: &str, result: archiver_core::Result<()>) -> BatchResult {
        BatchResult { input: PathBuf::from(input), output: PathBuf::from("out"), result }
    }

    #[test]
    fn selected_extraction_never_falls_back_to_all() {
        assert!(extraction_names(&[], ExtractScope::Selected).is_err());
        assert!(extraction_names(&[Item::parent()], ExtractScope::Selected).is_err());
        assert_eq!(extraction_names(&[], ExtractScope::All), Ok(None));
        let item = Item { name: "chosen.txt".into(), full: "folder/chosen.txt".into(), is_dir: false, is_parent: false, size: None, packed: None, modified: String::new(), crc: None };
        assert_eq!(extraction_names(std::slice::from_ref(&item), ExtractScope::Selected), Ok(Some(vec!["folder/chosen.txt".into()])));
        assert_eq!(extraction_names(std::slice::from_ref(&item), ExtractScope::All), Ok(None));
    }

    #[test]
    fn manager_selection_extracts_only_chosen_files_and_folder_descendants() {
        let t = tempfile::tempdir().unwrap();
        let src = t.path().join("data");
        std::fs::create_dir_all(src.join("folder/nested")).unwrap();
        std::fs::write(src.join("folder/chosen.txt"), b"chosen").unwrap();
        std::fs::write(src.join("folder/skip.txt"), b"skip").unwrap();
        std::fs::write(src.join("folder/nested/中文.txt"), b"nested").unwrap();
        std::fs::write(src.join("other.txt"), b"other").unwrap();
        let archive = t.path().join("selection.zip");
        let options = CreateOptions { format: Format::Zip, level: None, password: None };
        archiver_core::create(&archive, &[src], &options, &Progress::none()).unwrap();
        let entries = archiver_core::list(&archive, None).unwrap();

        let mut rows = view::archive_children(&entries, "data/folder/");
        view::sort_items(&mut rows, 0, false);
        let chosen: Vec<Item> = rows.into_iter().filter(|i| i.name == "chosen.txt").collect();
        let names = extraction_names(&chosen, ExtractScope::Selected).unwrap().unwrap();
        let out = t.path().join("single");
        archiver_core::extract(&archive, &out, None, Some(&names), &Progress::none()).unwrap();
        assert_eq!(std::fs::read(out.join("data/folder/chosen.txt")).unwrap(), b"chosen");
        assert!(!out.join("data/folder/skip.txt").exists());
        assert!(!out.join("data/other.txt").exists());

        let results = view::archive_search(&entries, "中文");
        let names = extraction_names(&results, ExtractScope::Selected).unwrap().unwrap();
        let out = t.path().join("search");
        archiver_core::extract(&archive, &out, None, Some(&names), &Progress::none()).unwrap();
        assert_eq!(std::fs::read(out.join("data/folder/nested/中文.txt")).unwrap(), b"nested");
        assert!(!out.join("data/folder/chosen.txt").exists());

        let folder: Vec<Item> = view::archive_children(&entries, "data/").into_iter().filter(|i| i.name == "folder").collect();
        let names = extraction_names(&folder, ExtractScope::Selected).unwrap().unwrap();
        let out = t.path().join("folder");
        archiver_core::extract(&archive, &out, None, Some(&names), &Progress::none()).unwrap();
        assert!(out.join("data/folder/chosen.txt").is_file());
        assert!(out.join("data/folder/skip.txt").is_file());
        assert!(out.join("data/folder/nested/中文.txt").is_file());
        assert!(!out.join("data/other.txt").exists());
    }

    #[test]
    fn summary() {
        let ok = vec![r("/a/x.zip", Ok(())), r("/a/y.zip", Ok(()))];
        assert_eq!(batch_summary(&ok), ("已完成 2 個（成功 2，失敗 0）".to_string(), 0));
        assert_eq!(single_password_failure(&ok), None);

        let mixed = vec![r("/a/x.zip", Ok(())), r("/a/b c.7z", Err(Error::Cancelled))];
        let (text, failed) = batch_summary(&mixed);
        assert_eq!(failed, 1);
        assert_eq!(text, "已完成 2 個（成功 1，失敗 1）\n✗ b c.7z：operation cancelled");

        let many: Vec<BatchResult> = (0..25).map(|i| r(&format!("/d/{i}.zip"), Err(Error::Unsupported("x".into())))).collect();
        let (text, failed) = batch_summary(&many);
        assert_eq!(failed, 25);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 1 + 20 + 1);
        assert_eq!(lines[1], "✗ 0.zip：unsupported format: x");
        assert_eq!(lines[21], "…還有 5 個");
    }

    #[test]
    fn password_failure() {
        let pw = || Err(Error::SevenZ("wrong password".into()));
        assert_eq!(single_password_failure(&[r("/a/x.zip", Ok(())), r("/a/e.7z", pw())]), Some(PathBuf::from("/a/e.7z")));
        assert_eq!(single_password_failure(&[r("/a/e.7z", pw()), r("/a/f.7z", pw())]), None);
        assert_eq!(single_password_failure(&[r("/a/x.zip", Err(Error::Cancelled))]), None);
        assert!(is_password_err("Password required") && !is_password_err("I/O error"));
    }

    #[test]
    fn meta_and_folder() {
        let it = Item { name: "a.txt".into(), full: "d/a.txt".into(), is_dir: false, is_parent: false, size: Some(12345), packed: None, modified: "2026-10-10 12:00".into(), crc: Some(0xAB) };
        assert_eq!(item_meta(&it), "12,345 位元組　2026/10/10 12:00　CRC32 000000AB");
        assert_eq!(item_meta(&Item::parent()), "");
        let e = |name: &str, size: u64, is_dir: bool| Entry { name: name.into(), size, compressed: None, modified: None, is_dir, crc32: None, encrypted: false };
        let es = vec![e("d/", 0, true), e("d/a.txt", 1000, false), e("d/s/t/b.bin", 234, false), e("d/e/", 0, true), e("dx/c", 5, false), e("x", 1, false)];
        assert_eq!(folder_summary(&es, "d"), "包含 2 個檔案、3 個資料夾\n共 1,234 個位元組");
        assert_eq!(folder_summary(&es, "nope"), "包含 0 個檔案、0 個資料夾\n共 0 個位元組");
    }

    #[test]
    fn head_of_file() {
        let t = tempfile::tempdir().unwrap();
        let f = t.path().join("f.bin");
        std::fs::write(&f, b"0123456789").unwrap();
        assert_eq!(read_head(&f, 4).unwrap(), (b"0123".to_vec(), true));
        assert_eq!(read_head(&f, 10).unwrap(), (b"0123456789".to_vec(), false));
        assert_eq!(read_head(&f, 64).unwrap(), (b"0123456789".to_vec(), false));
        assert!(read_head(&t.path().join("missing"), 4).is_err());
    }

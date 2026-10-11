//! Pure listing logic for the main window: virtual folders inside archives, local folder
//! listing, sorting, type names and number formatting (kept separate from the UI for tests).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use archiver_core::Entry;

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    /// Display name (last path component, or full path for search results).
    pub name: String,
    /// Archive entry name (archive mode, folders without trailing '/') or file-system path.
    pub full: String,
    pub is_dir: bool,
    pub is_parent: bool,
    pub size: Option<u64>,
    pub packed: Option<u64>,
    pub modified: String,
    pub crc: Option<u32>,
}

impl Item {
    pub fn parent() -> Self {
        Item { name: "..".into(), full: String::new(), is_dir: true, is_parent: true, size: None, packed: None, modified: String::new(), crc: None }
    }
}

/// Immediate children of `prefix` ("" or "a/b/") in a flat archive listing.
/// Folders that exist only implicitly (as path prefixes) are synthesised.
pub fn archive_children(entries: &[Entry], prefix: &str) -> Vec<Item> {
    let mut dirs: BTreeMap<String, Item> = BTreeMap::new();
    let mut files = Vec::new();
    for e in entries {
        let name = e.name.replace('\\', "/");
        let name = name.trim_start_matches("./").trim_start_matches('/');
        let Some(rest) = name.strip_prefix(prefix) else { continue };
        let rest = rest.trim_end_matches('/');
        if rest.is_empty() {
            continue;
        }
        match rest.split_once('/') {
            Some((dir, _)) => {
                let full = format!("{prefix}{dir}");
                dirs.entry(dir.to_string()).or_insert_with(|| Item { name: dir.into(), full, is_dir: true, is_parent: false, size: None, packed: None, modified: String::new(), crc: None });
            }
            None if e.is_dir => {
                let it = dirs.entry(rest.to_string()).or_insert_with(|| Item { name: rest.into(), full: format!("{prefix}{rest}"), is_dir: true, is_parent: false, size: None, packed: None, modified: String::new(), crc: None });
                it.modified = e.modified.clone().unwrap_or_default();
            }
            None => files.push(Item {
                name: rest.into(),
                full: name.to_string(),
                is_dir: false,
                is_parent: false,
                size: Some(e.size),
                packed: e.compressed,
                modified: e.modified.clone().unwrap_or_default(),
                crc: e.crc32,
            }),
        }
    }
    let mut v: Vec<Item> = dirs.into_values().collect();
    v.extend(files);
    v
}

/// All entries whose name contains `needle` (case-insensitive), shown with their full path.
pub fn archive_search(entries: &[Entry], needle: &str) -> Vec<Item> {
    let n = needle.to_lowercase();
    entries
        .iter()
        .filter(|e| e.name.to_lowercase().contains(&n))
        .map(|e| {
            let full = e.name.trim_end_matches('/').to_string();
            Item { name: full.clone(), full, is_dir: e.is_dir, is_parent: false, size: (!e.is_dir).then_some(e.size), packed: e.compressed, modified: e.modified.clone().unwrap_or_default(), crc: e.crc32 }
        })
        .collect()
}

pub fn fs_children(dir: &Path) -> std::io::Result<Vec<Item>> {
    let mut v = Vec::new();
    for e in std::fs::read_dir(dir)? {
        let Ok(e) = e else { continue };
        let path = e.path();
        let md = std::fs::metadata(&path).ok();
        let is_dir = md.as_ref().map(|m| m.is_dir()).unwrap_or(false);
        let modified = md
            .as_ref()
            .and_then(|m| m.modified().ok())
            .map(|t| chrono::DateTime::<chrono::Local>::from(t).format("%Y/%m/%d %H:%M").to_string())
            .unwrap_or_default();
        v.push(Item {
            name: e.file_name().to_string_lossy().to_string(),
            full: path.display().to_string(),
            is_dir,
            is_parent: false,
            size: (!is_dir).then(|| md.as_ref().map(|m| m.len()).unwrap_or(0)),
            packed: None,
            modified,
            crc: None,
        });
    }
    Ok(v)
}

/// Sort by column (0 name, 1 size, 2 packed, 3 type, 4 date, 5 crc); folders stay on top
/// and the ".." row first.
pub fn sort_items(items: &mut [Item], col: i32, asc: bool) {
    items.sort_by(|a, b| {
        use std::cmp::Ordering;
        let top = b.is_parent.cmp(&a.is_parent).then(b.is_dir.cmp(&a.is_dir));
        if top != Ordering::Equal {
            return top;
        }
        let o = match col {
            1 => a.size.cmp(&b.size),
            2 => a.packed.cmp(&b.packed),
            3 => type_name(&a.name, a.is_dir).cmp(&type_name(&b.name, b.is_dir)),
            4 => a.modified.cmp(&b.modified),
            5 => a.crc.cmp(&b.crc),
            _ => Ordering::Equal,
        }
        .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        if asc { o } else { o.reverse() }
    });
}

fn ext(name: &str) -> String {
    Path::new(name).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
}

/// Icon id used by the UI: 0 parent, 1 folder, 2 file, 3 archive, 4 exe, 5 text, 6 image.
pub fn icon(it: &Item) -> i32 {
    if it.is_parent {
        return 0;
    }
    if it.is_dir {
        return 1;
    }
    if archiver_core::Format::detect(Path::new(&it.name)).is_some() {
        return 3;
    }
    match ext(&it.name).as_str() {
        "exe" | "msi" | "bat" | "cmd" | "com" | "app" | "sh" | "appimage" => 4,
        "txt" | "md" | "log" | "csv" | "json" | "toml" | "yaml" | "yml" | "xml" | "ini" | "rs" | "c" | "h" | "cpp" | "py" | "js" | "ts" | "html" | "css" => 5,
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "svg" | "ico" | "tif" | "tiff" => 6,
        _ => 2,
    }
}

/// Windows-Explorer-style type description (zh-TW).
pub fn type_name(name: &str, is_dir: bool) -> String {
    if is_dir {
        return "檔案資料夾".into();
    }
    if let Some(f) = archiver_core::Format::detect(Path::new(name)) {
        return format!("{} 壓縮檔", f.label().split(' ').next().unwrap_or(""));
    }
    let e = ext(name);
    let known = match e.as_str() {
        "exe" | "com" => "應用程式",
        "msi" => "Windows 安裝程式套件",
        "dll" => "應用程式擴充",
        "txt" => "文字文件",
        "md" => "Markdown 來源檔案",
        "log" => "文字文件",
        "csv" => "CSV 檔案",
        "json" => "JSON 檔案",
        "pdf" => "PDF 文件",
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" => "圖片",
        "svg" => "SVG 文件",
        "mp4" | "mkv" | "avi" | "mov" => "視訊檔",
        "mp3" | "wav" | "flac" | "ogg" => "音訊檔",
        "rs" => "Rust 原始碼",
        "html" | "htm" => "HTML 文件",
        "doc" | "docx" => "Word 文件",
        "xls" | "xlsx" => "Excel 工作表",
        "" => "檔案",
        _ => "",
    };
    if known.is_empty() { format!("{} 檔案", e.to_uppercase()) } else { known.into() }
}

/// 10263323 -> "10,263,323"
pub fn fmt_num(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Parent of an archive-internal prefix: "a/b/" -> "a/", "a/" -> "".
pub fn parent_prefix(prefix: &str) -> String {
    let t = prefix.trim_end_matches('/');
    match t.rfind('/') {
        Some(i) => t[..=i].to_string(),
        None => String::new(),
    }
}

/// Text in the path bar for archive mode, WinRAR style.
pub fn archive_path_text(archive: &Path, prefix: &str, fmt_label: &str, unpacked: u64) -> String {
    let name = archive.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let mut p = name;
    let sub = prefix.trim_end_matches('/');
    if !sub.is_empty() {
        p.push('\\');
        p.push_str(&sub.replace('/', "\\"));
    }
    format!("{p} - {fmt_label} 壓縮檔, 未封裝大小 {} 位元組", fmt_num(unpacked))
}

pub fn temp_view_dir() -> PathBuf {
    std::env::temp_dir().join(format!("archiver-view-{}", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(name: &str, size: u64, dir: bool) -> Entry {
        Entry { name: name.into(), size, compressed: Some(size / 2), modified: Some("2026-10-10 12:00".into()), is_dir: dir, crc32: (!dir).then_some(0xAB), encrypted: false }
    }

    #[test]
    fn virtual_folders() {
        let es = vec![e("a/", 0, true), e("a/x.txt", 10, false), e("b/c/d.bin", 5, false), e("top.md", 3, false)];
        let root = archive_children(&es, "");
        let names: Vec<_> = root.iter().map(|i| (i.name.as_str(), i.is_dir)).collect();
        assert_eq!(names, vec![("a", true), ("b", true), ("top.md", false)]);
        assert_eq!(root[1].full, "b");
        let b = archive_children(&es, "b/");
        assert_eq!(b.len(), 1);
        assert!(b[0].is_dir && b[0].full == "b/c");
        let c = archive_children(&es, "b/c/");
        assert_eq!(c[0].full, "b/c/d.bin");
        assert_eq!(c[0].crc, Some(0xAB));
        assert_eq!(parent_prefix("b/c/"), "b/");
        assert_eq!(parent_prefix("b/"), "");
    }

    #[test]
    fn search_and_sort() {
        let es = vec![e("a/Readme.TXT", 10, false), e("b/readme.md", 50, false), e("z.bin", 1, false)];
        let mut r = archive_search(&es, "readme");
        assert_eq!(r.len(), 2);
        r.insert(0, Item::parent());
        r.push(Item { name: "dir".into(), full: "dir".into(), is_dir: true, is_parent: false, size: None, packed: None, modified: String::new(), crc: None });
        sort_items(&mut r, 1, false);
        assert!(r[0].is_parent);
        assert_eq!(r[1].name, "dir");
        assert_eq!(r[2].name, "b/readme.md");
        sort_items(&mut r, 0, true);
        assert_eq!(r[2].name, "a/Readme.TXT");
    }

    #[test]
    fn formatting() {
        assert_eq!(fmt_num(0), "0");
        assert_eq!(fmt_num(999), "999");
        assert_eq!(fmt_num(1000), "1,000");
        assert_eq!(fmt_num(10263323), "10,263,323");
        assert_eq!(type_name("README.md", false), "Markdown 來源檔案");
        assert_eq!(type_name("rarc.exe", false), "應用程式");
        assert_eq!(type_name("LICENSE", false), "檔案");
        assert_eq!(type_name("x.tar.gz", false), "TAR.GZ 壓縮檔");
        assert_eq!(type_name("foo.xyz", false), "XYZ 檔案");
        assert_eq!(type_name("d", true), "檔案資料夾");
        assert_eq!(
            archive_path_text(Path::new("/t/archiver-windows-x64.zip"), "archiver-windows-x64/", "ZIP", 10263323),
            "archiver-windows-x64.zip\\archiver-windows-x64 - ZIP 壓縮檔, 未封裝大小 10,263,323 位元組"
        );
        let it = |n: &str| Item { name: n.into(), full: n.into(), is_dir: false, is_parent: false, size: None, packed: None, modified: String::new(), crc: None };
        assert_eq!(icon(&it("a.zip")), 3);
        assert_eq!(icon(&it("a.exe")), 4);
        assert_eq!(icon(&it("a.md")), 5);
        assert_eq!(icon(&Item::parent()), 0);
    }

    #[test]
    fn fs_listing() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join("sub")).unwrap();
        std::fs::write(t.path().join("f.txt"), b"abc").unwrap();
        let mut v = fs_children(t.path()).unwrap();
        sort_items(&mut v, 0, true);
        assert_eq!(v[0].name, "sub");
        assert_eq!(v[1].size, Some(3));
    }
}

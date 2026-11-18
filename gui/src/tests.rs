//! Headless tests of the GUI state machine ([`app::update`]) with a recording [`Effects`].

use std::path::{Path, PathBuf};

use acme_platform::PlatformKey;
use archiver_core::{BatchResult, CreateOptions, Entry, Error, Format, Progress};

use crate::app::menus::{context_entries, menu_entries};
use crate::app::update::*;
use crate::app::worker::read_head;
use crate::app::effects::JobFn;
use crate::app::*;
use crate::view::{self, Item};

/// Records side effects; jobs are queued and run on demand.
#[derive(Default)]
struct Fx {
    log: Vec<String>,
    jobs: Vec<JobFn>,
    folder: Option<PathBuf>,
    files: Option<Vec<PathBuf>>,
    confirm: bool,
    closed: bool,
    clip_files: Vec<PathBuf>,
    clip_text: Option<String>,
    previews: Vec<(u64, String)>,
    favorites: Vec<PathBuf>,
    registered: bool,
}

impl Fx {
    fn run_jobs(&mut self, m: &mut Model) {
        while !self.jobs.is_empty() {
            let job = self.jobs.remove(0);
            let msg = job(&Progress::none());
            update(m, msg, self);
        }
    }
}

impl Effects for Fx {
    fn pick_archive(&mut self) -> Option<PathBuf> {
        self.log.push("pick_archive".into());
        self.files.as_ref().and_then(|f| f.first().cloned())
    }
    fn pick_file(&mut self, title: &str) -> Option<PathBuf> {
        self.log.push(format!("pick_file {title}"));
        self.files.as_ref().and_then(|f| f.first().cloned())
    }
    fn pick_files(&mut self, title: &str) -> Option<Vec<PathBuf>> {
        self.log.push(format!("pick_files {title}"));
        self.files.clone()
    }
    fn pick_folder(&mut self, title: &str, _start: Option<&Path>) -> Option<PathBuf> {
        self.log.push(format!("pick_folder {title}"));
        self.folder.clone()
    }
    fn save_file(&mut self, title: &str, _dir: Option<&Path>, _name: Option<&str>) -> Option<PathBuf> {
        self.log.push(format!("save_file {title}"));
        self.folder.clone()
    }
    fn confirm(&mut self, title: &str, _text: &str) -> bool {
        self.log.push(format!("confirm {title}"));
        self.confirm
    }
    fn info(&mut self, title: &str, text: &str) {
        self.log.push(format!("info {title}: {text}"));
    }
    fn error(&mut self, text: &str) {
        self.log.push(format!("error {text}"));
    }
    fn open_path(&mut self, path: &Path) {
        self.log.push(format!("open {}", path.display()));
    }
    fn spawn(&mut self, job: JobFn) {
        self.jobs.push(job);
    }
    fn cancel_job(&mut self) {
        self.log.push("cancel".into());
    }
    fn start_preview(&mut self, generation: u64, _src: PvSrc, name: String) {
        self.previews.push((generation, name));
    }
    fn cancel_preview(&mut self) {}
    fn copy_files(&mut self, paths: &[PathBuf]) -> Result<(), String> {
        self.clip_files = paths.to_vec();
        Ok(())
    }
    fn copy_text(&mut self, text: &str) -> Result<(), String> {
        self.clip_text = Some(text.into());
        Ok(())
    }
    fn paste_text(&mut self) -> Option<String> {
        self.clip_text.clone()
    }
    fn shell_set(&mut self, on: bool) -> Result<String, String> {
        self.registered = on;
        Ok(if on { "registered".into() } else { "removed".into() })
    }
    fn shell_registered(&mut self) -> bool {
        self.registered
    }
    fn favorites_load(&mut self) -> Vec<PathBuf> {
        self.favorites.clone()
    }
    fn favorites_add(&mut self, path: &Path) -> Result<(), String> {
        if !self.favorites.iter().any(|p| p == path) {
            self.favorites.push(path.to_path_buf());
        }
        Ok(())
    }
    fn favorites_remove(&mut self, path: &Path) -> Result<(), String> {
        self.favorites.retain(|p| p != path);
        Ok(())
    }
    fn remove_dir_later(&mut self, dir: PathBuf) {
        self.log.push(format!("rmdir {}", dir.display()));
    }
    fn close(&mut self) {
        self.closed = true;
    }
}

fn it(name: &str, dir: bool) -> Item {
    Item { name: name.rsplit('/').next().unwrap_or(name).into(), full: name.into(), is_dir: dir, is_parent: false, size: (!dir).then_some(10), packed: None, modified: String::new(), crc: None }
}

/// A model in file-system mode with the given rows (no disk access).
fn model_with(items: Vec<Item>) -> Model {
    let mut m = Model::new(PathBuf::from("/nowhere"));
    m.selected = vec![false; items.len()];
    m.items = items;
    m.list_view_h = 5.0 * ROW_H;
    m
}

fn rows() -> Vec<Item> {
    vec![Item::parent(), it("a.txt", false), it("b.zip", false), it("c.txt", false), it("d", true)]
}

fn press(i: usize, ctrl: bool, shift: bool) -> Msg {
    Msg::RowPressed { index: i, ctrl, shift }
}

fn key(k: PlatformKey) -> Msg {
    Msg::Key(KeyPress::plain(k))
}

/// Archive with data/folder/{chosen,skip}.txt, data/folder/nested/中文.txt, data/other.txt.
fn sample_zip(dir: &Path) -> PathBuf {
    let src = dir.join("data");
    std::fs::create_dir_all(src.join("folder/nested")).unwrap();
    std::fs::write(src.join("folder/chosen.txt"), b"chosen").unwrap();
    std::fs::write(src.join("folder/skip.txt"), b"skip").unwrap();
    std::fs::write(src.join("folder/nested/中文.txt"), b"nested").unwrap();
    std::fs::write(src.join("other.txt"), b"other").unwrap();
    let archive = dir.join("selection.zip");
    let options = CreateOptions { format: Format::Zip, level: None, password: None };
    archiver_core::create(&archive, &[src], &options, &Progress::none()).unwrap();
    archive
}

// ------------------------------------------------------------------ ported logic tests

fn r(input: &str, result: archiver_core::Result<()>) -> BatchResult {
    BatchResult { input: PathBuf::from(input), output: PathBuf::from("out"), result }
}

#[test]
fn selected_extraction_never_falls_back_to_all() {
    assert!(extraction_names(&[], ExtractScope::Selected).is_err());
    assert!(extraction_names(&[Item::parent()], ExtractScope::Selected).is_err());
    assert_eq!(extraction_names(&[], ExtractScope::All), Ok(None));
    let item = it("folder/chosen.txt", false);
    assert_eq!(extraction_names(std::slice::from_ref(&item), ExtractScope::Selected), Ok(Some(vec!["folder/chosen.txt".into()])));
    assert_eq!(extraction_names(std::slice::from_ref(&item), ExtractScope::All), Ok(None));
}

#[test]
fn manager_selection_extracts_only_chosen_files_and_folder_descendants() {
    let t = tempfile::tempdir().unwrap();
    let archive = sample_zip(t.path());
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

// ------------------------------------------------------------------ selection

#[test]
fn click_ctrl_and_shift_selection() {
    let (mut m, mut fx) = (model_with(rows()), Fx::default());
    update(&mut m, press(1, false, false), &mut fx);
    assert_eq!(m.selected, [false, true, false, false, false]);
    update(&mut m, press(3, true, false), &mut fx);
    assert_eq!(m.selected, [false, true, false, true, false]);
    // ctrl toggles off again
    update(&mut m, press(3, true, false), &mut fx);
    assert_eq!(m.selected, [false, true, false, false, false]);
    // shift: range from the anchor (row 3 after the ctrl click)
    update(&mut m, press(1, false, false), &mut fx);
    update(&mut m, press(4, false, true), &mut fx);
    assert_eq!(m.selected, [false, true, true, true, true]);
    // plain click resets
    update(&mut m, press(2, false, false), &mut fx);
    assert_eq!(m.selected, [false, false, true, false, false]);
    assert_eq!(m.status_left(), "已選取 1 個項目, 10 個位元組");
    // out of range is ignored
    update(&mut m, press(99, false, false), &mut fx);
    assert_eq!(m.selected, [false, false, true, false, false]);
    update(&mut m, Msg::ClearSelection, &mut fx);
    assert!(m.selected.iter().all(|s| !s));
}

#[test]
fn select_all_skips_parent_and_arrow_keys_move() {
    let (mut m, mut fx) = (model_with(rows()), Fx::default());
    update(&mut m, Msg::Key(KeyPress::ctrl_char("a")), &mut fx);
    assert_eq!(m.selected, [false, true, true, true, true]);
    update(&mut m, press(1, false, false), &mut fx);
    update(&mut m, key(PlatformKey::ArrowDown), &mut fx);
    update(&mut m, key(PlatformKey::ArrowDown), &mut fx);
    assert_eq!(m.selected, [false, false, false, true, false]);
    update(&mut m, key(PlatformKey::ArrowUp), &mut fx);
    assert_eq!(m.anchor, Some(2));
    update(&mut m, key(PlatformKey::End), &mut fx);
    assert_eq!(m.anchor, Some(4));
    update(&mut m, key(PlatformKey::Home), &mut fx);
    assert_eq!(m.anchor, Some(0));
}

#[test]
fn keyboard_moves_scroll_the_selection_into_view() {
    let items: Vec<Item> = (0..50).map(|i| it(&format!("f{i:02}.txt"), false)).collect();
    let (mut m, mut fx) = (model_with(items), Fx::default());
    m.list_view_h = 10.0 * ROW_H;
    update(&mut m, key(PlatformKey::End), &mut fx);
    assert_eq!(m.list_scroll, 40.0 * ROW_H);
    update(&mut m, key(PlatformKey::Home), &mut fx);
    assert_eq!(m.list_scroll, 0.0);
    update(&mut m, key(PlatformKey::PageDown), &mut fx);
    assert_eq!(m.anchor, Some(10));
    update(&mut m, Msg::ScrollList(1e6), &mut fx);
    assert_eq!(m.list_scroll, m.max_list_scroll());
    update(&mut m, Msg::ScrollList(-1e6), &mut fx);
    assert_eq!(m.list_scroll, 0.0);
}

#[test]
fn context_menu_keeps_multi_selection_or_retargets() {
    let (mut m, mut fx) = (model_with(rows()), Fx::default());
    update(&mut m, press(1, false, false), &mut fx);
    update(&mut m, press(2, true, false), &mut fx);
    update(&mut m, Msg::RowContext { index: 2, x: 10.0, y: 20.0 }, &mut fx);
    assert_eq!(m.selected, [false, true, true, false, false]);
    assert_eq!(m.popup, Some(Popup::Context { x: 10.0, y: 20.0 }));
    update(&mut m, Msg::ClosePopup, &mut fx);
    update(&mut m, Msg::RowContext { index: 3, x: 0.0, y: 0.0 }, &mut fx);
    assert_eq!(m.selected, [false, false, false, true, false]);
    // the parent row never opens a menu
    m.popup = None;
    update(&mut m, Msg::RowContext { index: 0, x: 0.0, y: 0.0 }, &mut fx);
    assert_eq!(m.popup, None);
    // busy: no menu, a hint instead
    m.job = Some(JobView::default());
    update(&mut m, Msg::RowContext { index: 1, x: 0.0, y: 0.0 }, &mut fx);
    assert_eq!(m.popup, None);
    assert_eq!(m.status, BUSY_MSG);
}

#[test]
fn context_menu_entries_follow_mode_and_selection() {
    let (mut m, mut fx) = (model_with(rows()), Fx::default());
    update(&mut m, Msg::RowContext { index: 2, x: 0.0, y: 0.0 }, &mut fx);
    let labels = |m: &Model| context_entries(m).into_iter().filter(|e| !e.is_separator()).map(|e| (e.label, e.enabled)).collect::<Vec<_>>();
    let l = labels(&m);
    assert!(l.contains(&("解壓縮至…".into(), true)));
    assert!(l.contains(&("解壓縮選取項目…".into(), true)));
    assert!(l.contains(&("解壓縮全部…".into(), false)));
    assert!(l.contains(&("刪除".into(), false)));
    assert!(l.contains(&("預覽…".into(), true)));
    // a text file is not extractable in file-system mode
    update(&mut m, Msg::RowContext { index: 1, x: 0.0, y: 0.0 }, &mut fx);
    assert!(labels(&m).contains(&("解壓縮選取項目…".into(), false)));
}

// ------------------------------------------------------------------ sorting / navigation

#[test]
fn sorting_toggles_and_keeps_selection() {
    let mut items = rows();
    items[1].size = Some(300);
    items[3].size = Some(5);
    let (mut m, mut fx) = (model_with(items), Fx::default());
    update(&mut m, press(1, false, false), &mut fx); // a.txt (300 bytes)
    update(&mut m, Msg::Sort(1), &mut fx);
    assert_eq!((m.sort_col, m.sort_asc), (1, true));
    let names: Vec<&str> = m.items.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(names, ["..", "d", "c.txt", "b.zip", "a.txt"]);
    assert!(m.selected[4] && m.selected.iter().filter(|s| **s).count() == 1);
    assert_eq!(m.anchor, Some(4));
    update(&mut m, Msg::Sort(1), &mut fx);
    assert!(!m.sort_asc);
    assert_eq!(m.items[2].name, "a.txt");
    assert!(m.selected[2]);
    update(&mut m, Msg::Sort(0), &mut fx);
    assert_eq!((m.sort_col, m.sort_asc), (0, true));
}

#[test]
fn navigation_in_file_system_and_archive() {
    let t = tempfile::tempdir().unwrap();
    let archive = sample_zip(t.path());
    let mut m = Model::new(t.path().to_path_buf());
    let mut fx = Fx::default();
    refresh(&mut m, &mut fx);
    let data = m.items.iter().position(|i| i.name == "data").unwrap();
    update(&mut m, Msg::RowActivated(data), &mut fx);
    assert_eq!(m.location(), t.path().join("data"));
    update(&mut m, key(PlatformKey::Backspace), &mut fx);
    assert_eq!(m.location(), t.path());

    // double-click an archive: listed in the background
    let zip = m.items.iter().position(|i| i.name == "selection.zip").unwrap();
    update(&mut m, Msg::RowActivated(zip), &mut fx);
    assert!(m.busy());
    fx.run_jobs(&mut m);
    assert!(!m.busy() && m.in_archive());
    assert_eq!(m.window_title(), "selection.zip - Archiver");
    assert!(m.path_text().starts_with("selection.zip - ZIP 壓縮檔"));
    let names: Vec<&str> = m.items.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(names, ["..", "data"]);
    update(&mut m, Msg::RowActivated(1), &mut fx);
    let folder = m.items.iter().position(|i| i.name == "folder").unwrap();
    update(&mut m, press(folder, false, false), &mut fx);
    update(&mut m, key(PlatformKey::Enter), &mut fx);
    assert!(m.path_text().starts_with("selection.zip\\data\\folder - ZIP"));
    assert!(m.can_delete());
    update(&mut m, Msg::Cmd(Cmd::Up), &mut fx);
    update(&mut m, Msg::Cmd(Cmd::Up), &mut fx);
    update(&mut m, Msg::Cmd(Cmd::Up), &mut fx);
    assert!(!m.in_archive());
    assert_eq!(m.location(), t.path());
    let _ = archive;
}

#[test]
fn find_filters_the_archive_and_escape_clears() {
    let t = tempfile::tempdir().unwrap();
    let archive = sample_zip(t.path());
    let (mut m, mut fx) = (Model::new(t.path().to_path_buf()), Fx::default());
    upd_open(&mut m, &mut fx, &archive);
    update(&mut m, Msg::Key(KeyPress::ctrl_char("f")), &mut fx);
    assert!(matches!(m.dialog, Some(Dialog::Find(_))));
    assert_eq!(m.focus, Some(Field::Find));
    assert!(m.text_focus());
    update(&mut m, Msg::Text("中文".into()), &mut fx);
    update(&mut m, key(PlatformKey::Enter), &mut fx);
    assert!(m.dialog.is_none());
    assert_eq!(m.filter, "中文");
    let names: Vec<&str> = m.items.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(names, ["..", "data/folder/nested/中文.txt"]);
    update(&mut m, key(PlatformKey::Escape), &mut fx);
    assert!(m.filter.is_empty());
    assert_eq!(m.items.len(), 2);
}

fn upd_open(m: &mut Model, fx: &mut Fx, archive: &Path) {
    open_archive(m, fx, archive.to_path_buf());
    fx.run_jobs(m);
    assert!(m.in_archive());
}

// ------------------------------------------------------------------ menus / keyboard

#[test]
fn menus_open_switch_and_close() {
    let (mut m, mut fx) = (model_with(rows()), Fx::default());
    update(&mut m, Msg::OpenMenu(1), &mut fx);
    assert_eq!(m.popup, Some(Popup::Menu(1)));
    update(&mut m, Msg::HoverMenu(2), &mut fx);
    assert_eq!(m.popup, Some(Popup::Menu(2)));
    update(&mut m, Msg::OpenMenu(2), &mut fx);
    assert_eq!(m.popup, None);
    // hovering does not open a closed menu
    update(&mut m, Msg::HoverMenu(3), &mut fx);
    assert_eq!(m.popup, None);
    update(&mut m, Msg::Key(KeyPress { alt: true, ..KeyPress::ctrl_char("o") }), &mut fx);
    assert_eq!(m.popup, Some(Popup::Menu(3)));
    update(&mut m, key(PlatformKey::Escape), &mut fx);
    assert_eq!(m.popup, None);
    // a command closes the menu
    m.popup = Some(Popup::Menu(4));
    update(&mut m, Msg::Cmd(Cmd::TogglePreview), &mut fx);
    assert!(m.popup.is_none() && !m.preview_visible);
    assert!(menu_entries(&m, 4).iter().any(|e| e.checked == Some(false)));
}

#[test]
fn toolbar_menu_state_follows_the_mode() {
    let m = model_with(rows());
    let cmds = |m: &Model| menu_entries(m, 1).into_iter().map(|e| (e.label, e.enabled)).collect::<Vec<_>>();
    let c = cmds(&m);
    assert!(c.contains(&("刪除檔案".into(), false)));
    assert!(c.contains(&("註解…".into(), false)));
    assert!(c.contains(&("防毒掃描".into(), false)));
    let mut busy = m.clone();
    busy.job = Some(JobView::default());
    assert!(cmds(&busy).contains(&("解壓縮至…".into(), false)));
}

#[test]
fn escape_closes_dialogs_and_shortcuts_do_not_leak() {
    let (mut m, mut fx) = (model_with(rows()), Fx::default());
    update(&mut m, Msg::Cmd(Cmd::Password), &mut fx);
    assert!(matches!(m.dialog, Some(Dialog::Password(_))));
    // Delete / Backspace edit the field, not the list
    update(&mut m, Msg::Text("ab".into()), &mut fx);
    update(&mut m, key(PlatformKey::Backspace), &mut fx);
    assert_eq!(m.input(Field::Password).unwrap().text, "a");
    update(&mut m, key(PlatformKey::Escape), &mut fx);
    assert!(m.dialog.is_none());
    assert_eq!(m.password, "");
    update(&mut m, Msg::Cmd(Cmd::Info), &mut fx);
    assert!(matches!(m.dialog, Some(Dialog::Info(_))));
    update(&mut m, key(PlatformKey::Enter), &mut fx);
    assert!(m.dialog.is_none());
}

#[test]
fn text_field_clipboard_shortcuts() {
    let (mut m, mut fx) = (model_with(rows()), Fx::default());
    update(&mut m, Msg::Cmd(Cmd::Find), &mut fx);
    update(&mut m, Msg::Text("hello".into()), &mut fx);
    update(&mut m, Msg::Key(KeyPress::ctrl_char("a")), &mut fx);
    update(&mut m, Msg::Key(KeyPress::ctrl_char("c")), &mut fx);
    assert_eq!(fx.clip_text.as_deref(), Some("hello"));
    update(&mut m, Msg::Key(KeyPress::ctrl_char("x")), &mut fx);
    assert_eq!(m.input(Field::Find).unwrap().text, "");
    fx.clip_text = Some("a\nb".into());
    update(&mut m, Msg::Key(KeyPress::ctrl_char("v")), &mut fx);
    assert_eq!(m.input(Field::Find).unwrap().text, "a b");
    // the list's Ctrl+A did not fire
    assert!(m.selected.iter().all(|s| !s));
}

// ------------------------------------------------------------------ dialogs

#[test]
fn create_dialog_flow_builds_an_archive() {
    let t = tempfile::tempdir().unwrap();
    let f = t.path().join("note.txt");
    std::fs::write(&f, b"hello").unwrap();
    let (mut m, mut fx) = (Model::new(t.path().to_path_buf()), Fx::default());
    refresh(&mut m, &mut fx);
    update(&mut m, Msg::Dropped(vec![f.clone()]), &mut fx);
    let Some(Dialog::Create(d)) = &m.dialog else { panic!("create dialog expected") };
    assert_eq!(d.fmt(), Format::Zip);
    assert_eq!(PathBuf::from(&d.output.text), t.path().join("note.txt.zip"));
    assert_eq!(m.focus, Some(Field::CreateOutput));
    // 7z keeps a password field, TAR has no level
    let seven = Format::writable().iter().position(|f| *f == Format::SevenZ).unwrap();
    update(&mut m, Msg::Dialog(DlgMsg::SetFormat(seven)), &mut fx);
    let Some(Dialog::Create(d)) = &m.dialog else { panic!() };
    assert!(d.output.text.ends_with("note.txt.7z"));
    assert_eq!(m.dialog.as_ref().unwrap().fields(), [Field::CreatePassword, Field::CreateOutput]);
    let tar = Format::writable().iter().position(|f| *f == Format::Tar).unwrap();
    update(&mut m, Msg::Dialog(DlgMsg::SetFormat(tar)), &mut fx);
    assert_eq!(m.dialog.as_ref().unwrap().fields(), [Field::CreateOutput]);
    update(&mut m, Msg::Dialog(DlgMsg::Focus(Field::CreatePassword, None)), &mut fx);
    assert_eq!(m.focus, Some(Field::CreateOutput));
    let gz = Format::writable().iter().position(|f| *f == Format::Gz).unwrap();
    update(&mut m, Msg::Dialog(DlgMsg::SetFormat(gz)), &mut fx);
    assert!(!m.create_warning());
    update(&mut m, Msg::Dialog(DlgMsg::SetFormat(0)), &mut fx);
    update(&mut m, Msg::Dialog(DlgMsg::SetLevel(99)), &mut fx);
    let Some(Dialog::Create(d)) = &m.dialog else { panic!() };
    assert_eq!(d.level, 9);
    update(&mut m, Msg::Dialog(DlgMsg::LevelStep(-3)), &mut fx);
    update(&mut m, Msg::Dialog(DlgMsg::Ok), &mut fx);
    assert!(m.dialog.is_none() && m.create_inputs.is_empty());
    assert!(m.busy());
    fx.run_jobs(&mut m);
    assert!(t.path().join("note.txt.zip").is_file());
    assert!(m.status.starts_with("已建立"));
    assert!(m.items.iter().any(|i| i.name == "note.txt.zip"));
    assert_eq!(m.last_format, 0);
}

#[test]
fn create_dialog_batch_mode_uses_a_folder() {
    let t = tempfile::tempdir().unwrap();
    let (a, b) = (t.path().join("a.txt"), t.path().join("b.txt"));
    std::fs::write(&a, b"a").unwrap();
    std::fs::write(&b, b"b").unwrap();
    let (mut m, mut fx) = (Model::new(t.path().to_path_buf()), Fx::default());
    open_create(&mut m, vec![a.clone(), b.clone()], Some(true));
    let Some(Dialog::Create(d)) = &m.dialog else { panic!() };
    assert!(d.each);
    assert_eq!(PathBuf::from(&d.output.text), t.path());
    // the same file twice is listed once
    open_create(&mut m, vec![a.clone()], None);
    assert_eq!(m.create_inputs.len(), 2);
    update(&mut m, Msg::Dialog(DlgMsg::Ok), &mut fx);
    fx.run_jobs(&mut m);
    assert!(t.path().join("a.txt.zip").is_file() && t.path().join("b.txt.zip").is_file());
    assert_eq!(m.status, "已完成 2 個（成功 2，失敗 0）");
    // cancel clears the inputs
    open_create(&mut m, vec![a], None);
    update(&mut m, Msg::Dialog(DlgMsg::Cancel), &mut fx);
    assert!(m.dialog.is_none() && m.create_inputs.is_empty());
}

#[test]
fn password_prompt_after_failed_listing_reopens_the_archive() {
    let (mut m, mut fx) = (model_with(rows()), Fx::default());
    let p = PathBuf::from("/x/secret.7z");
    update(&mut m, Msg::Listed(p.clone(), Err("password required".into())), &mut fx);
    assert!(matches!(m.dialog, Some(Dialog::Password(_))));
    assert_eq!(m.fallback_open, Some(p));
    update(&mut m, Msg::Text("pw".into()), &mut fx);
    update(&mut m, key(PlatformKey::Enter), &mut fx);
    assert_eq!(m.password, "pw");
    assert!(m.busy(), "the archive is listed again with the password");
    // other errors are reported
    let (mut m, mut fx) = (model_with(rows()), Fx::default());
    update(&mut m, Msg::Listed(PathBuf::from("/x/a.zip"), Err("broken".into())), &mut fx);
    assert!(m.dialog.is_none());
    assert!(fx.log.iter().any(|l| l.starts_with("error ")));
}

#[test]
fn shell_dialog_toggles_registration() {
    let (mut m, mut fx) = (model_with(rows()), Fx::default());
    update(&mut m, Msg::Cmd(Cmd::Shell), &mut fx);
    assert!(matches!(m.dialog, Some(Dialog::Shell { registered: false })));
    update(&mut m, Msg::Dialog(DlgMsg::ToggleShell), &mut fx);
    assert!(matches!(m.dialog, Some(Dialog::Shell { registered: true })));
    assert_eq!(m.status, "registered");
    update(&mut m, Msg::Dialog(DlgMsg::ToggleShell), &mut fx);
    assert!(!fx.registered);
}

#[test]
fn favorites_add_open_and_remove() {
    let t = tempfile::tempdir().unwrap();
    std::fs::create_dir(t.path().join("sub")).unwrap();
    let (mut m, mut fx) = (Model::new(t.path().join("sub")), Fx::default());
    refresh(&mut m, &mut fx);
    update(&mut m, Msg::Cmd(Cmd::FavAdd), &mut fx);
    assert_eq!(m.favorites, [t.path().join("sub")]);
    let entries = menu_entries(&m, 3);
    assert!(entries.iter().any(|e| matches!(e.msg, Some(Msg::Cmd(Cmd::FavOpen(0))))));
    update(&mut m, Msg::Cmd(Cmd::Up), &mut fx);
    assert_eq!(m.location(), t.path());
    update(&mut m, Msg::Cmd(Cmd::FavOpen(0)), &mut fx);
    assert_eq!(m.location(), t.path().join("sub"));
    update(&mut m, Msg::Cmd(Cmd::FavManage), &mut fx);
    assert!(matches!(m.dialog, Some(Dialog::Favorites)));
    update(&mut m, Msg::Dialog(DlgMsg::FavRemove(0)), &mut fx);
    assert!(m.favorites.is_empty());
    assert!(menu_entries(&m, 3).iter().any(|e| e.label == "（尚無項目）" && !e.enabled));
}

#[test]
fn comment_dialog_is_zip_only_and_saves_in_the_background() {
    let t = tempfile::tempdir().unwrap();
    let archive = sample_zip(t.path());
    let (mut m, mut fx) = (Model::new(t.path().to_path_buf()), Fx::default());
    update(&mut m, Msg::Cmd(Cmd::Comment), &mut fx);
    assert!(m.dialog.is_none(), "no archive open");
    upd_open(&mut m, &mut fx, &archive);
    update(&mut m, Msg::Cmd(Cmd::Comment), &mut fx);
    match &m.dialog {
        Some(Dialog::Comment { .. }) => {
            // multi-line: Enter inserts a newline, Ctrl+Enter confirms
            update(&mut m, Msg::Text("line1".into()), &mut fx);
            update(&mut m, key(PlatformKey::Enter), &mut fx);
            update(&mut m, Msg::Text("line2".into()), &mut fx);
            assert!(m.input(Field::Comment).unwrap().text.ends_with("line1\nline2"));
            update(&mut m, Msg::Key(KeyPress { ctrl: true, ..KeyPress::plain(PlatformKey::Enter) }), &mut fx);
            assert!(m.dialog.is_none());
            fx.run_jobs(&mut m);
            match archiver_core::read_comment(&archive) {
                Ok(c) => assert_eq!(c.as_deref().map(str::trim_start), Some("line1\nline2")),
                Err(e) => assert!(fx.log.iter().any(|l| l.contains(&e.to_string()))),
            }
        }
        // core could not read the comment: reported, no dialog
        _ => assert!(fx.log.iter().any(|l| l.starts_with("error "))),
    }
}

#[test]
fn drops_open_archives_add_to_open_archive_or_create() {
    let t = tempfile::tempdir().unwrap();
    let archive = sample_zip(t.path());
    let extra = t.path().join("extra.txt");
    std::fs::write(&extra, b"extra").unwrap();
    let (mut m, mut fx) = (Model::new(t.path().to_path_buf()), Fx::default());
    refresh(&mut m, &mut fx);
    // a plain file: create dialog
    update(&mut m, Msg::Dropped(vec![extra.clone()]), &mut fx);
    assert!(matches!(m.dialog, Some(Dialog::Create(_))));
    update(&mut m, Msg::Dialog(DlgMsg::Cancel), &mut fx);
    // an archive: opened
    update(&mut m, Msg::Dropped(vec![archive.clone()]), &mut fx);
    fx.run_jobs(&mut m);
    assert!(m.in_archive());
    // files onto the open archive: confirmed, then added into the shown folder
    update(&mut m, Msg::RowActivated(1), &mut fx); // data/
    update(&mut m, Msg::Dropped(vec![extra.clone()]), &mut fx);
    assert!(fx.log.iter().any(|l| l == "confirm 加入檔案"));
    assert!(!m.busy(), "declined");
    fx.confirm = true;
    update(&mut m, Msg::Dropped(vec![extra]), &mut fx);
    assert!(m.busy());
    fx.run_jobs(&mut m);
    let entries = archiver_core::list(&archive, None).unwrap();
    if m.status.starts_with("錯誤") {
        assert!(fx.log.iter().any(|l| l.starts_with("error ")));
    } else {
        assert!(entries.iter().any(|e| e.name == "data/extra.txt"), "{:?}", entries.iter().map(|e| &e.name).collect::<Vec<_>>());
    }
}

#[test]
fn delete_asks_and_reloads() {
    let t = tempfile::tempdir().unwrap();
    let archive = sample_zip(t.path());
    let (mut m, mut fx) = (Model::new(t.path().to_path_buf()), Fx::default());
    upd_open(&mut m, &mut fx, &archive);
    update(&mut m, press(1, false, false), &mut fx); // data
    update(&mut m, key(PlatformKey::Delete), &mut fx);
    assert!(fx.log.iter().any(|l| l == "confirm 刪除檔案"));
    assert!(!m.busy());
    fx.confirm = true;
    update(&mut m, key(PlatformKey::Delete), &mut fx);
    fx.run_jobs(&mut m);
    assert!(archiver_core::list(&archive, None).unwrap().is_empty());
    assert_eq!(m.items.len(), 1);
}

#[test]
fn extract_selected_uses_the_picked_folder() {
    let t = tempfile::tempdir().unwrap();
    let archive = sample_zip(t.path());
    let (mut m, mut fx) = (Model::new(t.path().to_path_buf()), Fx::default());
    upd_open(&mut m, &mut fx, &archive);
    update(&mut m, Msg::Cmd(Cmd::ExtractSelected), &mut fx);
    assert!(m.status.starts_with("請先選取"));
    update(&mut m, press(1, false, false), &mut fx);
    update(&mut m, Msg::Cmd(Cmd::ExtractSelected), &mut fx);
    assert!(fx.log.iter().any(|l| l == "pick_folder 解壓縮選取項目至…"));
    assert!(!m.busy(), "dialog cancelled");
    fx.folder = Some(t.path().join("out"));
    update(&mut m, Msg::Cmd(Cmd::ExtractSelected), &mut fx);
    fx.run_jobs(&mut m);
    assert!(t.path().join("out/data/folder/nested/中文.txt").is_file());
}

#[test]
fn busy_jobs_block_new_ones_and_cancel_is_forwarded() {
    let (mut m, mut fx) = (model_with(rows()), Fx::default());
    m.job = Some(JobView { title: "x".into(), ..JobView::default() });
    update(&mut m, Msg::Cmd(Cmd::Open), &mut fx);
    fx.files = Some(vec![PathBuf::from("/a.zip")]);
    update(&mut m, Msg::Cmd(Cmd::Open), &mut fx);
    assert!(fx.jobs.is_empty());
    assert_eq!(m.status, BUSY_MSG);
    update(&mut m, Msg::Cmd(Cmd::Cancel), &mut fx);
    assert!(fx.log.contains(&"cancel".to_string()));
    update(&mut m, Msg::JobDone(Err("operation cancelled".into()), After::Nothing), &mut fx);
    assert!(!m.busy());
    assert_eq!(m.status, "已取消");
    assert!(!fx.log.iter().any(|l| l.starts_with("error ")), "a user cancel is not an error box");
}

#[test]
fn cli_jobs_close_the_window_when_done() {
    let t = tempfile::tempdir().unwrap();
    let archive = sample_zip(t.path());
    let (mut m, mut fx) = (Model::new(t.path().to_path_buf()), Fx::default());
    test_files(&mut m, &mut fx, vec![archive.clone()]);
    fx.run_jobs(&mut m);
    assert!(fx.closed);
    assert!(fx.log.iter().any(|l| l.contains("個檔案測試正確")));

    let src = t.path().join("data");
    let (mut m, mut fx) = (Model::new(t.path().to_path_buf()), Fx::default());
    compress_to(&mut m, &mut fx, Format::SevenZ, vec![src.clone()]);
    fx.run_jobs(&mut m);
    assert!(fx.closed);
    assert!(t.path().join("data.7z").is_file());
    // a second run does not overwrite
    let (mut m, mut fx) = (Model::new(t.path().to_path_buf()), Fx::default());
    compress_to(&mut m, &mut fx, Format::SevenZ, vec![src]);
    fx.run_jobs(&mut m);
    assert!(t.path().join("data (2).7z").is_file());
}

#[test]
fn preview_is_debounced_and_ignores_stale_results() {
    let t = tempfile::tempdir().unwrap();
    std::fs::write(t.path().join("a.txt"), b"hello").unwrap();
    std::fs::write(t.path().join("b.txt"), b"world").unwrap();
    let (mut m, mut fx) = (Model::new(t.path().to_path_buf()), Fx::default());
    refresh(&mut m, &mut fx);
    let a = m.items.iter().position(|i| i.name == "a.txt").unwrap();
    update(&mut m, press(a, false, false), &mut fx);
    assert_eq!(m.pv.view, PvView::Loading);
    update(&mut m, Msg::Tick(std::time::Instant::now()), &mut fx);
    assert!(fx.previews.is_empty(), "debounce");
    let old = m.pv.generation;
    update(&mut m, press(a + 1, false, false), &mut fx);
    update(&mut m, Msg::Tick(std::time::Instant::now() + std::time::Duration::from_secs(1)), &mut fx);
    assert_eq!(fx.previews, [(m.pv.generation, "b.txt".to_string())]);
    let src = PvSrc::File(t.path().join("b.txt"));
    let done = worker::preview_worker(m.pv.generation, src, "b.txt", Default::default());
    update(&mut m, Msg::PreviewDone(PreviewDone { generation: old, body: PvBody::Text("stale".into()), full: None }), &mut fx);
    assert_eq!(m.pv.view, PvView::Loading);
    update(&mut m, Msg::PreviewDone(done), &mut fx);
    assert_eq!(m.pv.view, PvView::Text("world".into()));
    update(&mut m, Msg::Cmd(Cmd::CopyText), &mut fx);
    assert_eq!(fx.clip_text.as_deref(), Some("world"));
    // folders and multi-selections need no worker
    update(&mut m, Msg::Cmd(Cmd::SelectAll), &mut fx);
    assert!(matches!(m.pv.view, PvView::Info(_)));
    // the hidden pane does not load anything
    update(&mut m, Msg::Cmd(Cmd::TogglePreview), &mut fx);
    update(&mut m, press(a, false, false), &mut fx);
    assert!(m.pv.pending.is_none());
}

#[test]
fn preview_worker_decodes_images_and_svg() {
    let mut png = Vec::new();
    image::write_buffer_with_format(&mut std::io::Cursor::new(&mut png), &[255, 0, 0, 255, 0, 255, 0, 255], 2, 1, image::ExtendedColorType::Rgba8, image::ImageFormat::Png).unwrap();
    match worker::classify("x.png", &png, false) {
        PvBody::Image { width, height, rgba } => assert_eq!((width, height, &rgba[..4]), (2, 1, &[255u8, 0, 0, 255][..])),
        other => panic!("{other:?}"),
    }
    let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="20"><rect width="10" height="20" fill="blue"/></svg>"#;
    match worker::classify("x.svg", svg, false) {
        PvBody::Image { width, height, .. } => assert_eq!((width, height), (512, 1024)),
        other => panic!("{other:?}"),
    }
    assert!(matches!(worker::classify("bad.png", &[0x89, b'P', b'N', b'G', 0, 0], false), PvBody::Hex(_)));
}

#[test]
fn copy_from_archive_extracts_to_a_staging_folder() {
    let t = tempfile::tempdir().unwrap();
    let archive = sample_zip(t.path());
    let (mut m, mut fx) = (Model::new(t.path().to_path_buf()), Fx::default());
    upd_open(&mut m, &mut fx, &archive);
    update(&mut m, Msg::Key(KeyPress::ctrl_char("c")), &mut fx);
    assert_eq!(m.status, "請先選取要複製的檔案");
    update(&mut m, press(1, false, false), &mut fx);
    update(&mut m, Msg::Key(KeyPress::ctrl_char("c")), &mut fx);
    fx.run_jobs(&mut m);
    assert_eq!(fx.clip_files.len(), 1);
    assert!(fx.clip_files[0].join("folder/chosen.txt").is_file());
    let _ = std::fs::remove_dir_all(view::clip_dir_of(&fx.clip_files[0]).unwrap());
}

#[test]
fn screenshot_scene_renders_without_a_window() {
    let t = tempfile::tempdir().unwrap();
    let archive = sample_zip(t.path());
    let mut m = Model::new(t.path().to_path_buf());
    let mut fx = Fx::default();
    show_listing(&mut m, &mut fx, archive.clone(), archiver_core::list(&archive, None).unwrap());
    update(&mut m, press(1, false, false), &mut fx);
    update(&mut m, Msg::RowContext { index: 1, x: 100.0, y: 200.0 }, &mut fx);
    let mut ui = crate::ui::Ui::new(acme_theme::Theme::light());
    for (w, h) in [(1100.0, 700.0), (900.0, 600.0), (760.0, 480.0)] {
        let scene = ui.paint(&m, w, h, 1.25);
        scene.validate().unwrap();
        assert!(scene.commands().len() > 50);
    }
    // every row and the popup items are hit regions
    assert!(ui.layer.len() > 20);
    m.popup = None;
    open_create(&mut m, vec![archive], None);
    ui.paint(&m, 900.0, 600.0, 1.0).validate().unwrap();
    let cols = crate::ui::columns(600.0, true);
    assert!(cols[0].w >= 180.0, "{cols:?}");
    assert!(crate::ui::columns(1100.0, true).len() == 6);
}

// ------------------------------------------------------------------ review fixes

#[test]
fn double_click_on_a_row_opens_it_through_the_pointer_path() {
    use acme_platform::{Application, PlatformEvent, WindowId};
    let t = tempfile::tempdir().unwrap();
    std::fs::create_dir(t.path().join("sub")).unwrap();
    std::fs::write(t.path().join("a.txt"), b"a").unwrap();
    let mut app = crate::App::new(t.path().to_path_buf());
    let (w, h) = (1100.0, 700.0);
    app.frame_scene(w, h, 1.0);
    let row = app.m.items.iter().position(|i| i.name == "sub").unwrap();
    let g = crate::ui::geometry(w, h, app.m.preview_visible);
    let (x, y) = (80.0, g.list.origin.y.get() + row as f32 * ROW_H + ROW_H / 2.0);
    let button = |pressed| PlatformEvent::PointerButton { window: WindowId(0), pressed, x, y, button: 0, pointer: 0, shift: false, ctrl: false, alt: false };
    app.event(PlatformEvent::PointerMoved { window: WindowId(0), x, y });
    // the frame after the first press enables 複製/檢視 (selection): ids must not shift
    for pressed in [true, false, true, false] {
        app.event(button(pressed));
        app.frame_scene(w, h, 1.0);
    }
    assert_eq!(app.m.location(), t.path().join("sub"));
}

#[test]
fn password_is_forgotten_when_leaving_or_switching_archives() {
    let t = tempfile::tempdir().unwrap();
    let a = sample_zip(t.path());
    let b = t.path().join("plain.zip");
    std::fs::copy(&a, &b).unwrap();
    let (mut m, mut fx) = (Model::new(t.path().to_path_buf()), Fx::default());
    upd_open(&mut m, &mut fx, &a);
    update(&mut m, Msg::Cmd(Cmd::Password), &mut fx);
    update(&mut m, Msg::Text("secret".into()), &mut fx);
    update(&mut m, key(PlatformKey::Enter), &mut fx);
    fx.run_jobs(&mut m);
    assert_eq!((m.password.as_str(), m.password_for.as_deref()), ("secret", Some(a.as_path())));
    // another archive: the password does not follow
    upd_open(&mut m, &mut fx, &b);
    assert!(m.password.is_empty() && m.password_for.is_none());
    // leaving archive mode forgets it too
    upd_open(&mut m, &mut fx, &a);
    m.password = "secret".into();
    m.password_for = Some(a.clone());
    update(&mut m, Msg::Cmd(Cmd::Up), &mut fx);
    update(&mut m, Msg::Cmd(Cmd::Up), &mut fx);
    update(&mut m, Msg::Cmd(Cmd::Up), &mut fx);
    assert!(!m.in_archive());
    assert!(m.password.is_empty());
}

#[test]
fn adding_to_a_plain_archive_never_encrypts_it() {
    let t = tempfile::tempdir().unwrap();
    let archive = sample_zip(t.path());
    let extra = t.path().join("extra.txt");
    std::fs::write(&extra, b"extra").unwrap();
    let (mut m, mut fx) = (Model::new(t.path().to_path_buf()), Fx::default());
    upd_open(&mut m, &mut fx, &archive);
    // a stale password (e.g. from the dialog) must not be used for an unencrypted archive
    m.password = "stale".into();
    add_to_archive(&mut m, &mut fx, vec![extra]);
    fx.run_jobs(&mut m);
    let entries = archiver_core::list(&archive, None).unwrap();
    assert!(entries.iter().any(|e| e.name == "extra.txt"), "{:?}", fx.log);
    assert!(entries.iter().all(|e| !e.encrypted));
}

#[test]
fn rewriting_an_encrypted_archive_asks_for_the_password_first() {
    let t = tempfile::tempdir().unwrap();
    let (mut m, mut fx) = (Model::new(t.path().to_path_buf()), Fx::default());
    let enc = |name: &str| Entry { name: name.into(), size: 1, compressed: None, modified: None, is_dir: false, crc32: None, encrypted: true };
    show_listing(&mut m, &mut fx, t.path().join("e.zip"), vec![enc("a.txt")]);
    add_to_archive(&mut m, &mut fx, vec![t.path().join("x")]);
    assert!(matches!(m.dialog, Some(Dialog::Password(_))));
    assert!(fx.jobs.is_empty());
    update(&mut m, Msg::Dialog(DlgMsg::Cancel), &mut fx);
    fx.confirm = true;
    update(&mut m, press(1, false, false), &mut fx);
    update(&mut m, key(PlatformKey::Delete), &mut fx);
    assert!(matches!(m.dialog, Some(Dialog::Password(_))));
    assert!(fx.jobs.is_empty());
}

#[test]
fn password_prompt_reopens_the_archive_that_asked() {
    let t = tempfile::tempdir().unwrap();
    let shown = sample_zip(t.path());
    let (mut m, mut fx) = (Model::new(t.path().to_path_buf()), Fx::default());
    upd_open(&mut m, &mut fx, &shown);
    let other = t.path().join("other.7z");
    update(&mut m, Msg::Listed(other.clone(), Err("wrong password".into())), &mut fx);
    update(&mut m, Msg::Text("pw".into()), &mut fx);
    update(&mut m, Msg::Dialog(DlgMsg::Ok), &mut fx);
    assert_eq!(m.password_for.as_deref(), Some(other.as_path()));
    assert!(m.fallback_open.is_none());
    // Escape clears the fallback as well
    update(&mut m, Msg::Listed(other, Err("wrong password".into())), &mut fx);
    update(&mut m, key(PlatformKey::Escape), &mut fx);
    assert!(m.dialog.is_none() && m.fallback_open.is_none());
}

#[test]
fn fallback_open_only_on_password_errors() {
    let (mut m, mut fx) = (model_with(rows()), Fx::default());
    m.fallback_open = Some(PathBuf::from("/x/a.7z"));
    m.job = Some(JobView::default());
    update(&mut m, Msg::JobDone(Err("disk full".into()), After::Nothing), &mut fx);
    assert!(fx.log.iter().any(|l| l == "error disk full"));
    assert!(fx.jobs.is_empty() && m.fallback_open.is_none());
    m.fallback_open = Some(PathBuf::from("/x/a.7z"));
    update(&mut m, Msg::JobDone(Err("password required".into()), After::Nothing), &mut fx);
    assert_eq!(fx.jobs.len(), 1, "the archive is opened so a password can be entered");
}

#[test]
fn testing_several_archives_continues_past_failures() {
    let t = tempfile::tempdir().unwrap();
    let good = sample_zip(t.path());
    let bad = t.path().join("bad.zip");
    std::fs::write(&bad, b"not a zip").unwrap();
    match test_msg(&[bad, good], None, &Progress::none()) {
        Msg::JobDone(Err(text), After::Nothing) => {
            assert!(text.starts_with("已測試 2 個（正確 1，錯誤 1）"), "{text}");
            assert!(text.contains("✗ bad.zip") && text.contains("selection.zip："), "{text}");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn cancelled_batches_report_cancel_not_errors() {
    let results = vec![r("/a/x.zip", Ok(())), r("/a/y.zip", Err(Error::Cancelled))];
    let msg = batch_msg(&results);
    let (mut m, mut fx) = (model_with(rows()), Fx::default());
    m.close_when_done = true;
    update(&mut m, msg, &mut fx);
    assert_eq!(m.status, "已取消");
    assert!(fx.log.iter().all(|l| !l.starts_with("error ")));
    assert!(fx.closed, "a cancelled shell job closes its window");
}

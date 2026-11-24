//! WinRAR-alignment handlers (v0.8.0): persisted settings, recent archives, overwrite policy,
//! rename (F2), select / deselect group and invert (Gray + − *), convert (Alt+Q), report
//! (Alt+G), save archive copy, and the create dialog's "test after" / "delete after" steps.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use archiver_core::{BatchResult, CreateOptions, Error, ExtractOptions, ExtractStats, Format, Overwrite, Progress};

use super::effects::Effects;
use super::model::*;
use super::msg::*;
use super::update::{batch_summary, close_dialog, is_busy, open_archive, open_dialog, rewrite_password, selection_changed, set_focus, set_status, single_password_failure, spawn};
use crate::report::ReportFormat;
use crate::settings::{self, Settings};

fn cancelled() -> Msg {
    Msg::JobDone(Err(Error::Cancelled.to_string()), After::Nothing)
}

fn is_cancelled(p: &Progress) -> bool {
    p.cancel.load(Ordering::Relaxed)
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| p.display().to_string())
}

// ------------------------------------------------------------------ settings

/// The persisted part of the model.
pub fn settings_of(m: &Model) -> Settings {
    Settings { last_format: m.last_format, preview_visible: m.preview_visible, overwrite: m.overwrite, test_after: m.test_after, recent: m.recent.clone() }
}

/// Start-up: put the loaded settings into the model.
pub fn apply_settings(m: &mut Model, s: Settings) {
    m.last_format = s.last_format.min(Format::writable().len() - 1);
    m.preview_visible = s.preview_visible;
    m.overwrite = s.overwrite;
    m.test_after = s.test_after;
    m.recent = s.recent;
}

/// Write the settings (a failure must not get in the way of the work itself).
pub fn save_settings(m: &Model, fx: &mut dyn Effects) {
    let _ = fx.settings_save(&settings_of(m));
}

pub fn set_overwrite(m: &mut Model, fx: &mut dyn Effects, o: Overwrite) {
    if m.overwrite != o {
        m.overwrite = o;
        save_settings(m, fx);
    }
    set_status(m, format!("解壓縮時：{}", o.label()));
}

pub fn extract_options(m: &Model) -> ExtractOptions {
    ExtractOptions { overwrite: m.overwrite }
}

/// "已解壓縮到 X（略過 3 個已存在的檔案）".
pub fn extracted_text(dest: &Path, s: ExtractStats) -> String {
    let base = format!("已解壓縮到 {}", dest.display());
    match (s.skipped, s.renamed) {
        (0, 0) => base,
        (k, 0) => format!("{base}（略過 {k} 個已存在的檔案）"),
        (0, r) => format!("{base}（重新命名 {r} 個已存在的檔案）"),
        (k, r) => format!("{base}（略過 {k} 個、重新命名 {r} 個已存在的檔案）"),
    }
}

// ------------------------------------------------------------------ recent archives

/// An archive was opened: move it to the front of the recent list.
pub fn remember_recent(m: &mut Model, fx: &mut dyn Effects, p: &Path) {
    let before = m.recent.clone();
    settings::push_recent(&mut m.recent, p);
    if before != m.recent {
        save_settings(m, fx);
    }
}

/// Marker error of the listing job: the archive does not exist (checked off the UI thread).
pub const NOT_FOUND: &str = "\u{0}not-found";

/// Open through the normal async listing path (no file-system call on the UI thread).
pub fn recent_open(m: &mut Model, fx: &mut dyn Effects, i: usize) {
    let Some(p) = m.recent.get(i).cloned() else { return };
    open_archive(m, fx, p);
}

/// The listing of `p` found no file: drop it from the recent list. Returns whether it was there.
pub fn forget_missing_recent(m: &mut Model, fx: &mut dyn Effects, p: &Path) -> bool {
    let Some(i) = m.recent.iter().position(|q| q == p) else { return false };
    m.recent.remove(i);
    save_settings(m, fx);
    set_status(m, format!("找不到 {}，已從最近開啟的清單中移除", p.display()));
    true
}

// ------------------------------------------------------------------ rename (F2)

/// Exactly one item is selected and it can be renamed here.
pub fn can_rename(m: &Model) -> bool {
    m.selected_items().len() == 1 && (!m.in_archive() || m.archive_writable())
}

/// Folder of an entry name, with its trailing '/' ("" at the top level).
pub fn prefix_of(entry: &str) -> &str {
    entry.rfind('/').map_or("", |i| &entry[..=i])
}

fn last_component(full: &str, fs: bool) -> String {
    if fs { Path::new(full).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default() } else { full.rsplit('/').next().unwrap_or(full).to_string() }
}

pub fn open_rename(m: &mut Model, fx: &mut dyn Effects) {
    let _ = fx;
    if is_busy(m) {
        return;
    }
    let sel = m.selected_items();
    let [it] = sel.as_slice() else { return set_status(m, "請先選取一個要重新命名的項目") };
    if m.in_archive() && !m.archive_writable() {
        return set_status(m, "此格式不支援重新命名");
    }
    let fs = !m.in_archive();
    let mut text = text_input(&last_component(&it.full, fs), false);
    text.select_all();
    open_dialog(m, Dialog::Rename { from: it.full.clone(), fs, text });
}

/// The new name is a single, usable path component.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\']) && !name.chars().any(char::is_control)
}

pub fn apply_rename(m: &mut Model, fx: &mut dyn Effects) {
    let Some(Dialog::Rename { from, fs, text }) = &m.dialog else { return };
    let (from, fs, new) = (from.clone(), *fs, text.text.trim().to_string());
    if !valid_name(&new) {
        return set_status(m, "名稱無效：不能空白、不能是「.」或「..」，也不能包含 / 或 \\");
    }
    if new == last_component(&from, fs) {
        return close_dialog(m);
    }
    if is_busy(m) {
        return;
    }
    if fs {
        let src = PathBuf::from(&from);
        let dst = src.with_file_name(&new);
        close_dialog(m);
        return spawn(m, fx, "重新命名中…", move |_| Msg::JobDone(rename_file(&src, &dst).map(|()| format!("已重新命名為 {new}")), After::ReloadFs));
    }
    let Mode::Archive { path, fmt, .. } = &m.mode else { return };
    let (arc, fmt) = (path.clone(), *fmt);
    let to = format!("{}{new}", prefix_of(&from));
    // 7z entries are recompressed (an encrypted one needs the password); ZIP / tar are copied
    let pw = if fmt == Format::SevenZ {
        match rewrite_password(m) {
            Some(pw) => pw,
            None => return,
        }
    } else {
        m.opt_pw()
    };
    close_dialog(m);
    spawn(m, fx, "重新命名中…", move |p| {
        let r = archiver_core::rename(&arc, &from, &to, pw.as_deref(), p).map(|()| format!("已重新命名為 {new}")).map_err(|e| e.to_string());
        Msg::JobDone(r, After::ReloadArchive)
    });
}

/// Rename a file or folder on disk; never replaces an existing one. A case-only change on a
/// case-insensitive file system (Windows, default macOS) is the same file and allowed; it goes
/// through a temporary name so the new case really sticks. Runs inside a job (touches the disk).
pub fn rename_file(src: &Path, dst: &Path) -> Result<(), String> {
    let err = |p: &Path, e: std::io::Error| format!("{}: {e}", p.display());
    if dst.symlink_metadata().is_ok() {
        let case_only = src.file_name().map(|n| n.to_string_lossy().to_lowercase()) == dst.file_name().map(|n| n.to_string_lossy().to_lowercase());
        let same_file = matches!((std::fs::canonicalize(src), std::fs::canonicalize(dst)), (Ok(a), Ok(b)) if a == b);
        if !(case_only && same_file) {
            return Err(format!("{} 已存在", dst.display()));
        }
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        let tmp = dst.with_file_name(format!(".{}-{nanos}.archiver-rename", std::process::id()));
        std::fs::rename(src, &tmp).map_err(|e| err(src, e))?;
        return std::fs::rename(&tmp, dst).map_err(|e| {
            let _ = std::fs::rename(&tmp, src); // put it back
            err(dst, e)
        });
    }
    std::fs::rename(src, dst).map_err(|e| err(src, e))
}

// ------------------------------------------------------------------ select group / invert

pub fn open_mask(m: &mut Model, select: bool) {
    let mut text = text_input(&m.last_mask, false);
    text.select_all();
    open_dialog(m, Dialog::SelectMask { select, text });
}

/// WinRAR treats `*.*` as "everything", also names without a dot.
fn normalize_masks(masks: &str) -> String {
    masks.split(|c: char| c == ';' || c.is_whitespace()).filter(|t| !t.is_empty()).map(|t| if t == "*.*" { "*" } else { t }).collect::<Vec<_>>().join(";")
}

pub fn apply_mask(m: &mut Model, fx: &mut dyn Effects) {
    let Some(Dialog::SelectMask { select, text }) = &m.dialog else { return };
    let (select, mask) = (*select, text.text.trim().to_string());
    close_dialog(m);
    if mask.is_empty() {
        return;
    }
    m.last_mask = mask.clone();
    let masks = normalize_masks(&mask);
    let mut changed = false;
    for (s, it) in m.selected.iter_mut().zip(&m.items) {
        if !it.is_parent && *s != select && crate::select::matches(&masks, &it.name) {
            *s = select;
            changed = true;
        }
    }
    m.status.clear();
    if changed {
        selection_changed(m, fx);
    }
}

pub fn invert_selection(m: &mut Model, fx: &mut dyn Effects) {
    for (s, it) in m.selected.iter_mut().zip(&m.items) {
        *s = !it.is_parent && !*s;
    }
    m.anchor = None;
    m.status.clear();
    selection_changed(m, fx);
}

// ------------------------------------------------------------------ convert (Alt+Q)

pub fn open_convert(m: &mut Model, fx: &mut dyn Effects) {
    if is_busy(m) {
        return;
    }
    let mut sources: Vec<PathBuf> = match &m.mode {
        Mode::Archive { path, .. } => vec![path.clone()],
        Mode::Fs(_) => m.selected_items().into_iter().filter(|i| !i.is_dir && Format::detect(Path::new(&i.name)).is_some()).map(|i| PathBuf::from(i.full)).collect(),
    };
    if sources.is_empty() {
        let Some(files) = fx.pick_files("選擇要轉換的壓縮檔") else { return };
        sources = files.into_iter().filter(|f| Format::detect(f).is_some()).collect();
        if sources.is_empty() {
            return set_status(m, "沒有可轉換的壓縮檔");
        }
    }
    let all = Format::writable();
    let format = m.last_format.min(all.len() - 1);
    let level = all[format].level_range().map_or(0, |r| r.2);
    open_dialog(m, Dialog::Convert(ConvertDlg { sources, format, level, password: text_input("", true) }));
}

/// Format popup / level controls of the create and convert dialogs.
pub fn dialog_format(m: &Model) -> Option<usize> {
    match &m.dialog {
        Some(Dialog::Create(d)) => Some(d.format),
        Some(Dialog::Convert(d)) => Some(d.format),
        _ => None,
    }
}

pub fn dialog_level_mut(m: &mut Model) -> Option<(&mut u32, Format)> {
    match &mut m.dialog {
        Some(Dialog::Create(d)) => {
            let f = d.fmt();
            Some((&mut d.level, f))
        }
        Some(Dialog::Convert(d)) => {
            let f = d.fmt();
            Some((&mut d.level, f))
        }
        _ => None,
    }
}

pub fn convert_set_format(m: &mut Model, idx: usize) {
    let Some(Dialog::Convert(d)) = &mut m.dialog else { return };
    d.format = idx.min(Format::writable().len() - 1);
    let fmt = d.fmt();
    if let Some((lo, hi, def)) = fmt.level_range()
        && !(lo..=hi).contains(&d.level)
    {
        d.level = def;
    }
    if !fmt.supports_password() && m.focus == Some(Field::ConvertPassword) {
        set_focus(m, None);
    }
}

pub fn start_convert(m: &mut Model, fx: &mut dyn Effects) {
    if is_busy(m) {
        return;
    }
    let Some(Dialog::Convert(d)) = &m.dialog else { return };
    if d.sources.is_empty() {
        return;
    }
    let fmt = d.fmt();
    let opts = CreateOptions { format: fmt, level: fmt.level_range().map(|_| d.level), password: Some(d.password.text.clone()).filter(|p| !p.is_empty() && fmt.supports_password()) };
    let sources = d.sources.clone();
    let src_pw = m.opt_pw();
    let in_archive = m.in_archive();
    close_dialog(m);
    spawn(m, fx, "轉換中…", move |p| convert_msg(&sources, &opts, src_pw.as_deref(), in_archive, p));
}

/// Convert every source next to itself (`name (2).ext` when taken); one failure does not stop the rest.
pub fn convert_msg(sources: &[PathBuf], opts: &CreateOptions, src_pw: Option<&str>, in_archive: bool, p: &Progress) -> Msg {
    let mut results = Vec::new();
    for src in sources {
        if is_cancelled(p) {
            return cancelled();
        }
        let out = unique_path(&archiver_core::convert_output(src, opts.format));
        let result = archiver_core::convert(src, &out, opts, src_pw, p);
        if matches!(result, Err(Error::Cancelled)) {
            return cancelled();
        }
        results.push(BatchResult { input: src.clone(), output: out, result });
    }
    let (text, failed) = batch_summary(&results);
    let mut text = text.replacen("已完成", "已轉換", 1);
    if let [r] = results.as_slice()
        && r.result.is_ok()
    {
        text = format!("已轉換為 {}", r.output.display());
    }
    if !in_archive {
        return Msg::JobDone(Ok(text), After::Batch { failed, retry: None });
    }
    if failed == 0 { Msg::JobDone(Ok(text), After::Message) } else { Msg::JobDone(Err(text), After::Nothing) }
}

// ------------------------------------------------------------------ report (Alt+G)

pub fn do_report(m: &mut Model, fx: &mut dyn Effects) {
    if is_busy(m) {
        return;
    }
    let (arc, entries) = match &m.mode {
        Mode::Archive { path, entries, .. } => (path.clone(), Some(entries.clone())),
        Mode::Fs(_) => match m.selected_items().as_slice() {
            [it] if !it.is_dir && Format::detect(Path::new(&it.name)).is_some() => (PathBuf::from(&it.full), None),
            _ => return set_status(m, "請先開啟壓縮檔，或在資料夾中選取一個壓縮檔"),
        },
    };
    let name = format!("{}.{}", archiver_core::archive_stem(&arc), ReportFormat::Text.extension());
    let Some(out) = fx.save_file("產生報告", arc.parent(), Some(&name)) else { return };
    if out == arc {
        return set_status(m, "報告不能覆寫壓縮檔本身");
    }
    let pw = m.opt_pw();
    spawn(m, fx, "產生報告中…", move |_| {
        let r = (|| {
            let entries = match entries {
                Some(e) => e,
                None => archiver_core::list(&arc, pw.as_deref()).map_err(|e| format!("{}: {e}", arc.display()))?,
            };
            let label = Format::detect(&arc).map(|f| f.label().split(' ').next().unwrap_or("").to_string()).unwrap_or_default();
            let text = crate::report::render(&arc, &label, &entries, ReportFormat::from_path(&out));
            std::fs::write(&out, text).map_err(|e| format!("{}: {e}", out.display()))?;
            Ok(format!("已產生報告 {}", out.display()))
        })();
        Msg::JobDone(r, After::ReloadFs)
    });
}

// ------------------------------------------------------------------ save archive copy

pub fn do_save_copy(m: &mut Model, fx: &mut dyn Effects) {
    let Mode::Archive { path, .. } = &m.mode else { return set_status(m, "請先開啟壓縮檔") };
    let src = path.clone();
    if is_busy(m) {
        return;
    }
    let name = file_name(&src);
    let Some(dst) = fx.save_file("另存壓縮檔複本", src.parent(), Some(&name)) else { return };
    if dst == src {
        return set_status(m, "複本不能與原壓縮檔相同");
    }
    spawn(m, fx, "另存複本中…", move |p| {
        let r = copy_file(&src, &dst, p).map(|()| format!("已另存複本 {}", dst.display()));
        Msg::JobDone(r, After::ReloadFs)
    });
}

/// Copy through a temporary file next to `dst` (progress, cancellation; `dst` is replaced
/// only when the copy is complete).
pub fn copy_file(src: &Path, dst: &Path, p: &Progress) -> Result<(), String> {
    use std::io::{Read, Write};
    let same = std::fs::canonicalize(src).ok().zip(std::fs::canonicalize(dst).ok()).is_some_and(|(a, b)| a == b);
    if same {
        return Err("複本不能與原壓縮檔相同".into());
    }
    let tmp = dst.with_file_name(format!(".{}.archiver-tmp", file_name(dst)));
    let r = (|| -> Result<(), String> {
        let io = |e: std::io::Error| e.to_string();
        let mut input = std::fs::File::open(src).map_err(|e| format!("{}: {e}", src.display()))?;
        let total = input.metadata().map(|md| md.len()).unwrap_or(0);
        let mut output = std::fs::File::create(&tmp).map_err(|e| format!("{}: {e}", dst.display()))?;
        let mut buf = vec![0u8; 1 << 20];
        let (mut done, name) = (0u64, file_name(src));
        loop {
            if is_cancelled(p) {
                return Err(Error::Cancelled.to_string());
            }
            let n = input.read(&mut buf).map_err(io)?;
            if n == 0 {
                break;
            }
            output.write_all(&buf[..n]).map_err(io)?;
            done += n as u64;
            (p.on)(done, total, &name);
        }
        output.sync_all().map_err(io)?;
        drop(output);
        std::fs::rename(&tmp, dst).map_err(|e| format!("{}: {e}", dst.display()))
    })();
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r
}

// ------------------------------------------------------------------ create: test / delete after

/// Steps after a successful create (create dialog checkboxes).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AfterCreate {
    pub test: bool,
    pub delete: bool,
}

/// Delete a source file / folder unless it is (or contains) one of the `protect`ed archives.
/// Ok(false): kept on purpose (or already gone).
pub fn delete_source(src: &Path, protect: &[PathBuf]) -> Result<bool, String> {
    let Ok(s) = std::fs::canonicalize(src) else { return Ok(false) };
    if protect.iter().any(|o| std::fs::canonicalize(o).unwrap_or_else(|_| o.clone()).starts_with(&s)) {
        return Ok(false);
    }
    let md = std::fs::symlink_metadata(src).map_err(|e| format!("{}: {e}", src.display()))?;
    let r = if md.is_dir() { std::fs::remove_dir_all(src) } else { std::fs::remove_file(src) };
    r.map(|()| true).map_err(|e| format!("{}: {e}", src.display()))
}

/// Delete the sources of successfully created archives: (deleted, kept, error lines).
fn delete_sources<'a>(sources: impl Iterator<Item = &'a PathBuf>, protect: &[PathBuf]) -> (usize, usize, Vec<String>) {
    let (mut deleted, mut kept, mut errors) = (0, 0, Vec::new());
    for s in sources {
        match delete_source(s, protect) {
            Ok(true) => deleted += 1,
            Ok(false) => kept += 1,
            Err(e) => errors.push(format!("✗ 無法刪除 {e}")),
        }
    }
    (deleted, kept, errors)
}

fn delete_note(deleted: usize, kept: usize) -> String {
    let mut s = format!("已刪除 {deleted} 個原始項目");
    if kept > 0 {
        s.push_str(&format!("（保留 {kept} 個包含壓縮檔的項目）"));
    }
    s
}

/// Job of the create dialog (one archive): create, then test and delete the sources if asked.
pub fn create_one_msg(output: &Path, inputs: &[PathBuf], opts: &CreateOptions, post: AfterCreate, p: &Progress) -> Msg {
    if let Err(e) = archiver_core::create(output, inputs, opts, p) {
        return Msg::JobDone(Err(e.to_string()), After::Nothing);
    }
    let mut text = format!("已建立 {}", output.display());
    if post.test {
        match archiver_core::test(output, opts.password.as_deref(), p) {
            Ok(n) => text.push_str(&format!("，測試正確（{n} 個檔案）")),
            Err(Error::Cancelled) => return cancelled(),
            Err(e) => {
                let kept = if post.delete { "；未刪除原始檔案" } else { "" };
                return Msg::JobDone(Err(format!("{text}，但測試失敗：{e}{kept}")), After::Nothing);
            }
        }
    }
    if post.delete {
        if is_cancelled(p) {
            return cancelled();
        }
        let (deleted, kept, errors) = delete_sources(inputs.iter(), &[output.to_path_buf()]);
        text.push_str(&format!("，{}", delete_note(deleted, kept)));
        if !errors.is_empty() {
            return Msg::JobDone(Err(format!("{text}\n{}", errors.join("\n"))), After::Nothing);
        }
    }
    Msg::JobDone(Ok(text), After::ReloadFs)
}

/// Job of the create dialog in batch mode: one archive per input into `out_dir`, then test /
/// delete per successful item (deletion only after every test ran, never after a cancel).
pub fn create_each_msg(inputs: &[PathBuf], out_dir: &Path, opts: &CreateOptions, post: AfterCreate, p: &Progress) -> Msg {
    if let Err(e) = std::fs::create_dir_all(out_dir) {
        return Msg::JobDone(Err(format!("{}: {e}", out_dir.display())), After::Nothing);
    }
    let mut results = archiver_core::create_each(inputs, Some(out_dir), opts, p);
    if results.iter().any(|r| matches!(r.result, Err(Error::Cancelled))) {
        return cancelled();
    }
    let mut notes = Vec::new();
    if post.test {
        let mut tested = 0;
        for r in results.iter_mut().filter(|r| r.result.is_ok()) {
            match archiver_core::test(&r.output, opts.password.as_deref(), p) {
                Ok(_) => tested += 1,
                Err(Error::Cancelled) => return cancelled(),
                Err(e) => r.result = Err(e),
            }
        }
        notes.push(format!("測試正確 {tested} 個"));
    }
    let mut errors = Vec::new();
    if post.delete {
        if is_cancelled(p) {
            return cancelled();
        }
        let outputs: Vec<PathBuf> = results.iter().map(|r| r.output.clone()).collect();
        let (deleted, kept, e) = delete_sources(results.iter().filter(|r| r.result.is_ok()).map(|r| &r.input), &outputs);
        notes.push(delete_note(deleted, kept));
        errors = e;
    }
    let (text, failed) = batch_summary(&results);
    let (first, rest) = text.split_once('\n').map_or((text.as_str(), ""), |(a, b)| (a, b));
    let mut text = first.to_string();
    for n in &notes {
        text.push_str(&format!("，{n}"));
    }
    for line in rest.lines().map(str::to_string).chain(errors.iter().cloned()) {
        text.push('\n');
        text.push_str(&line);
    }
    Msg::JobDone(Ok(text), After::Batch { failed: failed + errors.len(), retry: single_password_failure(&results) })
}

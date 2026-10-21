// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod args;
mod clipboard;
mod preview;
mod shell;
mod view;

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use archiver_core::{BatchDest, BatchResult, CreateOptions, Entry, Format, Progress};
use args::Action;
use preview::{ImageKind, Preview};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use view::Item;

slint::include_modules!();

const BUSY_MSG: &str = "目前有工作正在進行，請稍候…";
const PREVIEW_PW_MSG: &str = "此檔案已加密，請先輸入壓縮檔密碼（檔案 → 壓縮檔密碼…）";
/// Bytes shown as hex when an image cannot be decoded.
const HEX_FALLBACK: usize = 16 << 10;
const PREVIEW_DEBOUNCE: Duration = Duration::from_millis(120);

enum Mode {
    Fs(PathBuf),
    Archive { path: PathBuf, fmt: Format, entries: Vec<Entry>, prefix: String },
}

/// What to do in the UI thread after a background job succeeds.
enum After {
    Nothing,
    ReloadFs,
    ReloadArchive,
    OpenFile(PathBuf),
    Message,
    /// Put these (extracted) files on the clipboard.
    CopyFiles(Vec<PathBuf>),
    /// Batch create/extract finished; the message is [`batch_summary`]. `retry`: the single
    /// archive that failed because of a password (opened instead when started from the CLI).
    Batch { failed: usize, retry: Option<PathBuf> },
}

enum Msg {
    Listed(PathBuf, Result<Vec<Entry>, String>),
    Done(Result<String, String>, After),
}

struct Job {
    rx: Receiver<Msg>,
    cancel: Arc<AtomicBool>,
    progress: Arc<Mutex<(u64, u64, String)>>,
    title: String,
}

/// What the preview worker produced.
enum PvBody {
    Text(String),
    Hex(String),
    /// Temp file holding the image, hex fallback.
    Raster(PathBuf, String),
    /// SVG data, hex fallback.
    Svg(Vec<u8>, String),
    Empty,
    Error(String),
}

struct PreviewDone {
    generation: u64,
    body: PvBody,
    /// Complete bytes of an archive entry: (archive, entry name, bytes).
    full: Option<(PathBuf, String, Vec<u8>)>,
}

enum PvSrc {
    Entry { archive: PathBuf, name: String, password: Option<String> },
    File(PathBuf),
}

struct PreviewState {
    generation: u64,
    cancel: Arc<AtomicBool>,
    timer: slint::Timer,
    tx: Sender<PreviewDone>,
    rx: Receiver<PreviewDone>,
    /// Last fully read archive entry, for the quick copy path.
    cache: Option<(PathBuf, String, Vec<u8>)>,
}

struct State {
    mode: Mode,
    items: Vec<Item>,
    selected: Vec<bool>,
    anchor: Option<usize>,
    sort_col: i32,
    sort_asc: bool,
    filter: String,
    password: String,
    job: Option<Job>,
    create_inputs: Vec<PathBuf>,
    close_when_done: bool,
    fallback_open: Option<PathBuf>,
    drops: Vec<PathBuf>,
    status: String,
    clip: clipboard::Clip,
    /// Staging folder of the files currently on the clipboard (removed when replaced).
    last_clip: Option<PathBuf>,
    pv: PreviewState,
}

type S = Rc<RefCell<State>>;

/// On Windows prefer the Skia renderer: the software renderer's partial presentation
/// can leave regions of the window unpainted (desktop shows through).
fn select_renderer() -> Result<(), slint::PlatformError> {
    let sel = || slint::BackendSelector::new().backend_name("winit".into());
    #[cfg(windows)]
    if std::env::var_os("SLINT_BACKEND").is_none() && sel().renderer_name("skia".into()).select().is_ok() {
        return Ok(());
    }
    sel().select()
}

fn main() -> Result<(), slint::PlatformError> {
    let action = match args::parse(std::env::args_os().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    match action {
        Action::Help => {
            println!("{}", args::USAGE);
            return Ok(());
        }
        Action::RegisterShell | Action::UnregisterShell => {
            let r = if action == Action::RegisterShell { shell::register() } else { shell::unregister() };
            match r {
                Ok(msg) => {
                    println!("{msg}");
                    return Ok(());
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            }
        }
        _ => {}
    }

    select_renderer()?;
    let ui = AppWindow::new()?;
    let start_dir = std::env::current_dir().ok().filter(|d| d.is_dir()).or_else(home_dir).unwrap_or_else(|| PathBuf::from("/"));
    std::thread::spawn(|| view::purge_old_clips(Duration::from_secs(24 * 3600)));
    let (pv_tx, pv_rx) = channel();
    let st: S = Rc::new(RefCell::new(State {
        mode: Mode::Fs(start_dir),
        items: vec![],
        selected: vec![],
        anchor: None,
        sort_col: 0,
        sort_asc: true,
        filter: String::new(),
        password: String::new(),
        job: None,
        create_inputs: vec![],
        close_when_done: false,
        fallback_open: None,
        drops: vec![],
        status: String::new(),
        clip: clipboard::Clip::new(),
        last_clip: None,
        pv: PreviewState { generation: 0, cancel: Arc::new(AtomicBool::new(false)), timer: slint::Timer::default(), tx: pv_tx, rx: pv_rx, cache: None },
    }));
    ui.set_mono_font(if cfg!(windows) { "Consolas" } else if cfg!(target_os = "macos") { "Menlo" } else { "monospace" }.into());
    let formats: Vec<SharedString> = Format::writable().iter().map(|f| f.label().into()).collect();
    ui.set_formats(ModelRc::new(VecModel::from(formats)));

    wire_callbacks(&ui, &st);
    install_drop_handler(&ui, &st);

    // poll background jobs + dropped files
    let timer = slint::Timer::default();
    {
        let (w, st) = (ui.as_weak(), st.clone());
        timer.start(slint::TimerMode::Repeated, Duration::from_millis(50), move || {
            if let Some(ui) = w.upgrade() {
                poll(&ui, &st);
            }
        });
    }

    refresh(&ui, &st);
    match action {
        Action::Open(p) => open_archive(&ui, &st, p),
        Action::Compress(files) => open_create(&ui, &st, files, None),
        Action::ExtractHere(files) => extract_batch(&ui, &st, files, false),
        Action::ExtractToFolder(files) => extract_batch(&ui, &st, files, true),
        _ => {}
    }
    ui.run()?;
    let _ = std::fs::remove_dir_all(view::temp_view_dir());
    Ok(())
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from)
}

fn opt_pw(st: &State) -> Option<String> {
    Some(st.password.clone()).filter(|s| !s.is_empty())
}

// ------------------------------------------------------------------ listing / refresh

fn refresh(ui: &AppWindow, st: &S) {
    let mut s = st.borrow_mut();
    let mut items = match &s.mode {
        Mode::Fs(dir) => {
            let mut v = view::fs_children(dir).unwrap_or_default();
            if !s.filter.is_empty() {
                let f = s.filter.to_lowercase();
                v.retain(|i| i.name.to_lowercase().contains(&f));
            }
            if dir.parent().is_some() {
                v.insert(0, Item::parent());
            }
            v
        }
        Mode::Archive { entries, prefix, .. } => {
            let mut v = if s.filter.is_empty() { view::archive_children(entries, prefix) } else { view::archive_search(entries, &s.filter) };
            v.insert(0, Item::parent());
            v
        }
    };
    view::sort_items(&mut items, s.sort_col, s.sort_asc);
    s.selected = vec![false; items.len()];
    s.anchor = None;
    s.items = items;
    let (title, path_text, archive_open) = match &s.mode {
        Mode::Fs(dir) => ("Archiver".to_string(), dir.display().to_string(), false),
        Mode::Archive { path, fmt, entries, prefix } => {
            let unpacked: u64 = entries.iter().filter(|e| !e.is_dir).map(|e| e.size).sum();
            let label = fmt.label().split(' ').next().unwrap_or("").to_string();
            let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let mut pt = view::archive_path_text(path, prefix, &label, unpacked);
            if !s.filter.is_empty() {
                pt = format!("{pt}  （尋找：{}）", s.filter);
            }
            (format!("{name} - Archiver"), pt, true)
        }
    };
    ui.set_window_title(title.into());
    ui.set_path_text(path_text.into());
    ui.set_archive_open(archive_open);
    ui.set_can_delete(archive_open && matches!(&s.mode, Mode::Archive { fmt, .. } if fmt.can_create() && !fmt.is_single_file()));
    ui.set_sort_column(s.sort_col);
    ui.set_sort_ascending(s.sort_asc);
    drop(s);
    push_rows(ui, st);
    selection_changed(ui, st);
}

fn push_rows(ui: &AppWindow, st: &S) {
    let s = st.borrow();
    let in_archive = matches!(s.mode, Mode::Archive { .. });
    let rows: Vec<Row> = s
        .items
        .iter()
        .zip(&s.selected)
        .map(|(it, &sel)| Row {
            name: it.name.clone().into(),
            size: it.size.map(view::fmt_num).unwrap_or_default().into(),
            packed: if in_archive { it.packed.map(view::fmt_num).unwrap_or_default() } else { String::new() }.into(),
            kind: if it.is_parent { "檔案資料夾".to_string() } else { view::type_name(&it.name, it.is_dir) }.into(),
            modified: it.modified.replace('-', "/").into(),
            crc: it.crc.map(|c| format!("{c:08X}")).unwrap_or_default().into(),
            icon: view::icon(it),
            selected: sel,
        })
        .collect();
    ui.set_rows(ModelRc::new(VecModel::from(rows)));
    update_status(ui, &s);
}

fn update_status(ui: &AppWindow, s: &State) {
    let files: Vec<&Item> = s.items.iter().filter(|i| !i.is_dir).collect();
    let total: u64 = files.iter().filter_map(|i| i.size).sum();
    let dirs = s.items.iter().filter(|i| i.is_dir && !i.is_parent).count();
    let mut right = format!("總共 {} 個檔案, {} 個位元組", files.len(), view::fmt_num(total));
    if dirs > 0 {
        right = format!("總共 {dirs} 個資料夾和 {} 個檔案, {} 個位元組", files.len(), view::fmt_num(total));
    }
    let sel: Vec<&Item> = s.items.iter().zip(&s.selected).filter(|(i, x)| **x && !i.is_parent).map(|(i, _)| i).collect();
    let left = if !s.status.is_empty() {
        s.status.clone()
    } else if sel.is_empty() {
        String::new()
    } else {
        let b: u64 = sel.iter().filter_map(|i| i.size).sum();
        format!("已選取 {} 個項目, {} 個位元組", sel.len(), view::fmt_num(b))
    };
    ui.set_status_left(left.into());
    ui.set_status_right(right.into());
}

fn set_status(ui: &AppWindow, st: &S, msg: impl Into<String>) {
    st.borrow_mut().status = msg.into();
    update_status(ui, &st.borrow());
}

fn selected_items(st: &State) -> Vec<Item> {
    st.items.iter().zip(&st.selected).filter(|(i, x)| **x && !i.is_parent).map(|(i, _)| i.clone()).collect()
}

/// Update only the selection flags in the existing row model.
fn sync_selection(ui: &AppWindow, st: &S) {
    let rows = ui.get_rows();
    let s = st.borrow();
    for (k, &sel) in s.selected.iter().enumerate() {
        if let Some(mut r) = rows.row_data(k)
            && r.selected != sel
        {
            r.selected = sel;
            rows.set_row_data(k, r);
        }
    }
    update_status(ui, &s);
}

/// Up/Down arrow: move a single selection by `delta` rows.
fn move_selection(ui: &AppWindow, st: &S, delta: i32) {
    let next = {
        let mut s = st.borrow_mut();
        let n = s.items.len();
        if n == 0 {
            return;
        }
        let cur = s.anchor.or_else(|| s.selected.iter().position(|&x| x));
        let next = cur.map_or(0, |c| (c as i64 + delta as i64).clamp(0, n as i64 - 1) as usize);
        s.selected.iter_mut().for_each(|x| *x = false);
        s.selected[next] = true;
        s.anchor = Some(next);
        s.status.clear();
        next
    };
    sync_selection(ui, st);
    ui.invoke_ensure_row_visible(next as i32);
    selection_changed(ui, st);
}

// ------------------------------------------------------------------ background jobs

fn spawn<F>(ui: &AppWindow, st: &S, title: &str, f: F)
where
    F: FnOnce(&Progress) -> Msg + Send + 'static,
{
    if is_busy(ui, st) {
        return;
    }
    let (tx, rx) = channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let progress = Arc::new(Mutex::new((0u64, 0u64, String::new())));
    let (c2, p2) = (cancel.clone(), progress.clone());
    std::thread::spawn(move || {
        let cb = move |d: u64, t: u64, n: &str| {
            if let Ok(mut g) = p2.lock() {
                *g = (d, t, n.to_string());
            }
        };
        let prog = Progress { on: &cb, cancel: c2 };
        let _ = tx.send(f(&prog));
    });
    st.borrow_mut().job = Some(Job { rx, cancel, progress, title: title.into() });
    ui.set_busy(true);
    ui.set_progress(0.0);
    ui.set_progress_text(title.into());
}

/// True (and a status hint) while a background job runs.
fn is_busy(ui: &AppWindow, st: &S) -> bool {
    let busy = st.borrow().job.is_some();
    if busy {
        set_status(ui, st, BUSY_MSG);
    }
    busy
}

fn poll(ui: &AppWindow, st: &S) {
    poll_preview(ui, st);
    let drops = std::mem::take(&mut st.borrow_mut().drops);
    if !drops.is_empty() {
        handle_drops(ui, st, drops);
    }
    let msg = {
        let s = st.borrow();
        let Some(job) = &s.job else { return };
        let (d, t, n) = job.progress.lock().map(|g| g.clone()).unwrap_or_default();
        ui.set_progress(if t > 0 { (d as f32 / t as f32).min(1.0) } else { 0.0 });
        ui.set_progress_text(if n.is_empty() { job.title.clone() } else { format!("{} {n}", job.title) }.into());
        match job.rx.try_recv() {
            Ok(m) => m,
            Err(_) => return,
        }
    };
    st.borrow_mut().job = None;
    ui.set_busy(false);
    match msg {
        Msg::Listed(path, Ok(entries)) => {
            let fmt = Format::detect(&path).unwrap_or(Format::Zip);
            {
                let mut s = st.borrow_mut();
                // re-listing the open archive (F5, after delete) keeps the folder and status
                let prefix = match &s.mode {
                    Mode::Archive { path: p, prefix, .. } if *p == path => Some(prefix.clone()),
                    _ => None,
                };
                if prefix.is_none() {
                    s.filter.clear();
                    s.status.clear();
                }
                s.mode = Mode::Archive { path, fmt, entries, prefix: prefix.unwrap_or_default() };
                s.pv.cache = None;
            }
            refresh(ui, st);
        }
        Msg::Listed(path, Err(e)) => {
            set_status(ui, st, format!("無法開啟 {}: {e}", path.display()));
            if e.to_lowercase().contains("password") {
                ui.set_archive_password(SharedString::new());
                ui.set_dialog("password".into());
                st.borrow_mut().fallback_open = Some(path);
            }
        }
        Msg::Done(Ok(text), after) => {
            st.borrow_mut().status = text.clone();
            let batch_failed = matches!(after, After::Batch { failed, .. } if failed > 0);
            if st.borrow().close_when_done && !batch_failed {
                let _ = ui.hide();
                let _ = slint::quit_event_loop();
                return;
            }
            match after {
                After::Nothing => update_status(ui, &st.borrow()),
                After::ReloadFs => {
                    if matches!(st.borrow().mode, Mode::Fs(_)) {
                        refresh(ui, st);
                    }
                    update_status(ui, &st.borrow());
                }
                After::ReloadArchive => {
                    let path = match &st.borrow().mode {
                        Mode::Archive { path, .. } => Some(path.clone()),
                        _ => None,
                    };
                    if let Some(p) = path {
                        reload_archive_keep_prefix(ui, st, p);
                    }
                }
                After::OpenFile(p) => {
                    open_path(&p);
                    update_status(ui, &st.borrow());
                }
                After::Message => {
                    update_status(ui, &st.borrow());
                    info_box("Archiver", &text);
                }
                After::CopyFiles(paths) => copy_to_clipboard(ui, st, &paths),
                After::Batch { failed, retry } => {
                    let from_cli = {
                        let mut s = st.borrow_mut();
                        s.status = text.lines().next().unwrap_or_default().to_string();
                        std::mem::take(&mut s.close_when_done)
                    };
                    if matches!(st.borrow().mode, Mode::Fs(_)) {
                        refresh(ui, st);
                    }
                    update_status(ui, &st.borrow());
                    match retry.filter(|_| from_cli && failed == 1) {
                        // e.g. encrypted archive from the context menu: open it so a password can be entered
                        Some(p) => open_archive(ui, st, p),
                        None if failed > 0 => error_box(&text),
                        None => {}
                    }
                }
            }
        }
        Msg::Done(Err(e), _) => {
            let fallback = {
                let mut s = st.borrow_mut();
                s.close_when_done = false;
                s.fallback_open.take()
            };
            set_status(ui, st, format!("錯誤：{e}"));
            if let Some(p) = fallback {
                // e.g. encrypted archive from the context menu: open it so a password can be entered
                open_archive(ui, st, p);
            } else {
                error_box(&e);
            }
        }
    }
}

fn reload_archive_keep_prefix(ui: &AppWindow, st: &S, path: PathBuf) {
    // listing can take long (.tar.gz is read in full): in the background, like open_archive
    let pw = opt_pw(&st.borrow());
    spawn(ui, st, "讀取中…", move |_| {
        let r = archiver_core::list(&path, pw.as_deref()).map_err(|e| e.to_string());
        Msg::Listed(path, r)
    });
}

// ------------------------------------------------------------------ actions

fn open_archive(ui: &AppWindow, st: &S, path: PathBuf) {
    if Format::detect(&path).is_none() {
        set_status(ui, st, format!("不支援的格式：{}", path.display()));
        return;
    }
    let pw = opt_pw(&st.borrow());
    spawn(ui, st, "讀取中…", move |_| {
        let r = archiver_core::list(&path, pw.as_deref()).map_err(|e| e.to_string());
        Msg::Listed(path, r)
    });
}

/// Context-menu "extract here / to folder": extract every archive, continue after failures.
fn extract_batch(ui: &AppWindow, st: &S, files: Vec<PathBuf>, to_folder: bool) {
    st.borrow_mut().close_when_done = true;
    let dest = if to_folder { BatchDest::BesideInFolder } else { BatchDest::Beside };
    spawn(ui, st, "解壓縮中…", move |p| batch_msg(&archiver_core::extract_each(&files, &dest, None, p)));
}

fn is_password_err(e: &str) -> bool {
    e.to_lowercase().contains("password")
}

/// "已完成 N 個（成功 X，失敗 Y）" plus one line per failure (at most 20); returns the failure count.
fn batch_summary(results: &[BatchResult]) -> (String, usize) {
    const MAX_LINES: usize = 20;
    let failed: Vec<(&BatchResult, String)> = results.iter().filter_map(|r| r.result.as_ref().err().map(|e| (r, e.to_string()))).collect();
    let mut text = format!("已完成 {} 個（成功 {}，失敗 {}）", results.len(), results.len() - failed.len(), failed.len());
    for (r, e) in failed.iter().take(MAX_LINES) {
        let name = r.input.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| r.input.display().to_string());
        text.push_str(&format!("\n✗ {name}：{e}"));
    }
    if failed.len() > MAX_LINES {
        text.push_str(&format!("\n…還有 {} 個", failed.len() - MAX_LINES));
    }
    (text, failed.len())
}

/// The input of the only failed item, if it failed because of a missing/wrong password.
fn single_password_failure(results: &[BatchResult]) -> Option<PathBuf> {
    let mut failed = results.iter().filter(|r| r.result.is_err());
    match (failed.next(), failed.next()) {
        (Some(r), None) if r.result.as_ref().is_err_and(|e| is_password_err(&e.to_string())) => Some(r.input.clone()),
        _ => None,
    }
}

fn batch_msg(results: &[BatchResult]) -> Msg {
    let (text, failed) = batch_summary(results);
    Msg::Done(Ok(text), After::Batch { failed, retry: single_password_failure(results) })
}

/// Open the create dialog with `files` added. `each`: preset the batch checkbox (a fresh
/// dialog defaults to a single archive).
fn open_create(ui: &AppWindow, st: &S, files: Vec<PathBuf>, each: Option<bool>) {
    {
        let mut s = st.borrow_mut();
        for f in files {
            if !s.create_inputs.contains(&f) {
                s.create_inputs.push(f);
            }
        }
    }
    if ui.get_dialog() != "create" {
        ui.set_create_password(SharedString::new());
        ui.set_create_each(each.unwrap_or(false));
        let idx = ui.get_format_index().max(0) as usize;
        apply_format(ui, st, idx);
    } else if let Some(e) = each {
        ui.set_create_each(e);
    }
    update_create_fields(ui, st);
    ui.set_dialog("create".into());
}

fn update_create_fields(ui: &AppWindow, st: &S) {
    let s = st.borrow();
    let text = s.create_inputs.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\n");
    ui.set_create_sources(text.into());
    let fmt = Format::writable()[ui.get_format_index().max(0) as usize];
    let each = ui.get_create_each();
    let cur = ui.get_create_output();
    let cur = Path::new(cur.trim());
    if let Some(first) = s.create_inputs.first() {
        if !each {
            ui.set_create_output(archiver_core::suggest_output(&s.create_inputs, fmt).display().to_string().into());
        } else if cur.as_os_str().is_empty() || (!cur.is_dir() && Format::detect(cur).is_some()) {
            // empty, or still the single-archive file name: use the first input's folder
            let dir = first.parent().filter(|p| !p.as_os_str().is_empty()).map(Path::to_path_buf).or_else(|| std::env::current_dir().ok()).unwrap_or_default();
            ui.set_create_output(dir.display().to_string().into());
        }
    }
    let warn = if each { s.create_inputs.iter().any(|p| p.is_dir()) } else { s.create_inputs.len() != 1 };
    ui.set_single_file_warning(fmt.is_single_file() && warn);
}

fn apply_format(ui: &AppWindow, st: &S, idx: usize) {
    let fmt = Format::writable()[idx.min(Format::writable().len() - 1)];
    ui.set_format_index(idx as i32);
    match fmt.level_range() {
        Some((lo, hi, def)) => {
            ui.set_level_enabled(true);
            ui.set_level_min(lo as f32);
            ui.set_level_max(hi as f32);
            let cur = ui.get_level();
            if cur < lo as f32 || cur > hi as f32 {
                ui.set_level(def as f32);
            }
        }
        None => ui.set_level_enabled(false),
    }
    ui.set_password_enabled(fmt.supports_password());
    update_create_fields(ui, st);
}

fn start_create(ui: &AppWindow, st: &S) {
    let fmt = Format::writable()[ui.get_format_index().max(0) as usize];
    let output = PathBuf::from(ui.get_create_output().trim());
    let inputs = st.borrow().create_inputs.clone();
    let each = ui.get_create_each();
    if inputs.is_empty() || output.as_os_str().is_empty() || is_busy(ui, st) {
        return;
    }
    if !each && output.exists() && !confirm("覆寫檔案？", &format!("{} 已存在，要覆寫嗎？", output.display())) {
        return;
    }
    let pw = ui.get_create_password().to_string();
    let opts = CreateOptions {
        format: fmt,
        level: fmt.level_range().map(|_| ui.get_level().round() as u32),
        password: Some(pw).filter(|p| !p.is_empty() && fmt.supports_password()),
    };
    st.borrow_mut().create_inputs.clear();
    ui.set_dialog("".into());
    if each {
        // output is a folder; core picks a unique archive name per input
        return spawn(ui, st, "分批壓縮中…", move |p| {
            if let Err(e) = std::fs::create_dir_all(&output) {
                return Msg::Done(Err(format!("{}: {e}", output.display())), After::Nothing);
            }
            batch_msg(&archiver_core::create_each(&inputs, Some(&output), &opts, p))
        });
    }
    spawn(ui, st, "壓縮中…", move |p| {
        let r = archiver_core::create(&output, &inputs, &opts, p)
            .map(|_| format!("已建立 {}", output.display()))
            .map_err(|e| e.to_string());
        Msg::Done(r, After::ReloadFs)
    });
}

fn do_action(ui: &AppWindow, st: &S, a: &str) {
    match a {
        "open" => {
            let exts = archiver_core::archive_extensions();
            if let Some(p) = rfd::FileDialog::new().set_title("開啟壓縮檔").add_filter("壓縮檔", &exts).add_filter("所有檔案", &["*"]).pick_file() {
                open_archive(ui, st, p);
            }
        }
        "add" => do_add(ui, st, None),
        "add-each" => do_add(ui, st, Some(true)),
        "extract" => do_extract(ui, st),
        "extract-here" => do_extract_each(ui, st, BatchDest::Beside),
        "extract-each" => do_extract_each(ui, st, BatchDest::BesideInFolder),
        "copy" => do_copy(ui, st),
        "copy-text" => {
            let text = ui.get_preview_text();
            let r = st.borrow_mut().clip.copy_text(&text);
            match r {
                Ok(()) => set_status(ui, st, "已複製文字到剪貼簿"),
                Err(e) => set_status(ui, st, format!("錯誤：{e}")),
            }
        }
        "refresh" => {
            let arc = {
                let mut s = st.borrow_mut();
                s.status.clear();
                match &s.mode {
                    Mode::Archive { path, .. } => Some(path.clone()),
                    Mode::Fs(_) => None,
                }
            };
            match arc {
                Some(p) => reload_archive_keep_prefix(ui, st, p),
                None => refresh(ui, st),
            }
        }
        "preview-changed" => selection_changed(ui, st),
        "create-each-changed" => update_create_fields(ui, st),
        "test" => do_test(ui, st),
        "view" => {
            let first = selected_items(&st.borrow()).into_iter().find(|i| !i.is_dir);
            if let Some(it) = first {
                view_item(ui, st, it);
            }
        }
        "delete" => do_delete(ui, st),
        "find" => {
            ui.set_find_text(st.borrow().filter.clone().into());
            ui.set_dialog("find".into());
        }
        "find-ok" => {
            st.borrow_mut().filter = ui.get_find_text().trim().to_string();
            ui.set_dialog("".into());
            refresh(ui, st);
        }
        "wizard" => {
            if matches!(st.borrow().mode, Mode::Archive { .. }) {
                do_extract(ui, st);
            } else {
                do_action(ui, st, "add");
            }
        }
        "info" => {
            ui.set_info_text(info_text(&st.borrow()).into());
            ui.set_dialog("info".into());
        }
        "shell" => {
            ui.set_shell_enabled(shell::is_registered());
            let d = if cfg!(windows) {
                "Windows 檔案總管右鍵選單（目前使用者 HKCU，不需系統管理員）：開啟、解壓縮到這裡、解壓縮到資料夾、加入壓縮檔。"
            } else if cfg!(target_os = "macos") {
                "Finder 快速動作（~/Library/Services）。"
            } else {
                "Nautilus 腳本、Dolphin 服務選單、Nemo 動作與 .desktop MIME 關聯。"
            };
            ui.set_shell_description(d.into());
            ui.set_dialog("shell".into());
        }
        "password" => {
            ui.set_archive_password(st.borrow().password.clone().into());
            ui.set_dialog("password".into());
        }
        "password-ok" => {
            st.borrow_mut().password = ui.get_archive_password().to_string();
            ui.set_dialog("".into());
            let target = {
                let mut s = st.borrow_mut();
                match &s.mode {
                    Mode::Archive { path, .. } => Some(path.clone()),
                    _ => s.fallback_open.take(),
                }
            };
            if let Some(p) = target {
                open_archive(ui, st, p);
            }
        }
        "create-browse" if ui.get_create_each() => {
            let cur = PathBuf::from(ui.get_create_output().trim());
            let mut d = rfd::FileDialog::new().set_title("選擇輸出資料夾");
            if cur.is_dir() {
                d = d.set_directory(&cur);
            }
            if let Some(p) = d.pick_folder() {
                ui.set_create_output(p.display().to_string().into());
            }
        }
        "create-browse" => {
            let cur = PathBuf::from(ui.get_create_output().to_string());
            let mut d = rfd::FileDialog::new().set_title("儲存壓縮檔");
            if let Some(dir) = cur.parent().filter(|p| p.is_dir()) {
                d = d.set_directory(dir);
            }
            if let Some(n) = cur.file_name() {
                d = d.set_file_name(n.to_string_lossy());
            }
            if let Some(p) = d.save_file() {
                ui.set_create_output(p.display().to_string().into());
            }
        }
        "create-ok" => start_create(ui, st),
        "dialog-close" => {
            if ui.get_dialog() == "create" {
                st.borrow_mut().create_inputs.clear();
            }
            ui.set_dialog("".into());
        }
        "select-all" => {
            {
                let mut s = st.borrow_mut();
                let n = s.items.len();
                s.selected = (0..n).map(|i| !s.items[i].is_parent).collect();
                s.status.clear();
            }
            push_rows(ui, st);
            selection_changed(ui, st);
        }
        "up" => go_up(ui, st),
        "cancel" => {
            if let Some(j) = &st.borrow().job {
                j.cancel.store(true, Ordering::Relaxed);
            }
        }
        "about" => info_box(
            "關於 Archiver",
            &format!("Archiver {}\n跨平台壓縮／解壓縮工具（Rust + Slint）\nhttps://github.com/stevenke1981/Archiver\nMIT License", env!("CARGO_PKG_VERSION")),
        ),
        "exit" => {
            let _ = slint::quit_event_loop();
        }
        _ => {}
    }
}

fn go_up(ui: &AppWindow, st: &S) {
    {
        let mut s = st.borrow_mut();
        s.status.clear();
        if !s.filter.is_empty() {
            s.filter.clear();
        } else {
            let next = match &mut s.mode {
                Mode::Fs(dir) => {
                    if let Some(p) = dir.parent() {
                        *dir = p.to_path_buf();
                    }
                    None
                }
                Mode::Archive { path, prefix, .. } => {
                    if prefix.is_empty() {
                        Some(path.parent().map(Path::to_path_buf).unwrap_or_default())
                    } else {
                        *prefix = view::parent_prefix(prefix);
                        None
                    }
                }
            };
            if let Some(d) = next {
                s.mode = Mode::Fs(d);
                s.pv.cache = None;
            }
        }
    }
    refresh(ui, st);
}

fn activate(ui: &AppWindow, st: &S, idx: usize) {
    let Some(it) = st.borrow().items.get(idx).cloned() else { return };
    if it.is_parent {
        return go_up(ui, st);
    }
    let in_archive = matches!(st.borrow().mode, Mode::Archive { .. });
    if it.is_dir {
        {
            let mut s = st.borrow_mut();
            s.filter.clear();
            s.status.clear();
            match &mut s.mode {
                Mode::Fs(dir) => *dir = PathBuf::from(&it.full),
                Mode::Archive { prefix, .. } => *prefix = format!("{}/", it.full),
            }
        }
        return refresh(ui, st);
    }
    if in_archive {
        view_item(ui, st, it);
    } else if Format::detect(Path::new(&it.name)).is_some() {
        open_archive(ui, st, PathBuf::from(it.full));
    } else {
        open_path(Path::new(&it.full));
    }
}

/// Extract one archive entry to a temp folder and open it with the system default app.
fn view_item(ui: &AppWindow, st: &S, it: Item) {
    let (arc, pw) = {
        let s = st.borrow();
        match &s.mode {
            Mode::Archive { path, .. } => (path.clone(), opt_pw(&s)),
            Mode::Fs(_) => {
                drop(s);
                return open_path(Path::new(&it.full));
            }
        }
    };
    let dest = view::temp_view_dir();
    spawn(ui, st, "解壓縮檢視中…", move |p| {
        let target = if Format::detect(&arc).is_some_and(|f| f.is_single_file()) { dest.join(&it.name) } else { dest.join(&it.full) };
        match archiver_core::extract(&arc, &dest, pw.as_deref(), Some(&[it.full.clone()]), p) {
            Ok(()) => Msg::Done(Ok(format!("已開啟 {}", it.name)), After::OpenFile(target)),
            Err(e) => Msg::Done(Err(e.to_string()), After::Nothing),
        }
    });
}

fn do_extract(ui: &AppWindow, st: &S) {
    let (sources, sel, start, pw): (Vec<PathBuf>, Option<Vec<String>>, PathBuf, Option<String>) = {
        let s = st.borrow();
        let sel = selected_items(&s);
        match &s.mode {
            Mode::Archive { path, .. } => {
                let names = (!sel.is_empty()).then(|| sel.iter().map(|i| i.full.clone()).collect());
                (vec![path.clone()], names, path.parent().map(Path::to_path_buf).unwrap_or_default(), opt_pw(&s))
            }
            Mode::Fs(dir) => {
                let arcs: Vec<PathBuf> = sel.iter().filter(|i| !i.is_dir && Format::detect(Path::new(&i.name)).is_some()).map(|i| PathBuf::from(&i.full)).collect();
                (arcs, None, dir.clone(), opt_pw(&s))
            }
        }
    };
    if sources.is_empty() {
        let Some(p) = rfd::FileDialog::new().set_title("選擇要解壓縮的壓縮檔").pick_file() else { return };
        return {
            open_archive(ui, st, p);
        };
    }
    let Some(dest) = rfd::FileDialog::new().set_title("解壓縮到").set_directory(&start).pick_folder() else { return };
    if sources.len() > 1 {
        let dest = BatchDest::IntoFolders(dest);
        return spawn(ui, st, "解壓縮中…", move |p| batch_msg(&archiver_core::extract_each(&sources, &dest, pw.as_deref(), p)));
    }
    spawn(ui, st, "解壓縮中…", move |p| match archiver_core::extract(&sources[0], &dest, pw.as_deref(), sel.as_deref(), p) {
        Ok(()) => Msg::Done(Ok(format!("已解壓縮到 {}", dest.display())), After::ReloadFs),
        Err(e) => Msg::Done(Err(format!("{}: {e}", sources[0].display())), After::Nothing),
    });
}

/// "Extract here" / "extract each to its own folder": the selected archives (file-system
/// mode) or the open archive.
fn do_extract_each(ui: &AppWindow, st: &S, dest: BatchDest) {
    let (arcs, pw) = {
        let s = st.borrow();
        let arcs: Vec<PathBuf> = match &s.mode {
            Mode::Archive { path, .. } => vec![path.clone()],
            Mode::Fs(_) => selected_items(&s).into_iter().filter(|i| !i.is_dir && Format::detect(Path::new(&i.name)).is_some()).map(|i| PathBuf::from(i.full)).collect(),
        };
        (arcs, opt_pw(&s))
    };
    if arcs.is_empty() {
        return set_status(ui, st, "請先開啟或選取壓縮檔");
    }
    spawn(ui, st, "解壓縮中…", move |p| batch_msg(&archiver_core::extract_each(&arcs, &dest, pw.as_deref(), p)));
}

fn do_add(ui: &AppWindow, st: &S, each: Option<bool>) {
    let fs_sel: Vec<PathBuf> = {
        let s = st.borrow();
        match &s.mode {
            Mode::Fs(_) => selected_items(&s).into_iter().map(|i| PathBuf::from(i.full)).collect(),
            _ => vec![],
        }
    };
    if !fs_sel.is_empty() {
        open_create(ui, st, fs_sel, each);
    } else if let Some(f) = rfd::FileDialog::new().set_title("選擇要加入的檔案").pick_files() {
        open_create(ui, st, f, each);
    }
}

// ------------------------------------------------------------------ clipboard

fn copy_to_clipboard(ui: &AppWindow, st: &S, paths: &[PathBuf]) {
    let r = st.borrow_mut().clip.copy_files(paths);
    match r {
        Ok(()) => {
            // the previous staging copy is no longer on the clipboard: free its disk space now
            let staged = paths.first().and_then(|p| view::clip_dir_of(p));
            let old = std::mem::replace(&mut st.borrow_mut().last_clip, staged.clone());
            if let Some(old) = old.filter(|o| Some(o) != staged.as_ref()) {
                std::thread::spawn(move || std::fs::remove_dir_all(old));
            }
            set_status(ui, st, format!("已複製 {} 個項目到剪貼簿，可在檔案總管中貼上", paths.len()))
        }
        Err(e) => {
            set_status(ui, st, format!("錯誤：{e}"));
            error_box(&e);
        }
    }
}

/// Write the cached bytes of a single archive entry into a fresh clipboard staging folder.
fn stage_bytes(bytes: &[u8], items: &[Item], single_file: bool) -> std::io::Result<Vec<PathBuf>> {
    let dest = view::new_clip_dir()?;
    let paths = view::extracted_paths(&dest, items, single_file);
    let r = (|| {
        let target = paths.first().ok_or_else(|| std::io::Error::other("no target path"))?;
        if let Some(dir) = target.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(target, bytes)
    })();
    if r.is_err() {
        let _ = std::fs::remove_dir_all(&dest);
    }
    r.map(|()| paths)
}

/// Copy the selected items to the clipboard as files (archive items are extracted first).
fn do_copy(ui: &AppWindow, st: &S) {
    let (sel, arc, pw) = {
        let s = st.borrow();
        let arc = match &s.mode {
            Mode::Archive { path, fmt, .. } => Some((path.clone(), *fmt)),
            Mode::Fs(_) => None,
        };
        (selected_items(&s), arc, opt_pw(&s))
    };
    if sel.is_empty() {
        return set_status(ui, st, "請先選取要複製的檔案");
    }
    let Some((arc, fmt)) = arc else {
        let paths: Vec<PathBuf> = sel.iter().map(|i| PathBuf::from(&i.full)).collect();
        return copy_to_clipboard(ui, st, &paths);
    };
    // quick path: the preview already holds the complete entry
    if let [it] = sel.as_slice()
        && !it.is_dir
    {
        let staged = {
            let s = st.borrow();
            s.pv.cache.as_ref().filter(|(a, n, _)| *a == arc && *n == it.full).map(|(_, _, b)| stage_bytes(b, &sel, fmt.is_single_file()))
        };
        if let Some(Ok(paths)) = staged {
            return copy_to_clipboard(ui, st, &paths);
        }
    }
    let names: Vec<String> = sel.iter().map(|i| i.full.clone()).collect();
    spawn(ui, st, "複製到剪貼簿…", move |p| {
        let dest = match view::new_clip_dir() {
            Ok(d) => d,
            Err(e) => return Msg::Done(Err(e.to_string()), After::Nothing),
        };
        let r = archiver_core::extract(&arc, &dest, pw.as_deref(), Some(&names), p).map_err(|e| e.to_string()).and_then(|_| {
            // only what was really written (entries core refused are not on disk)
            let paths: Vec<PathBuf> = view::extracted_paths(&dest, &sel, fmt.is_single_file()).into_iter().filter(|p| p.exists()).collect();
            if paths.is_empty() { Err("沒有可複製的檔案".to_string()) } else { Ok(paths) }
        });
        match r {
            Ok(paths) => Msg::Done(Ok(String::new()), After::CopyFiles(paths)),
            Err(e) => {
                let _ = std::fs::remove_dir_all(&dest);
                Msg::Done(Err(e), After::Nothing)
            }
        }
    });
}

// ------------------------------------------------------------------ preview pane

fn set_preview(ui: &AppWindow, mode: &str, title: &str, meta: &str, text: &str) {
    ui.set_preview_mode(mode.into());
    ui.set_preview_title(title.into());
    ui.set_preview_meta(meta.into());
    ui.set_preview_text(text.into());
}

/// "12,345 位元組　2026/10/10 12:00　CRC32 0000ABCD"
fn item_meta(it: &Item) -> String {
    let mut v = Vec::new();
    if let Some(n) = it.size {
        v.push(format!("{} 位元組", view::fmt_num(n)));
    }
    if !it.modified.is_empty() {
        v.push(it.modified.replace('-', "/"));
    }
    if let Some(c) = it.crc {
        v.push(format!("CRC32 {c:08X}"));
    }
    v.join("　")
}

/// Contents of the archive folder `dir` (entry name without trailing '/').
fn folder_summary(entries: &[Entry], dir: &str) -> String {
    let prefix = format!("{}/", dir.trim_end_matches('/'));
    let (mut files, mut bytes, mut dirs) = (0usize, 0u64, BTreeSet::new());
    for e in entries {
        let name = e.name.replace('\\', "/");
        let name = name.trim_start_matches("./").trim_start_matches('/');
        let Some(rest) = name.strip_prefix(&prefix) else { continue };
        let rest = rest.trim_end_matches('/');
        if rest.is_empty() {
            continue;
        }
        for (i, c) in rest.char_indices() {
            if c == '/' {
                dirs.insert(rest[..i].to_string());
            }
        }
        if e.is_dir {
            dirs.insert(rest.to_string());
        } else {
            files += 1;
            bytes += e.size;
        }
    }
    format!("包含 {files} 個檔案、{} 個資料夾\n共 {} 個位元組", dirs.len(), view::fmt_num(bytes))
}

/// Selection changed (or the pane was toggled / the listing reloaded): cancel the running
/// preview and show the new one (files are read after a short debounce).
fn selection_changed(ui: &AppWindow, st: &S) {
    let mut s = st.borrow_mut();
    s.pv.generation += 1;
    s.pv.cancel.store(true, Ordering::Relaxed);
    s.pv.timer.stop();
    if !ui.get_preview_visible() {
        return;
    }
    ui.set_preview_image(slint::Image::default());
    let sel = selected_items(&s);
    let it = match sel.as_slice() {
        [] => return set_preview(ui, "none", "", "", ""),
        [it] => it.clone(),
        many => {
            let bytes: u64 = many.iter().filter_map(|i| i.size).sum();
            return set_preview(ui, "info", &format!("已選取 {} 個項目", many.len()), "", &format!("共 {} 個位元組", view::fmt_num(bytes)));
        }
    };
    let meta = item_meta(&it);
    if it.is_dir {
        let text = match &s.mode {
            Mode::Archive { entries, .. } => folder_summary(entries, &it.full),
            Mode::Fs(_) => "檔案資料夾（按兩下開啟）".to_string(),
        };
        return set_preview(ui, "folder", &it.name, &meta, &text);
    }
    set_preview(ui, "loading", &it.name, &meta, "");
    let generation = s.pv.generation;
    let (w, weak) = (ui.as_weak(), Rc::downgrade(st));
    s.pv.timer.start(slint::TimerMode::SingleShot, PREVIEW_DEBOUNCE, move || {
        if let Some(st) = weak.upgrade()
            && w.upgrade().is_some()
        {
            start_preview(&st, generation, &it);
        }
    });
}

fn start_preview(st: &S, generation: u64, it: &Item) {
    let mut s = st.borrow_mut();
    if s.pv.generation != generation {
        return;
    }
    let src = match &s.mode {
        Mode::Archive { path, .. } => PvSrc::Entry { archive: path.clone(), name: it.full.clone(), password: opt_pw(&s) },
        Mode::Fs(_) => PvSrc::File(PathBuf::from(&it.full)),
    };
    let cancel = Arc::new(AtomicBool::new(false));
    s.pv.cancel = cancel.clone();
    let (tx, name) = (s.pv.tx.clone(), it.name.clone());
    std::thread::spawn(move || {
        let _ = tx.send(preview_worker(generation, src, &name, cancel));
    });
}

/// First `max` bytes of a file and whether it is longer.
fn read_head(path: &Path, max: usize) -> std::io::Result<(Vec<u8>, bool)> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?.take(max as u64 + 1).read_to_end(&mut bytes)?;
    let truncated = bytes.len() > max;
    bytes.truncate(max);
    Ok((bytes, truncated))
}

/// Background part of a preview: read the data and classify it.
fn preview_worker(generation: u64, src: PvSrc, name: &str, cancel: Arc<AtomicBool>) -> PreviewDone {
    let limit = preview::read_limit(name);
    let read = match &src {
        PvSrc::Entry { archive, name: entry, password } => {
            let noop = |_: u64, _: u64, _: &str| {};
            archiver_core::read_entry(archive, entry, password.as_deref(), limit, &Progress { on: &noop, cancel }).map(|d| (d.bytes, d.truncated)).map_err(|e| e.to_string())
        }
        PvSrc::File(p) => read_head(p, limit).map_err(|e| e.to_string()),
    };
    let (bytes, truncated) = match read {
        Ok(r) => r,
        Err(e) => return PreviewDone { generation, body: PvBody::Error(e), full: None },
    };
    let body = match preview::classify(name, &bytes, truncated) {
        Preview::Text(t) => PvBody::Text(t),
        Preview::Hex(h) => PvBody::Hex(h),
        Preview::Empty => PvBody::Empty,
        Preview::Image(ImageKind::Svg) => PvBody::Svg(bytes.clone(), preview::hex_dump(&bytes, HEX_FALLBACK)),
        Preview::Image(ImageKind::Raster) => {
            let ext = Path::new(name).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_else(|| "img".into());
            let dir = view::temp_view_dir().join("preview");
            let file = dir.join(format!("{generation}.{ext}"));
            match std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&file, &bytes)) {
                Ok(()) => PvBody::Raster(file, preview::hex_dump(&bytes, HEX_FALLBACK)),
                Err(_) => PvBody::Hex(preview::hex_dump(&bytes, HEX_FALLBACK)),
            }
        }
    };
    let full = match src {
        PvSrc::Entry { archive, name, .. } if !truncated => Some((archive, name, bytes)),
        _ => None,
    };
    PreviewDone { generation, body, full }
}

/// Apply the finished preview of the current generation (called from the poll timer).
fn poll_preview(ui: &AppWindow, st: &S) {
    let mut latest = None;
    {
        let s = st.borrow();
        while let Ok(d) = s.pv.rx.try_recv() {
            if d.generation == s.pv.generation {
                latest = Some(d);
            } else if let PvBody::Raster(p, _) = &d.body {
                let _ = std::fs::remove_file(p);
            }
        }
    }
    let Some(d) = latest else { return };
    if d.full.is_some() {
        st.borrow_mut().pv.cache = d.full;
    }
    let (mode, text) = match d.body {
        PvBody::Text(t) => ("text", t),
        PvBody::Hex(h) => ("hex", h),
        PvBody::Empty => ("info", "（空白檔案，0 位元組）".to_string()),
        PvBody::Raster(p, fallback) => {
            let img = slint::Image::load_from_path(&p);
            let _ = std::fs::remove_file(&p); // already decoded into memory
            match img {
                Ok(img) => {
                    ui.set_preview_image(img);
                    ("image", String::new())
                }
                Err(_) => ("hex", fallback),
            }
        }
        PvBody::Svg(data, fallback) => match slint::Image::load_from_svg_data(&data) {
            Ok(img) => {
                ui.set_preview_image(img);
                ("image", String::new())
            }
            Err(_) => ("hex", fallback),
        },
        PvBody::Error(e) => ("error", if is_password_err(&e) { PREVIEW_PW_MSG.to_string() } else { format!("無法預覽：{e}") }),
    };
    ui.set_preview_text(text.into());
    ui.set_preview_mode(mode.into());
}

fn do_test(ui: &AppWindow, st: &S) {
    let (targets, pw) = {
        let s = st.borrow();
        let t: Vec<PathBuf> = match &s.mode {
            Mode::Archive { path, .. } => vec![path.clone()],
            Mode::Fs(_) => selected_items(&s).into_iter().filter(|i| Format::detect(Path::new(&i.name)).is_some()).map(|i| PathBuf::from(i.full)).collect(),
        };
        (t, opt_pw(&s))
    };
    if targets.is_empty() {
        return set_status(ui, st, "請先開啟或選取壓縮檔");
    }
    spawn(ui, st, "測試中…", move |p| {
        let mut lines = Vec::new();
        for a in &targets {
            match archiver_core::test(a, pw.as_deref(), p) {
                Ok(n) => lines.push(format!("{}：{n} 個檔案測試正確，沒有發現錯誤。", a.file_name().unwrap_or_default().to_string_lossy())),
                Err(e) => return Msg::Done(Err(format!("{}：{e}", a.display())), After::Nothing),
            }
        }
        Msg::Done(Ok(lines.join("\n")), After::Message)
    });
}

fn do_delete(ui: &AppWindow, st: &S) {
    let (arc, names, pw) = {
        let s = st.borrow();
        let Mode::Archive { path, .. } = &s.mode else { return };
        let names: Vec<String> = selected_items(&s).into_iter().map(|i| i.full).collect();
        (path.clone(), names, opt_pw(&s))
    };
    if names.is_empty() {
        return set_status(ui, st, "請先選取要刪除的檔案");
    }
    if !confirm("刪除檔案", &format!("確定要從壓縮檔中刪除 {} 個項目嗎？", names.len())) {
        return;
    }
    spawn(ui, st, "刪除中…", move |p| {
        let r = archiver_core::delete(&arc, &names, pw.as_deref(), p).map(|_| format!("已刪除 {} 個項目", names.len())).map_err(|e| e.to_string());
        Msg::Done(r, After::ReloadArchive)
    });
}

fn info_text(s: &State) -> String {
    match &s.mode {
        Mode::Archive { path, fmt, entries, .. } => {
            let files = entries.iter().filter(|e| !e.is_dir).count();
            let dirs = entries.iter().filter(|e| e.is_dir).count();
            let unpacked: u64 = entries.iter().filter(|e| !e.is_dir).map(|e| e.size).sum();
            let packed = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            let ratio = if unpacked > 0 { packed as f64 * 100.0 / unpacked as f64 } else { 0.0 };
            let enc = entries.iter().any(|e| e.encrypted) || !s.password.is_empty();
            format!(
                "壓縮檔：{}\n格式：{}\n檔案：{}　資料夾：{}\n未封裝大小：{} 位元組\n壓縮檔大小：{} 位元組\n壓縮率：{:.1}%\n加密：{}",
                path.display(),
                fmt.label(),
                files,
                dirs,
                view::fmt_num(unpacked),
                view::fmt_num(packed),
                ratio,
                if enc { "是" } else { "否" }
            )
        }
        Mode::Fs(dir) => {
            let files = s.items.iter().filter(|i| !i.is_dir).count();
            let total: u64 = s.items.iter().filter_map(|i| i.size).sum();
            format!("資料夾：{}\n檔案：{files}\n大小：{} 位元組\n\n開啟壓縮檔後可查看壓縮檔資訊。", dir.display(), view::fmt_num(total))
        }
    }
}

fn handle_drops(ui: &AppWindow, st: &S, drops: Vec<PathBuf>) {
    if st.borrow().job.is_some() {
        return;
    }
    if drops.len() == 1 && drops[0].is_file() && Format::detect(&drops[0]).is_some() && ui.get_dialog() != "create" {
        open_archive(ui, st, drops[0].clone());
    } else {
        open_create(ui, st, drops, None);
    }
}

fn install_drop_handler(ui: &AppWindow, st: &S) {
    use slint::winit_030::{EventResult, WinitWindowAccessor, winit};
    let st = st.clone();
    ui.window().on_winit_window_event(move |_, ev| {
        if let winit::event::WindowEvent::DroppedFile(p) = ev {
            st.borrow_mut().drops.push(p.clone());
        }
        EventResult::Propagate
    });
}

fn wire_callbacks(ui: &AppWindow, st: &S) {
    {
        let (w, st) = (ui.as_weak(), st.clone());
        ui.on_action(move |a| {
            if let Some(ui) = w.upgrade() {
                do_action(&ui, &st, &a);
            }
        });
    }
    {
        let (w, st) = (ui.as_weak(), st.clone());
        ui.on_row_clicked(move |i, ctrl, shift| {
            let Some(ui) = w.upgrade() else { return };
            let i = i as usize;
            {
                let mut s = st.borrow_mut();
                if i >= s.items.len() {
                    return;
                }
                s.status.clear();
                if shift {
                    let a = s.anchor.unwrap_or(i);
                    let (lo, hi) = (a.min(i), a.max(i));
                    if !ctrl {
                        s.selected.iter_mut().for_each(|x| *x = false);
                    }
                    for k in lo..=hi {
                        s.selected[k] = true;
                    }
                } else if ctrl {
                    s.selected[i] = !s.selected[i];
                    s.anchor = Some(i);
                } else {
                    s.selected.iter_mut().for_each(|x| *x = false);
                    s.selected[i] = true;
                    s.anchor = Some(i);
                }
            }
            sync_selection(&ui, &st);
            selection_changed(&ui, &st);
        });
    }
    {
        let (w, st) = (ui.as_weak(), st.clone());
        ui.on_row_double_clicked(move |i| {
            if let Some(ui) = w.upgrade() {
                activate(&ui, &st, i as usize);
            }
        });
    }
    {
        let (w, st) = (ui.as_weak(), st.clone());
        ui.on_sort(move |c| {
            let Some(ui) = w.upgrade() else { return };
            {
                let mut s = st.borrow_mut();
                if s.sort_col == c {
                    s.sort_asc = !s.sort_asc;
                } else {
                    s.sort_col = c;
                    s.sort_asc = true;
                }
            }
            refresh(&ui, &st);
        });
    }
    {
        let (w, st) = (ui.as_weak(), st.clone());
        ui.on_format_changed(move |i| {
            if let Some(ui) = w.upgrade() {
                apply_format(&ui, &st, i.max(0) as usize);
            }
        });
    }
    {
        let (w, st) = (ui.as_weak(), st.clone());
        ui.on_shell_toggled(move |on| {
            let Some(ui) = w.upgrade() else { return };
            let r = if on { shell::register() } else { shell::unregister() };
            let msg = match r {
                Ok(m) => m,
                Err(e) => format!("錯誤：{e}"),
            };
            ui.set_shell_enabled(shell::is_registered());
            set_status(&ui, &st, msg);
        });
    }
    {
        let (w, st) = (ui.as_weak(), st.clone());
        ui.on_key(move |k| {
            let Some(ui) = w.upgrade() else { return };
            match k.as_str() {
                "enter" => {
                    let idx = st.borrow().selected.iter().position(|&x| x);
                    if let Some(i) = idx {
                        activate(&ui, &st, i);
                    }
                }
                "up" => go_up(&ui, &st),
                "prev" => move_selection(&ui, &st, -1),
                "next" => move_selection(&ui, &st, 1),
                "delete" => {
                    if ui.get_can_delete() {
                        do_delete(&ui, &st);
                    }
                }
                _ => {}
            }
        });
    }
}

// ------------------------------------------------------------------ OS helpers

fn open_path(p: &Path) {
    let mut cmd = if cfg!(windows) {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", ""]).arg(p);
        c
    } else if cfg!(target_os = "macos") {
        let mut c = std::process::Command::new("open");
        c.arg(p);
        c
    } else {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(p);
        c
    };
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let _ = cmd.spawn();
}

fn confirm(title: &str, text: &str) -> bool {
    rfd::MessageDialog::new()
        .set_title(title)
        .set_description(text)
        .set_buttons(rfd::MessageButtons::YesNo)
        .show()
        == rfd::MessageDialogResult::Yes
}

fn info_box(title: &str, text: &str) {
    rfd::MessageDialog::new().set_title(title).set_description(text).set_level(rfd::MessageLevel::Info).show();
}

fn error_box(text: &str) {
    rfd::MessageDialog::new().set_title("Archiver - 錯誤").set_description(text).set_level(rfd::MessageLevel::Error).show();
}

#[cfg(test)]
mod tests {
    use super::*;

    use archiver_core::Error;

    fn r(input: &str, result: archiver_core::Result<()>) -> BatchResult {
        BatchResult { input: PathBuf::from(input), output: PathBuf::from("out"), result }
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
}

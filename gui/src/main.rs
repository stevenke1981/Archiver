// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod args;
mod shell;
mod view;

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use archiver_core::{CreateOptions, Entry, Format, Progress};
use args::Action;
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use view::Item;

slint::include_modules!();

/// Destination for "extract here" / "extract to folder".
pub fn extract_dest(archive: &Path, to_folder: bool) -> PathBuf {
    let dir = archive.parent().map(Path::to_path_buf).unwrap_or_default();
    if to_folder { dir.join(archiver_core::archive_stem(archive)) } else { dir }
}

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
    }));
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
        Action::Compress(files) => open_create(&ui, &st, files),
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

// ------------------------------------------------------------------ background jobs

fn spawn<F>(ui: &AppWindow, st: &S, title: &str, f: F)
where
    F: FnOnce(&Progress) -> Msg + Send + 'static,
{
    if st.borrow().job.is_some() {
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

fn poll(ui: &AppWindow, st: &S) {
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
                s.mode = Mode::Archive { path, fmt, entries, prefix: String::new() };
                s.filter.clear();
                s.status.clear();
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
            if st.borrow().close_when_done {
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
    let pw = opt_pw(&st.borrow());
    match archiver_core::list(&path, pw.as_deref()) {
        Ok(entries) => {
            if let Mode::Archive { entries: e, .. } = &mut st.borrow_mut().mode {
                *e = entries;
            }
            refresh(ui, st);
        }
        Err(e) => set_status(ui, st, format!("錯誤：{e}")),
    }
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

fn extract_batch(ui: &AppWindow, st: &S, files: Vec<PathBuf>, to_folder: bool) {
    let jobs: Vec<(PathBuf, PathBuf)> = files.into_iter().map(|f| {
        let d = extract_dest(&f, to_folder);
        (f, d)
    }).collect();
    {
        let mut s = st.borrow_mut();
        if jobs.len() == 1 {
            s.fallback_open = Some(jobs[0].0.clone());
        }
        s.close_when_done = true;
    }
    spawn(ui, st, "解壓縮中…", move |p| {
        for (arc, dest) in &jobs {
            if let Err(e) = archiver_core::extract(arc, dest, None, None, p) {
                return Msg::Done(Err(format!("{}: {e}", arc.display())), After::Nothing);
            }
        }
        Msg::Done(Ok("解壓縮完成".into()), After::Nothing)
    });
}

fn open_create(ui: &AppWindow, st: &S, files: Vec<PathBuf>) {
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
        let idx = ui.get_format_index().max(0) as usize;
        apply_format(ui, st, idx);
    }
    update_create_fields(ui, st);
    ui.set_dialog("create".into());
}

fn update_create_fields(ui: &AppWindow, st: &S) {
    let s = st.borrow();
    let text = s.create_inputs.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\n");
    ui.set_create_sources(text.into());
    let fmt = Format::writable()[ui.get_format_index().max(0) as usize];
    if !s.create_inputs.is_empty() {
        ui.set_create_output(archiver_core::suggest_output(&s.create_inputs, fmt).display().to_string().into());
    }
    ui.set_single_file_warning(fmt.is_single_file() && s.create_inputs.len() != 1);
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
    if inputs.is_empty() || output.as_os_str().is_empty() {
        return;
    }
    if output.exists() && !confirm("覆寫檔案？", &format!("{} 已存在，要覆寫嗎？", output.display())) {
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
        "add" => {
            let fs_sel: Vec<PathBuf> = {
                let s = st.borrow();
                match &s.mode {
                    Mode::Fs(_) => selected_items(&s).into_iter().map(|i| PathBuf::from(i.full)).collect(),
                    _ => vec![],
                }
            };
            if !fs_sel.is_empty() {
                open_create(ui, st, fs_sel);
            } else if let Some(f) = rfd::FileDialog::new().set_title("選擇要加入的檔案").pick_files() {
                open_create(ui, st, f);
            }
        }
        "extract" => do_extract(ui, st),
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
    spawn(ui, st, "解壓縮中…", move |p| {
        for a in &sources {
            let to = if sources.len() > 1 { dest.join(archiver_core::archive_stem(a)) } else { dest.clone() };
            if let Err(e) = archiver_core::extract(a, &to, pw.as_deref(), sel.as_deref(), p) {
                return Msg::Done(Err(format!("{}: {e}", a.display())), After::Nothing);
            }
        }
        Msg::Done(Ok(format!("已解壓縮到 {}", dest.display())), After::ReloadFs)
    });
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
        open_create(ui, st, drops);
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
            // update only the selection flags in the existing model
            let rows = ui.get_rows();
            let s = st.borrow();
            for (k, &sel) in s.selected.iter().enumerate() {
                if let Some(mut r) = rows.row_data(k) {
                    if r.selected != sel {
                        r.selected = sel;
                        rows.set_row_data(k, r);
                    }
                }
            }
            update_status(&ui, &s);
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

    #[test]
    fn extract_destinations() {
        let a = Path::new("/data/in/Photos 2024.tar.gz");
        assert_eq!(extract_dest(a, false), Path::new("/data/in"));
        assert_eq!(extract_dest(a, true), Path::new("/data/in/Photos 2024"));
        assert_eq!(extract_dest(Path::new("/x/a.ZIP"), true), Path::new("/x/a"));
    }
}

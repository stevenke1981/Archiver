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
        Action::ExtractTo(files) => extract_to(&ui, &st, files),
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


mod listing;
use listing::*;
mod jobs;
use jobs::*;
mod actions;
use actions::*;
mod clipboard_ops;
use clipboard_ops::*;
mod preview_pane;
use preview_pane::*;
mod callbacks;
use callbacks::*;
mod os;
use os::*;

#[cfg(test)]
mod tests;

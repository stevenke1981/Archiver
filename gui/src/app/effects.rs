//! Side effects of [`super::update`]: native dialogs, threads, clipboard, shell and config.
//! [`RealEffects`] is used by the application; tests use a recording implementation.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use acme_platform::Waker;
use archiver_core::Progress;

use super::model::PvSrc;
use super::msg::Msg;
use super::worker;
use crate::clipboard::Clip;

/// A background job: runs on a worker thread and reports back with a [`Msg`].
pub type JobFn = Box<dyn FnOnce(&Progress) -> Msg + Send + 'static>;

pub trait Effects {
    fn pick_archive(&mut self) -> Option<PathBuf>;
    fn pick_file(&mut self, title: &str) -> Option<PathBuf>;
    fn pick_files(&mut self, title: &str) -> Option<Vec<PathBuf>>;
    fn pick_folder(&mut self, title: &str, start: Option<&Path>) -> Option<PathBuf>;
    fn save_file(&mut self, title: &str, dir: Option<&Path>, name: Option<&str>) -> Option<PathBuf>;
    fn confirm(&mut self, title: &str, text: &str) -> bool;
    fn info(&mut self, title: &str, text: &str);
    fn error(&mut self, text: &str);
    fn open_path(&mut self, path: &Path);
    /// Start the background job (the model already marked itself busy).
    fn spawn(&mut self, job: JobFn);
    fn cancel_job(&mut self);
    fn start_preview(&mut self, generation: u64, src: PvSrc, name: String);
    fn cancel_preview(&mut self);
    fn copy_files(&mut self, paths: &[PathBuf]) -> Result<(), String>;
    fn copy_text(&mut self, text: &str) -> Result<(), String>;
    fn paste_text(&mut self) -> Option<String>;
    /// Register (`true`) or remove the shell integration.
    fn shell_set(&mut self, on: bool) -> Result<String, String>;
    fn shell_registered(&mut self) -> bool;
    fn favorites_load(&mut self) -> Vec<PathBuf>;
    fn favorites_add(&mut self, path: &Path) -> Result<(), String>;
    fn favorites_remove(&mut self, path: &Path) -> Result<(), String>;
    /// Delete a no longer needed staging folder in the background.
    fn remove_dir_later(&mut self, dir: PathBuf);
    /// Close the main window (quits the application).
    fn close(&mut self);
}

struct RunningJob {
    rx: Receiver<Msg>,
    cancel: Arc<AtomicBool>,
    progress: Arc<Mutex<(u64, u64, String)>>,
}

/// The production implementation: rfd dialogs, worker threads woken through the
/// platform [`Waker`], arboard clipboard, registry / config files.
pub struct RealEffects {
    pub waker: Option<Waker>,
    job: Option<RunningJob>,
    pv_tx: Sender<Msg>,
    pv_rx: Receiver<Msg>,
    pv_cancel: Arc<AtomicBool>,
    clip: Clip,
    pub closing: bool,
}

impl Default for RealEffects {
    fn default() -> Self {
        Self::new()
    }
}

impl RealEffects {
    pub fn new() -> Self {
        let (pv_tx, pv_rx) = channel();
        RealEffects { waker: None, job: None, pv_tx, pv_rx, pv_cancel: Arc::new(AtomicBool::new(false)), clip: Clip::new(), closing: false }
    }

    /// Finished background results (job and previews), oldest first.
    pub fn drain(&mut self) -> Vec<Msg> {
        let mut out: Vec<Msg> = self.pv_rx.try_iter().collect();
        if let Some(job) = &self.job
            && let Ok(m) = job.rx.try_recv()
        {
            self.job = None;
            out.push(m);
        }
        out
    }

    /// (done, total, current item) of the running job.
    pub fn progress(&self) -> Option<(u64, u64, String)> {
        self.job.as_ref().and_then(|j| j.progress.lock().ok().map(|g| g.clone()))
    }

    fn wake(&self) -> impl Fn() + Send + 'static {
        let w = self.waker.clone();
        move || {
            if let Some(w) = &w {
                w.wake();
            }
        }
    }
}

fn message(title: &str, text: &str, level: rfd::MessageLevel) {
    rfd::MessageDialog::new().set_title(title).set_description(text).set_level(level).show();
}

impl Effects for RealEffects {
    fn pick_archive(&mut self) -> Option<PathBuf> {
        let exts = archiver_core::archive_extensions();
        rfd::FileDialog::new().set_title("開啟壓縮檔").add_filter("壓縮檔", &exts).add_filter("所有檔案", &["*"]).pick_file()
    }
    fn pick_file(&mut self, title: &str) -> Option<PathBuf> {
        rfd::FileDialog::new().set_title(title).pick_file()
    }
    fn pick_files(&mut self, title: &str) -> Option<Vec<PathBuf>> {
        rfd::FileDialog::new().set_title(title).pick_files()
    }
    fn pick_folder(&mut self, title: &str, start: Option<&Path>) -> Option<PathBuf> {
        let mut d = rfd::FileDialog::new().set_title(title);
        if let Some(s) = start.filter(|s| s.is_dir()) {
            d = d.set_directory(s);
        }
        d.pick_folder()
    }
    fn save_file(&mut self, title: &str, dir: Option<&Path>, name: Option<&str>) -> Option<PathBuf> {
        let mut d = rfd::FileDialog::new().set_title(title);
        if let Some(dir) = dir.filter(|p| p.is_dir()) {
            d = d.set_directory(dir);
        }
        if let Some(n) = name {
            d = d.set_file_name(n);
        }
        d.save_file()
    }
    fn confirm(&mut self, title: &str, text: &str) -> bool {
        rfd::MessageDialog::new().set_title(title).set_description(text).set_buttons(rfd::MessageButtons::YesNo).show() == rfd::MessageDialogResult::Yes
    }
    fn info(&mut self, title: &str, text: &str) {
        message(title, text, rfd::MessageLevel::Info);
    }
    fn error(&mut self, text: &str) {
        message("Archiver - 錯誤", text, rfd::MessageLevel::Error);
    }
    fn open_path(&mut self, path: &Path) {
        if let Err(e) = crate::os::open_path(path) {
            self.error(&e);
        }
    }
    fn spawn(&mut self, job: JobFn) {
        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(Mutex::new((0u64, 0u64, String::new())));
        let (c2, p2, wake) = (cancel.clone(), progress.clone(), self.wake());
        std::thread::spawn(move || {
            let cb = move |d: u64, t: u64, n: &str| {
                if let Ok(mut g) = p2.lock() {
                    *g = (d, t, n.to_string());
                }
            };
            let prog = Progress { on: &cb, cancel: c2 };
            let _ = tx.send(job(&prog));
            wake();
        });
        self.job = Some(RunningJob { rx, cancel, progress });
    }
    fn cancel_job(&mut self) {
        if let Some(j) = &self.job {
            j.cancel.store(true, Ordering::Relaxed);
        }
    }
    fn start_preview(&mut self, generation: u64, src: PvSrc, name: String) {
        let cancel = Arc::new(AtomicBool::new(false));
        self.pv_cancel = cancel.clone();
        let (tx, wake) = (self.pv_tx.clone(), self.wake());
        std::thread::spawn(move || {
            let done = worker::preview_worker(generation, src, &name, cancel);
            let _ = tx.send(Msg::PreviewDone(done));
            wake();
        });
    }
    fn cancel_preview(&mut self) {
        self.pv_cancel.store(true, Ordering::Relaxed);
    }
    fn copy_files(&mut self, paths: &[PathBuf]) -> Result<(), String> {
        self.clip.copy_files(paths)
    }
    fn copy_text(&mut self, text: &str) -> Result<(), String> {
        self.clip.copy_text(text)
    }
    fn paste_text(&mut self) -> Option<String> {
        self.clip.paste_text()
    }
    fn shell_set(&mut self, on: bool) -> Result<String, String> {
        let r = if on { crate::shell::register() } else { crate::shell::unregister() };
        r.map_err(|e| e.to_string())
    }
    fn shell_registered(&mut self) -> bool {
        crate::shell::is_registered()
    }
    fn favorites_load(&mut self) -> Vec<PathBuf> {
        crate::favorites::load()
    }
    fn favorites_add(&mut self, path: &Path) -> Result<(), String> {
        crate::favorites::add(path).map_err(|e| e.to_string())
    }
    fn favorites_remove(&mut self, path: &Path) -> Result<(), String> {
        crate::favorites::remove(path).map_err(|e| e.to_string())
    }
    fn remove_dir_later(&mut self, dir: PathBuf) {
        std::thread::spawn(move || std::fs::remove_dir_all(dir));
    }
    fn close(&mut self) {
        self.closing = true;
    }
}

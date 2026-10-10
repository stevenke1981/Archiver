// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};

use archiver_core::{CreateOptions, Entry, Format, Progress};
use eframe::egui;

mod args;
mod shell;

use args::Action;

fn main() -> eframe::Result {
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
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_drag_and_drop(true)
            .with_title("Rust Archiver")
            .with_inner_size([960.0, 640.0])
            .with_min_inner_size([640.0, 400.0]),
        ..Default::default()
    };
    eframe::run_native(
        "rust-archiver",
        options,
        Box::new(move |cc| {
            install_fonts(&cc.egui_ctx);
            let mut app = App::default();
            app.shell_registered = shell::is_registered();
            app.startup(action);
            Ok(Box::new(app))
        }),
    )
}

/// Add a system CJK font as fallback so Chinese/Japanese file names render.
fn install_fonts(ctx: &egui::Context) {
    let mut c: Vec<PathBuf> = Vec::new();
    if let Some(p) = std::env::var_os("RUST_ARCHIVER_FONT") {
        c.push(p.into());
    }
    if cfg!(windows) {
        let w = std::env::var_os("WINDIR").map(PathBuf::from).unwrap_or_else(|| "C:/Windows".into());
        c.extend([w.join("Fonts/msjh.ttc"), w.join("Fonts/msyh.ttc"), w.join("Fonts/YuGothR.ttc")]);
    } else if cfg!(target_os = "macos") {
        c.extend(["/System/Library/Fonts/PingFang.ttc", "/System/Library/Fonts/STHeiti Light.ttc"].map(PathBuf::from));
    } else {
        c.extend(
            [
                "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
                "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
                "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
                "/usr/share/fonts/wenquanyi/wqy-microhei/wqy-microhei.ttc",
            ]
            .map(PathBuf::from),
        );
    }
    let mut fonts = egui::FontDefinitions::default();
    for p in c {
        if let Ok(bytes) = std::fs::read(&p) {
            fonts.font_data.insert("cjk".into(), egui::FontData::from_owned(bytes).into());
            for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts.families.entry(fam).or_default().push("cjk".into());
            }
            break;
        }
    }
    ctx.set_fonts(fonts);
}

enum Msg {
    Listed(PathBuf, Result<Vec<Entry>, String>),
    Done(Result<String, String>),
}

struct Job {
    title: String,
    rx: Receiver<Msg>,
    cancel: Arc<AtomicBool>,
    progress: Arc<Mutex<(u64, u64, String)>>,
}

struct CreateDlg {
    open: bool,
    inputs: Vec<PathBuf>,
    format: Format,
    level: u32,
    password: String,
    output: String,
}

impl Default for CreateDlg {
    fn default() -> Self {
        Self { open: false, inputs: vec![], format: Format::Zip, level: 6, password: String::new(), output: String::new() }
    }
}

#[derive(Default)]
struct App {
    archive: Option<PathBuf>,
    entries: Vec<Entry>,
    selected: BTreeSet<usize>,
    password: String,
    status: String,
    job: Option<Job>,
    create: CreateDlg,
    filter: String,
    /// Close the window after a context-menu extraction succeeds.
    close_when_done: bool,
    close_now: bool,
    shell_window: bool,
    shell_registered: bool,
    /// Archive to open in the GUI if a context-menu extraction fails (e.g. needs a password).
    fallback_open: Option<PathBuf>,
}

/// Destination for "extract here" / "extract to folder".
pub fn extract_dest(archive: &Path, to_folder: bool) -> PathBuf {
    let dir = archive.parent().map(Path::to_path_buf).unwrap_or_default();
    if to_folder { dir.join(archiver_core::archive_stem(archive)) } else { dir }
}

fn human(n: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < 4 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{n} B") } else { format!("{v:.1} {}", U[i]) }
}

impl App {
    fn busy(&self) -> bool {
        self.job.is_some()
    }

    fn spawn<F>(&mut self, title: &str, f: F)
    where
        F: FnOnce(&Progress) -> Msg + Send + 'static,
    {
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
        self.job = Some(Job { title: title.into(), rx, cancel, progress });
    }

    fn startup(&mut self, action: Action) {
        match action {
            Action::Open(p) => self.open_archive(p),
            Action::Compress(files) => self.add_inputs(files),
            Action::ExtractHere(files) => self.extract_batch(files, false),
            Action::ExtractToFolder(files) => self.extract_batch(files, true),
            _ => {}
        }
    }

    /// Extract several archives next to themselves (context-menu actions).
    fn extract_batch(&mut self, files: Vec<PathBuf>, to_folder: bool) {
        let jobs: Vec<(PathBuf, PathBuf)> = files.into_iter().map(|f| {
            let d = extract_dest(&f, to_folder);
            (f, d)
        }).collect();
        if jobs.len() == 1 {
            self.fallback_open = Some(jobs[0].0.clone());
        }
        self.close_when_done = true;
        self.spawn("解壓縮中 / Extracting", move |p| {
            let mut ok = Vec::new();
            for (arc, dest) in &jobs {
                if let Err(e) = archiver_core::extract(arc, dest, None, None, p) {
                    return Msg::Done(Err(format!("{}: {e}", arc.display())));
                }
                ok.push(dest.display().to_string());
            }
            Msg::Done(Ok(format!("已解壓縮到 / Extracted to {}", ok.join(", "))))
        });
    }

    fn open_archive(&mut self, path: PathBuf) {
        if Format::detect(&path).is_none() {
            self.status = format!("不支援的格式 / Unsupported format: {}", path.display());
            return;
        }
        let pw = Some(self.password.clone()).filter(|s| !s.is_empty());
        self.spawn("讀取中 / Reading", move |_| {
            let r = archiver_core::list(&path, pw.as_deref()).map_err(|e| e.to_string());
            Msg::Listed(path, r)
        });
    }

    fn extract(&mut self, only_selected: bool) {
        let Some(arc) = self.archive.clone() else { return };
        let Some(dest) = rfd::FileDialog::new().set_title("解壓縮到 / Extract to").pick_folder() else { return };
        let sel: Option<Vec<String>> = only_selected.then(|| self.selected.iter().map(|&i| self.entries[i].name.clone()).collect());
        let pw = Some(self.password.clone()).filter(|s| !s.is_empty());
        self.spawn("解壓縮中 / Extracting", move |p| {
            let r = archiver_core::extract(&arc, &dest, pw.as_deref(), sel.as_deref(), p)
                .map(|_| format!("已解壓縮到 / Extracted to {}", dest.display()))
                .map_err(|e| e.to_string());
            Msg::Done(r)
        });
    }

    fn start_create(&mut self) {
        let d = &self.create;
        let output = PathBuf::from(d.output.trim());
        let opts = CreateOptions {
            format: d.format,
            level: d.format.level_range().map(|_| d.level),
            password: Some(d.password.clone()).filter(|s| !s.is_empty() && d.format.supports_password()),
        };
        let inputs = d.inputs.clone();
        self.create.open = false;
        self.spawn("壓縮中 / Compressing", move |p| {
            let r = archiver_core::create(&output, &inputs, &opts, p)
                .map(|_| format!("已建立 / Created {}", output.display()))
                .map_err(|e| e.to_string());
            Msg::Done(r)
        });
    }

    fn set_format(&mut self, f: Format) {
        self.create.format = f;
        if let Some((lo, hi, def)) = f.level_range() {
            if !(lo..=hi).contains(&self.create.level) {
                self.create.level = def;
            }
        }
        self.refresh_output();
    }

    fn refresh_output(&mut self) {
        if !self.create.inputs.is_empty() {
            self.create.output = archiver_core::suggest_output(&self.create.inputs, self.create.format).display().to_string();
        }
    }

    fn add_inputs(&mut self, paths: Vec<PathBuf>) {
        for p in paths {
            if !self.create.inputs.contains(&p) {
                self.create.inputs.push(p);
            }
        }
        self.create.open = true;
        self.refresh_output();
    }

    fn poll(&mut self) {
        let Some(job) = &self.job else { return };
        if let Ok(msg) = job.rx.try_recv() {
            self.job = None;
            match msg {
                Msg::Listed(p, Ok(e)) => {
                    self.status = format!("{} — {} 個項目 / entries", p.display(), e.len());
                    self.entries = e;
                    self.selected.clear();
                    self.archive = Some(p);
                }
                Msg::Listed(p, Err(e)) => self.status = format!("無法開啟 / Cannot open {}: {e}", p.display()),
                Msg::Done(Ok(s)) => {
                    self.status = s;
                    if self.close_when_done {
                        self.close_now = true;
                    }
                    if let Some(a) = self.archive.clone() {
                        if !a.exists() {
                            self.archive = None;
                            self.entries.clear();
                        }
                    }
                }
                Msg::Done(Err(e)) => {
                    self.status = format!("錯誤 / Error: {e}");
                    self.close_when_done = false;
                    if let Some(p) = self.fallback_open.take() {
                        // e.g. encrypted archive: open it so the user can type a password
                        self.open_archive(p);
                        self.status = format!("錯誤 / Error: {e} — 請輸入密碼後再解壓 / enter password and extract");
                    }
                }
            }
        }
    }

    fn handle_drops(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| p.is_absolute()).collect());
        if dropped.is_empty() || self.busy() {
            return;
        }
        if dropped.len() == 1 && dropped[0].is_file() && Format::detect(&dropped[0]).is_some() && !self.create.open {
            self.open_archive(dropped[0].clone());
        } else {
            self.add_inputs(dropped);
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.add_enabled_ui(!self.busy(), |ui| {
                if ui.button("📂 開啟 Open").clicked() {
                    let exts: Vec<&str> = vec!["zip", "7z", "rar", "tar", "gz", "tgz", "bz2", "tbz2", "xz", "txz", "zst", "tzst", "jar"];
                    if let Some(p) = rfd::FileDialog::new().add_filter("Archives", &exts).add_filter("All", &["*"]).pick_file() {
                        self.open_archive(p);
                    }
                }
                let has = self.archive.is_some();
                if ui.add_enabled(has, egui::Button::new("📤 全部解壓 Extract all")).clicked() {
                    self.extract(false);
                }
                if ui.add_enabled(has && !self.selected.is_empty(), egui::Button::new("📑 解壓選取 Extract selected")).clicked() {
                    self.extract(true);
                }
                ui.separator();
                if ui.button("🗜 新增檔案壓縮 Add files…").clicked() {
                    if let Some(f) = rfd::FileDialog::new().pick_files() {
                        self.add_inputs(f);
                    }
                }
                if ui.button("📁 新增資料夾壓縮 Add folder…").clicked() {
                    if let Some(f) = rfd::FileDialog::new().pick_folder() {
                        self.add_inputs(vec![f]);
                    }
                }
                if ui.button("⚙ 右鍵選單 Shell menu").clicked() {
                    self.shell_registered = shell::is_registered();
                    self.shell_window = true;
                }
                ui.separator();
                ui.label("🔑 密碼 Password:");
                ui.add(egui::TextEdit::singleline(&mut self.password).password(true).desired_width(120.0));
                if let Some(a) = self.archive.clone() {
                    if ui.button("↻").on_hover_text("以此密碼重新讀取 / Reload with password").clicked() {
                        self.open_archive(a);
                    }
                }
            });
        });
    }

    fn table(&mut self, ui: &mut egui::Ui) {
        if self.archive.is_none() {
            ui.centered_and_justified(|ui| {
                ui.heading("拖曳壓縮檔到這裡開啟，或拖曳檔案/資料夾建立新壓縮檔\nDrop an archive here to open it, or drop files/folders to create one");
            });
            return;
        }
        ui.horizontal(|ui| {
            ui.label("🔍");
            ui.text_edit_singleline(&mut self.filter);
            if ui.button("全選 Select all").clicked() {
                self.selected = (0..self.entries.len()).collect();
            }
            if ui.button("取消選取 Clear").clicked() {
                self.selected.clear();
            }
            ui.label(format!("已選 {} selected", self.selected.len()));
        });
        ui.separator();
        let filter = self.filter.to_lowercase();
        let rows: Vec<usize> =
            (0..self.entries.len()).filter(|&i| filter.is_empty() || self.entries[i].name.to_lowercase().contains(&filter)).collect();
        egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
            egui::Grid::new("entries").striped(true).num_columns(4).min_col_width(80.0).show(ui, |ui| {
                ui.strong("名稱 Name");
                ui.strong("大小 Size");
                ui.strong("壓縮後 Packed");
                ui.strong("修改時間 Modified");
                ui.end_row();
                for i in rows {
                    let e = &self.entries[i];
                    let icon = if e.is_dir { "📁" } else { "📄" };
                    let sel = self.selected.contains(&i);
                    if ui.selectable_label(sel, format!("{icon} {}", e.name)).clicked() {
                        let multi = ui.input(|i| i.modifiers.command || i.modifiers.shift);
                        if !multi {
                            self.selected.clear();
                            self.selected.insert(i);
                        } else if sel {
                            self.selected.remove(&i);
                        } else {
                            self.selected.insert(i);
                        }
                    }
                    ui.label(if e.is_dir { String::new() } else { human(e.size) });
                    ui.label(e.compressed.map(human).unwrap_or_else(|| "—".into()));
                    ui.label(e.modified.clone().unwrap_or_default());
                    ui.end_row();
                }
            });
        });
    }

    fn create_window(&mut self, ctx: &egui::Context) {
        if !self.create.open {
            return;
        }
        let mut open = true;
        let mut go = false;
        egui::Window::new("建立壓縮檔 / Create archive").open(&mut open).default_width(520.0).show(ctx, |ui| {
            ui.label("來源 / Sources (可拖曳更多檔案進來 / drop more files here):");
            let mut remove = None;
            egui::ScrollArea::vertical().max_height(140.0).show(ui, |ui| {
                for (i, p) in self.create.inputs.iter().enumerate() {
                    ui.horizontal(|ui| {
                        if ui.small_button("✖").clicked() {
                            remove = Some(i);
                        }
                        ui.label(p.display().to_string());
                    });
                }
            });
            if let Some(i) = remove {
                self.create.inputs.remove(i);
                self.refresh_output();
            }
            ui.separator();
            let mut fmt = self.create.format;
            egui::ComboBox::from_label("格式 Format").selected_text(fmt.label()).show_ui(ui, |ui| {
                for f in Format::writable() {
                    ui.selectable_value(&mut fmt, f, f.label());
                }
            });
            if fmt != self.create.format {
                self.set_format(fmt);
            }
            if let Some((lo, hi, _)) = self.create.format.level_range() {
                ui.add(egui::Slider::new(&mut self.create.level, lo..=hi).text("壓縮等級 Level"));
            } else {
                ui.label("（此格式不壓縮 / no compression for this format）");
            }
            if self.create.format.supports_password() {
                ui.horizontal(|ui| {
                    ui.label("密碼 Password (AES-256):");
                    ui.add(egui::TextEdit::singleline(&mut self.create.password).password(true));
                });
            }
            if self.create.format.is_single_file() {
                ui.colored_label(egui::Color32::from_rgb(200, 150, 0), "此格式僅能壓縮單一檔案 / single file only");
            }
            ui.horizontal(|ui| {
                ui.label("輸出 Output:");
                ui.add(egui::TextEdit::singleline(&mut self.create.output).desired_width(330.0));
                if ui.button("…").clicked() {
                    let cur = PathBuf::from(&self.create.output);
                    let mut d = rfd::FileDialog::new();
                    if let Some(dir) = cur.parent().filter(|p| p.is_dir()) {
                        d = d.set_directory(dir);
                    }
                    if let Some(n) = cur.file_name() {
                        d = d.set_file_name(n.to_string_lossy());
                    }
                    if let Some(p) = d.save_file() {
                        self.create.output = p.display().to_string();
                    }
                }
            });
            ui.separator();
            let ok = !self.create.inputs.is_empty() && !self.create.output.trim().is_empty();
            if ui.add_enabled(ok && self.job.is_none(), egui::Button::new("🗜 開始壓縮 Create")).clicked() {
                go = true;
            }
        });
        if !open {
            self.create.open = false;
            self.create.inputs.clear();
        }
        if go {
            if Path::new(self.create.output.trim()).exists() {
                let r = rfd::MessageDialog::new()
                    .set_title("覆寫? / Overwrite?")
                    .set_description("輸出檔已存在，要覆寫嗎？ / Output exists. Overwrite?")
                    .set_buttons(rfd::MessageButtons::YesNo)
                    .show();
                if r != rfd::MessageDialogResult::Yes {
                    return;
                }
            }
            self.start_create();
            self.create.inputs.clear();
        }
    }
}

impl App {
    fn shell_settings(&mut self, ctx: &egui::Context) {
        if !self.shell_window {
            return;
        }
        let mut open = true;
        egui::Window::new("右鍵選單整合 / Context menu integration").open(&mut open).show(ctx, |ui| {
            let what = if cfg!(windows) {
                "Windows 檔案總管（目前使用者 HKCU，不需系統管理員）\nWindows Explorer (current user, no admin)"
            } else if cfg!(target_os = "macos") {
                "Finder 快速動作（~/Library/Services）\nFinder Quick Actions"
            } else {
                "Nautilus / Dolphin / Nemo + .desktop MIME 關聯\nLinux file managers"
            };
            ui.label(what);
            ui.separator();
            let mut on = self.shell_registered;
            if ui.checkbox(&mut on, "啟用右鍵選單 / Enable context menu").changed() {
                let r = if on { shell::register() } else { shell::unregister() };
                self.status = match r {
                    Ok(m) => m,
                    Err(e) => format!("錯誤 / Error: {e}"),
                };
                self.shell_registered = shell::is_registered();
            }
            ui.small("若移動了程式位置，請重新啟用一次 / Re-enable after moving the executable.");
        });
        if !open {
            self.shell_window = false;
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll();
        self.handle_drops(&ctx);

        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.add_space(4.0);
            self.toolbar(ui);
            ui.add_space(2.0);
        });
        egui::Panel::bottom("status").show(ui, |ui| {
            if let Some(job) = &self.job {
                let (d, t, n) = job.progress.lock().map(|g| g.clone()).unwrap_or_default();
                let frac = if t > 0 { (d as f32 / t as f32).min(1.0) } else { 0.0 };
                ui.horizontal(|ui| {
                    ui.label(&job.title);
                    ui.add(egui::ProgressBar::new(frac).show_percentage().desired_width(300.0).animate(t == 0));
                    if ui.button("取消 Cancel").clicked() {
                        job.cancel.store(true, Ordering::Relaxed);
                    }
                    ui.label(n);
                });
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
            } else {
                ui.label(&self.status);
            }
        });
        egui::CentralPanel::default().show(ui, |ui| self.table(ui));
        self.create_window(&ctx);
        self.shell_settings(&ctx);
        if self.close_now {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        if ctx.input(|i| !i.raw.hovered_files.is_empty()) {
            let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("drop")));
            let rect = ctx.content_rect();
            painter.rect_filled(rect, 0.0, egui::Color32::from_black_alpha(160));
            painter.text(rect.center(), egui::Align2::CENTER_CENTER, "放開以加入 / Drop to add", egui::FontId::proportional(28.0), egui::Color32::WHITE);
        }
    }
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

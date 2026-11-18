// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod args;
mod clipboard;
mod collect;
mod favorites;
mod os;
mod preview;
mod scan;
mod shell;
mod ui;
mod view;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use acme_core::{ClickDetector, CursorHint, Point, PointerOutcome, Scene};
use acme_platform::{Application, CursorIcon, FrameContext, PlatformEvent, WindowConfig, WindowId, WindowRequest};
use acme_theme::Theme;
use app::effects::RealEffects;
use app::update::{self as upd};
use app::{DlgMsg, Effects, KeyPress, Model, Msg, Popup, ROW_H, update};
use args::Action;
use ui::{Hit, Ui, geometry};

const DROP_SETTLE: Duration = Duration::from_millis(120);
const POLL: Duration = Duration::from_millis(50);
/// How long closing the window waits for a cancelled job to clean up.
const JOB_STOP_WAIT: Duration = Duration::from_secs(3);

/// The window: model + effects + painter state.
struct App {
    m: Model,
    fx: RealEffects,
    ui: Ui,
    detector: ClickDetector,
    started: Instant,
    requests: Vec<WindowRequest>,
    cursor: CursorHint,
    title: String,
    ime_allowed: bool,
    size: (f32, f32),
    drops: Vec<PathBuf>,
    drop_due: Option<Instant>,
    /// Pointer y at the start of a list scrollbar drag.
    drag_last: Option<f32>,
    closed: bool,
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from)
}

fn theme() -> Theme {
    if os::prefers_dark() { Theme::dark() } else { Theme::light() }
}

impl App {
    fn new(start: PathBuf) -> Self {
        let mut m = Model::new(start);
        m.scan_available = scan::available();
        let mut fx = RealEffects::new();
        m.favorites = fx.favorites_load();
        upd::refresh(&mut m, &mut fx);
        App {
            m,
            fx,
            ui: Ui::new(theme()),
            detector: ClickDetector::new(),
            started: Instant::now(),
            requests: vec![],
            cursor: CursorHint::Default,
            title: String::new(),
            ime_allowed: false,
            size: (1100.0, 720.0),
            drops: vec![],
            drop_due: None,
            drag_last: None,
            closed: false,
        }
    }

    fn send(&mut self, msg: Msg) {
        update(&mut self.m, msg, &mut self.fx);
        self.after_update();
    }

    /// Window requests that follow from the model (title, IME, close).
    fn after_update(&mut self) {
        let title = self.m.window_title();
        if title != self.title {
            self.title = title.clone();
            self.requests.push(WindowRequest::SetTitle(title));
        }
        let ime = self.m.text_focus();
        if ime != self.ime_allowed {
            self.ime_allowed = ime;
            self.requests.push(WindowRequest::SetImeAllowed(ime));
        }
        if self.fx.closing && !self.closed {
            self.closed = true;
            // stop a running job first so it can remove partial output
            self.fx.cancel_and_wait(JOB_STOP_WAIT);
            self.requests.push(WindowRequest::Close);
        }
        self.m.pv.scroll = self.m.pv.scroll.min(self.ui.pv_max_scroll);
    }

    fn sync_cursor(&mut self) {
        let next = self.detector.cursor();
        if next != self.cursor {
            self.cursor = next;
            self.requests.push(WindowRequest::SetCursor(match next {
                CursorHint::Pointer => CursorIcon::Pointer,
                CursorHint::Text => CursorIcon::Text,
                CursorHint::NotAllowed => CursorIcon::NotAllowed,
                CursorHint::ResizeEW => CursorIcon::ResizeHorizontal,
                CursorHint::ResizeNS => CursorIcon::ResizeVertical,
                CursorHint::Grab => CursorIcon::Grab,
                CursorHint::Grabbing => CursorIcon::Grabbing,
                CursorHint::Default => CursorIcon::Default,
            }));
        }
    }

    fn level_at(x: f32, tx: f32, w: f32, lo: u32, hi: u32) -> u32 {
        let f = ((x - tx) / w.max(1.0)).clamp(0.0, 1.0);
        lo + (f * (hi - lo) as f32).round() as u32
    }

    fn pressed(&mut self, id: acme_core::NodeId, button: u16, x: f32, y: f32, ctrl: bool, shift: bool) {
        let Some(hit) = self.ui.hit(id).cloned() else { return };
        match hit {
            Hit::Row(index) if button == 0 => self.send(Msg::RowPressed { index, ctrl, shift }),
            Hit::Row(index) if button == 1 => self.send(Msg::RowContext { index, x, y }),
            Hit::ListBlank if button == 0 && !ctrl && !shift => self.send(Msg::ClearSelection),
            Hit::MenuTitle(i) if button == 0 => self.send(Msg::OpenMenu(i)),
            Hit::Dismiss => {
                let title = self.ui.menu_title_at(x, y);
                match (title, self.m.popup) {
                    (Some(i), Some(Popup::Menu(j))) if i != j => self.send(Msg::OpenMenu(i)),
                    _ => self.send(Msg::ClosePopup),
                }
            }
            Hit::Field(f, r) if button == 0 => {
                let off = self.m.input(f).map(|s| self.ui.offset_at(s, r, x));
                self.send(Msg::Dialog(DlgMsg::Focus(f, off)));
            }
            Hit::LevelTrack { x: tx, w, lo, hi } if button == 0 => self.send(Msg::Dialog(DlgMsg::SetLevel(Self::level_at(x, tx, w, lo, hi)))),
            Hit::ListScroll { track_y, track_h } if button == 0 => {
                let content = self.m.items.len() as f32 * ROW_H;
                let view = self.m.list_view_h;
                let thumb = (view * view / content.max(1.0)).max(24.0);
                let f = ((y - track_y - thumb / 2.0) / (track_h - thumb).max(1.0)).clamp(0.0, 1.0);
                self.send(Msg::ScrollListTo(f * (content - view).max(0.0)));
                self.drag_last = Some(y);
            }
            _ => {}
        }
    }

    fn clicked(&mut self, id: acme_core::NodeId, button: u16, count: u32) {
        let Some(hit) = self.ui.hit(id).cloned() else { return };
        match hit {
            Hit::Click(msg) if button == 0 => self.send(msg),
            Hit::Row(i) if button == 0 && count >= 2 && count.is_multiple_of(2) => self.send(Msg::RowActivated(i)),
            _ => {}
        }
    }

    fn dragged(&mut self, id: acme_core::NodeId, x: f32, y: f32) {
        match self.ui.hit(id).cloned() {
            Some(Hit::LevelTrack { x: tx, w, lo, hi }) => self.send(Msg::Dialog(DlgMsg::SetLevel(Self::level_at(x, tx, w, lo, hi)))),
            Some(Hit::ListScroll { track_h, .. }) => {
                let last = self.drag_last.unwrap_or(y);
                let content = self.m.items.len() as f32 * ROW_H;
                let ratio = content / track_h.max(1.0);
                self.send(Msg::ScrollList((y - last) * ratio));
                self.drag_last = Some(y);
            }
            _ => {}
        }
    }

    fn frame_scene(&mut self, w: f32, h: f32, scale: f32) -> Scene {
        self.size = (w, h);
        let g = geometry(w, h, self.m.preview_visible);
        self.m.list_view_h = g.list.size.height.get();
        self.m.clamp_list_scroll();
        self.ui.pressed = self.detector.pressed();
        self.ui.paint(&self.m, w, h, scale)
    }
}

impl Application for App {
    fn window_config(&self) -> WindowConfig {
        WindowConfig { title: "Archiver".into(), width: 1100.0, height: 700.0, min_width: Some(760.0), min_height: Some(480.0), ..WindowConfig::default() }
    }

    fn event(&mut self, event: PlatformEvent) -> bool {
        match event {
            PlatformEvent::Resized { .. } => true,
            PlatformEvent::PointerMoved { x, y, .. } => {
                let before = (self.detector.hovered(), self.ui.pointer);
                self.ui.pointer = Some((x, y));
                let outcomes = self.detector.on_pointer_move(&self.ui.layer, Point::new(x, y));
                for o in &outcomes {
                    if let PointerOutcome::DragMove { id, current_point, .. } | PointerOutcome::DragStart { id, current_point, .. } = o {
                        self.dragged(*id, current_point.x.get(), current_point.y.get());
                    }
                }
                if let Some(Popup::Menu(open)) = self.m.popup
                    && let Some(i) = self.ui.menu_title_at(x, y)
                    && i != open
                {
                    self.send(Msg::HoverMenu(i));
                }
                self.sync_cursor();
                // hover effects are geometric: repaint when the hovered region changes
                before.0 != self.detector.hovered() || before.1.is_none() || !outcomes.is_empty() || self.detector.is_dragging()
            }
            PlatformEvent::PointerLeft { .. } | PlatformEvent::CursorLeft { .. } => {
                self.ui.pointer = None;
                self.detector.on_pointer_leave();
                self.sync_cursor();
                true
            }
            PlatformEvent::PointerButton { pressed, x, y, button, shift, ctrl, .. } => {
                let t = self.started.elapsed().as_secs_f64();
                let outcomes = self.detector.on_pointer_button(&self.ui.layer, button, pressed, Point::new(x, y), t);
                if pressed && outcomes.iter().all(|o| !matches!(o, PointerOutcome::Pressed { .. })) && self.m.popup.is_some() {
                    self.send(Msg::ClosePopup);
                }
                for o in outcomes {
                    match o {
                        PointerOutcome::Pressed { id, button, .. } => self.pressed(id, button, x, y, ctrl, shift),
                        PointerOutcome::Clicked { id, button, count, .. } => self.clicked(id, button, count),
                        _ => {}
                    }
                }
                if !pressed {
                    self.drag_last = None;
                    let _ = self.detector.on_pointer_move(&self.ui.layer, Point::new(x, y));
                }
                self.sync_cursor();
                true
            }
            PlatformEvent::Scroll { delta_y, lines, x, y, .. } => {
                if self.m.popup.is_some() || self.m.dialog.is_some() {
                    return false;
                }
                let g = geometry(self.size.0, self.size.1, self.m.preview_visible);
                let pt = Point::new(x, y);
                let d = if lines { -delta_y / 32.0 * 3.0 * ROW_H } else { -delta_y };
                if g.list.contains(pt) || g.header.contains(pt) {
                    self.send(Msg::ScrollList(d));
                } else if g.preview.is_some_and(|r| r.contains(pt)) {
                    self.send(Msg::ScrollPreview(d));
                } else {
                    return false;
                }
                true
            }
            PlatformEvent::Key { key, pressed: true, shift, ctrl, alt, text, character, .. } => {
                self.send(Msg::Key(KeyPress { key, ctrl, shift, alt, text, character }));
                true
            }
            PlatformEvent::ImePreedit { text, cursor, .. } => {
                self.send(Msg::Preedit(text, cursor));
                true
            }
            PlatformEvent::ImeCommit { text, .. } => {
                self.send(Msg::Text(text));
                true
            }
            PlatformEvent::FileDropped { paths, .. } => {
                self.drops.extend(paths.into_iter().map(PathBuf::from));
                self.drop_due = Some(Instant::now() + DROP_SETTLE);
                false
            }
            _ => false,
        }
    }

    fn close_requested(&mut self, _window: WindowId) -> bool {
        if self.m.busy() {
            if !self.fx.confirm("結束 Archiver", "背景工作仍在進行中，確定要取消工作並結束嗎？") {
                return false;
            }
            self.fx.cancel_and_wait(JOB_STOP_WAIT);
        }
        true
    }

    fn on_waker(&mut self, waker: acme_platform::Waker) {
        self.fx.waker = Some(waker);
    }

    fn wake(&mut self) -> bool {
        let msgs = self.fx.drain();
        let any = !msgs.is_empty();
        for m in msgs {
            self.send(m);
        }
        any
    }

    fn take_window_requests(&mut self, _window: WindowId) -> Vec<WindowRequest> {
        std::mem::take(&mut self.requests)
    }

    fn ime_cursor_area(&self, _window: WindowId) -> Option<[f32; 4]> {
        self.ui.ime_rect
    }

    fn on_gpu_recovered(&mut self, _window: WindowId) {
        self.ui.text.reset();
    }

    fn background_poll_interval(&self) -> Option<Duration> {
        (self.m.job.is_some() || self.m.pv.pending.is_some() || self.drop_due.is_some()).then_some(POLL)
    }

    fn background_tick(&mut self) -> bool {
        let now = Instant::now();
        let mut redraw = false;
        if let Some(job) = &mut self.m.job
            && let Some((done, total, name)) = self.fx.progress()
        {
            job.done = done;
            job.total = total;
            job.name = name;
            redraw = true;
        }
        if self.drop_due.is_some_and(|d| now >= d) {
            self.drop_due = None;
            let drops = std::mem::take(&mut self.drops);
            self.send(Msg::Dropped(drops));
            redraw = true;
        }
        if self.m.pv.pending.is_some() {
            self.send(Msg::Tick(now));
        }
        redraw | self.wake()
    }

    fn frame(&mut self, ctx: FrameContext) -> Scene {
        self.frame_scene(ctx.logical_width, ctx.logical_height, ctx.scale_factor)
    }
}

/// --screenshot: lay out the window headlessly and rasterise it with the CPU renderer.
fn screenshot(out: &std::path::Path, (w, h): (u32, u32), open: Option<PathBuf>, start: PathBuf) -> Result<(), String> {
    let mut app = App::new(start);
    if let Some(p) = open {
        if p.is_dir() {
            upd::open_location(&mut app.m, &mut app.fx, p);
        } else {
            let entries = archiver_core::list(&p, None).map_err(|e| format!("{}: {e}", p.display()))?;
            upd::show_listing(&mut app.m, &mut app.fx, p, entries);
        }
    }
    let scene = app.frame_scene(w as f32, h as f32, 1.0);
    let mut r = acme_render_cpu::CpuRenderer::new(w, h, 1.0).map_err(|e| e.to_string())?;
    r.render_scene(&scene).map_err(|e| e.to_string())?;
    let rgba: Vec<u8> = r.pixels().iter().flat_map(|&p| [(p >> 16) as u8, (p >> 8) as u8, p as u8, 255]).collect();
    image::save_buffer(out, &rgba, w, h, image::ExtendedColorType::Rgba8).map_err(|e| format!("{}: {e}", out.display()))
}

/// Merge Explorer's per-file processes (see `collect`) into the leader's file list.
fn collected(verb: &str, files: Vec<PathBuf>, collect: bool) -> Vec<PathBuf> {
    if !collect {
        return files;
    }
    match collect::gather(verb, &files) {
        Some(all) => all,
        None => std::process::exit(0),
    }
}

fn main() {
    let inv = match args::parse_invocation(std::env::args_os().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let c = inv.collect;
    let action = match inv.action {
        Action::ExtractHere(f) => Action::ExtractHere(collected("extract-here", f, c)),
        Action::ExtractToFolder(f) => Action::ExtractToFolder(collected("extract-to-folder", f, c)),
        Action::ExtractTo(f) => Action::ExtractTo(collected("extract-to", f, c)),
        Action::Compress(f) => Action::Compress(collected("compress", f, c)),
        Action::Test(f) => Action::Test(collected("test", f, c)),
        Action::CompressTo { format, files } => Action::CompressTo { format, files: collected(&format!("compress-to-{}", format.extension()), files, c) },
        other => other,
    };
    let start_dir = std::env::current_dir().ok().filter(|d| d.is_dir()).or_else(home_dir).unwrap_or_else(|| PathBuf::from("/"));
    match action {
        Action::Help => {
            println!("{}", args::USAGE);
            return;
        }
        Action::RegisterShell | Action::UnregisterShell => {
            let r = if action == Action::RegisterShell { shell::register() } else { shell::unregister() };
            match r {
                Ok(msg) => return println!("{msg}"),
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            }
        }
        Action::Screenshot { out, size, open } => {
            if let Err(e) = screenshot(&out, size, open, start_dir) {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
            return;
        }
        _ => {}
    }

    std::thread::spawn(|| view::purge_old_clips(Duration::from_secs(24 * 3600)));
    let mut app = App::new(start_dir);
    {
        let (m, fx) = (&mut app.m, &mut app.fx);
        match action {
            Action::Open(p) => upd::open_archive(m, fx, p),
            Action::Compress(files) => upd::open_create(m, files, None),
            Action::ExtractHere(files) => upd::extract_batch(m, fx, files, false),
            Action::ExtractToFolder(files) => upd::extract_batch(m, fx, files, true),
            Action::ExtractTo(files) => upd::extract_to(m, fx, files),
            Action::Test(files) => upd::test_files(m, fx, files),
            Action::CompressTo { format, files } => upd::compress_to(m, fx, format, files),
            _ => {}
        }
    }
    app.after_update();
    if app.closed {
        return;
    }
    match acme_platform::run(app) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("archiver: {e}");
            let _ = std::fs::remove_dir_all(view::temp_view_dir());
            std::process::exit(1);
        }
    }
    let _ = std::fs::remove_dir_all(view::temp_view_dir());
}

#[cfg(test)]
mod tests;

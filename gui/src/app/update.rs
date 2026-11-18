//! The state machine: every user intent and background result goes through [`update`].

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use acme_core::{ImageKey, Rgba8Image};
use acme_platform::PlatformKey;
use acme_textinput::{handle_key, handle_keyboard_shortcut, handle_text};
use archiver_core::{BatchDest, BatchResult, CreateOptions, Error, Format, Progress};

use super::effects::Effects;
use super::menus::MENU_KEYS;
use super::model::*;
use super::msg::*;
use crate::view::{self, Item};

pub const BUSY_MSG: &str = "目前有工作正在進行，請稍候…";
const PREVIEW_PW_MSG: &str = "此檔案已加密，請先輸入壓縮檔密碼（檔案 → 壓縮檔密碼…）";
pub const PREVIEW_DEBOUNCE: Duration = Duration::from_millis(120);
/// Image keys of preview images (icons use small keys).
const PREVIEW_IMAGE_KEY: u64 = 1 << 40;

pub fn update(m: &mut Model, msg: Msg, fx: &mut dyn Effects) {
    match msg {
        Msg::Cmd(c) => {
            m.popup = None;
            command(m, fx, c);
        }
        Msg::RowPressed { index, ctrl, shift } => row_pressed(m, fx, index, ctrl, shift),
        Msg::RowActivated(i) => activate(m, fx, i),
        Msg::RowContext { index, x, y } => row_context(m, fx, index, x, y),
        Msg::ClearSelection => {
            if m.selected.iter().any(|&s| s) {
                m.selected.fill(false);
                m.anchor = None;
                m.status.clear();
                selection_changed(m, fx);
            }
        }
        Msg::Sort(c) => {
            if m.sort_col == c {
                m.sort_asc = !m.sort_asc;
            } else {
                m.sort_col = c;
                m.sort_asc = true;
            }
            resort(m);
        }
        Msg::ScrollList(d) => {
            m.list_scroll += d;
            m.clamp_list_scroll();
        }
        Msg::ScrollListTo(y) => {
            m.list_scroll = y;
            m.clamp_list_scroll();
        }
        Msg::ScrollPreview(d) => m.pv.scroll = (m.pv.scroll + d).max(0.0),
        Msg::Key(k) => key(m, fx, k),
        Msg::Text(t) => {
            let multiline = m.focus == Some(Field::Comment);
            if let Some(inp) = m.focused_input() {
                let t: String = t.chars().filter(|c| !c.is_control() || (multiline && *c == '\n')).collect();
                handle_text(inp, &t);
            }
        }
        Msg::Preedit(t, c) => {
            if let Some(inp) = m.focused_input() {
                inp.set_preedit(&t, c);
            }
        }
        Msg::Dropped(paths) => handle_drops(m, fx, paths),
        Msg::OpenMenu(i) => {
            m.popup = if m.popup == Some(Popup::Menu(i)) { None } else { Some(Popup::Menu(i)) };
        }
        Msg::HoverMenu(i) => {
            if matches!(m.popup, Some(Popup::Menu(_))) {
                m.popup = Some(Popup::Menu(i));
            }
        }
        Msg::ClosePopup => m.popup = None,
        Msg::Tick(now) => {
            if m.pv.pending.as_ref().is_some_and(|p| now >= p.due)
                && let Some(p) = m.pv.pending.take()
            {
                fx.start_preview(p.generation, p.src, p.name);
            }
        }
        Msg::Dialog(d) => dialog_msg(m, fx, d),
        Msg::Listed(path, r) => listed(m, fx, path, r),
        Msg::JobDone(r, after) => job_done(m, fx, r, after),
        Msg::PreviewDone(d) => preview_done(m, d),
    }
}

// ------------------------------------------------------------------ listing / status

pub fn refresh(m: &mut Model, fx: &mut dyn Effects) {
    let mut items = match &m.mode {
        Mode::Fs(dir) => {
            let mut v = view::fs_children(dir).unwrap_or_default();
            if !m.filter.is_empty() {
                let f = m.filter.to_lowercase();
                v.retain(|i| i.name.to_lowercase().contains(&f));
            }
            if dir.parent().is_some() {
                v.insert(0, Item::parent());
            }
            v
        }
        Mode::Archive { entries, prefix, .. } => {
            let mut v = if m.filter.is_empty() { view::archive_children(entries, prefix) } else { view::archive_search(entries, &m.filter) };
            v.insert(0, Item::parent());
            v
        }
    };
    view::sort_items(&mut items, m.sort_col, m.sort_asc);
    m.selected = vec![false; items.len()];
    m.anchor = None;
    m.items = items;
    m.list_scroll = 0.0;
    selection_changed(m, fx);
}

/// Sort again keeping the selection (and the anchor row) on the same items.
fn resort(m: &mut Model) {
    let anchor = m.anchor.and_then(|a| m.items.get(a)).map(|i| i.full.clone());
    let mut rows: Vec<(Item, bool)> = m.items.drain(..).zip(m.selected.drain(..)).collect();
    let mut items: Vec<Item> = rows.iter().map(|(i, _)| i.clone()).collect();
    view::sort_items(&mut items, m.sort_col, m.sort_asc);
    m.selected = items
        .iter()
        .map(|it| {
            let pos = rows.iter().position(|(r, _)| r.full == it.full && r.is_parent == it.is_parent);
            pos.map(|p| rows.swap_remove(p).1).unwrap_or(false)
        })
        .collect();
    m.anchor = anchor.and_then(|a| items.iter().position(|i| i.full == a && !i.is_parent));
    m.items = items;
    if let Some(a) = m.anchor {
        m.ensure_row_visible(a);
    }
}

pub fn set_status(m: &mut Model, msg: impl Into<String>) {
    m.status = msg.into();
}

/// True (and a status hint) while a background job runs.
fn is_busy(m: &mut Model) -> bool {
    if m.job.is_some() {
        set_status(m, BUSY_MSG);
        return true;
    }
    false
}

pub fn spawn<F>(m: &mut Model, fx: &mut dyn Effects, title: &str, f: F)
where
    F: FnOnce(&Progress) -> Msg + Send + 'static,
{
    if is_busy(m) {
        return;
    }
    m.job = Some(JobView { title: title.into(), ..JobView::default() });
    fx.spawn(Box::new(f));
}

// ------------------------------------------------------------------ selection

fn row_pressed(m: &mut Model, fx: &mut dyn Effects, i: usize, ctrl: bool, shift: bool) {
    if i >= m.items.len() {
        return;
    }
    m.status.clear();
    let before = m.selected.clone();
    if shift {
        let a = m.anchor.unwrap_or(i).min(m.items.len() - 1);
        let (lo, hi) = (a.min(i), a.max(i));
        if !ctrl {
            m.selected.fill(false);
        }
        m.selected[lo..=hi].fill(true);
    } else if ctrl {
        m.selected[i] = !m.selected[i];
        m.anchor = Some(i);
    } else {
        m.selected.fill(false);
        m.selected[i] = true;
        m.anchor = Some(i);
    }
    if before != m.selected {
        selection_changed(m, fx);
    }
}

/// Right-click: keep a multi-selection that contains the row, otherwise select only it.
fn row_context(m: &mut Model, fx: &mut dyn Effects, i: usize, x: f32, y: f32) {
    if is_busy(m) {
        return;
    }
    let was = m.selected.clone();
    if !view::select_context_item(&m.items, &mut m.selected, i) {
        return;
    }
    m.anchor = Some(i);
    m.status.clear();
    if was != m.selected {
        selection_changed(m, fx);
    }
    m.popup = Some(Popup::Context { x, y });
}

fn select_all(m: &mut Model, fx: &mut dyn Effects) {
    m.selected = m.items.iter().map(|i| !i.is_parent).collect();
    m.status.clear();
    selection_changed(m, fx);
}

/// Arrow / Page / Home / End keys: move a single selection by `delta` rows.
fn move_selection(m: &mut Model, fx: &mut dyn Effects, delta: i64) {
    let n = m.items.len();
    if n == 0 {
        return;
    }
    let cur = m.anchor.or_else(|| m.selected.iter().position(|&x| x));
    // with nothing selected, start just outside the list so Down/End and Up/Home land on an end
    let base = cur.map_or(if delta < 0 { n as i64 } else { -1 }, |c| c as i64);
    let next = (base + delta).clamp(0, n as i64 - 1) as usize;
    m.selected.fill(false);
    m.selected[next] = true;
    m.anchor = Some(next);
    m.status.clear();
    m.ensure_row_visible(next);
    selection_changed(m, fx);
}

// ------------------------------------------------------------------ keyboard

fn key(m: &mut Model, fx: &mut dyn Effects, k: KeyPress) {
    if m.popup.is_some() {
        if k.key == PlatformKey::Escape {
            m.popup = None;
        }
        return;
    }
    if m.dialog.is_some() {
        return dialog_key(m, fx, k);
    }
    if k.alt
        && let Some(c) = k.shortcut()
        && let Some(i) = MENU_KEYS.iter().position(|x| *x == c)
    {
        m.popup = Some(Popup::Menu(i));
        return;
    }
    match k.key {
        PlatformKey::Enter => {
            if let Some(i) = m.anchor.filter(|&a| m.selected.get(a) == Some(&true)).or_else(|| m.selected.iter().position(|&x| x)) {
                activate(m, fx, i);
            }
        }
        PlatformKey::Backspace => go_up(m, fx),
        PlatformKey::Delete => {
            if m.can_delete() {
                do_delete(m, fx);
            }
        }
        PlatformKey::ArrowUp => move_selection(m, fx, -1),
        PlatformKey::ArrowDown => move_selection(m, fx, 1),
        PlatformKey::PageUp => move_selection(m, fx, -(m.page_rows() as i64)),
        PlatformKey::PageDown => move_selection(m, fx, m.page_rows() as i64),
        PlatformKey::Home => move_selection(m, fx, -(m.items.len() as i64)),
        PlatformKey::End => move_selection(m, fx, m.items.len() as i64),
        PlatformKey::Function(3) => command(m, fx, Cmd::View),
        PlatformKey::Function(5) => command(m, fx, Cmd::Refresh),
        PlatformKey::Escape => {
            if !m.filter.is_empty() {
                m.filter.clear();
                refresh(m, fx);
            } else {
                m.status.clear();
            }
        }
        _ if k.ctrl => match k.shortcut().as_deref() {
            Some("a") => command(m, fx, Cmd::SelectAll),
            Some("c") => command(m, fx, Cmd::Copy),
            Some("f") => command(m, fx, Cmd::Find),
            Some("o") => command(m, fx, Cmd::Open),
            _ => {}
        },
        _ => {}
    }
}

fn dialog_key(m: &mut Model, fx: &mut dyn Effects, k: KeyPress) {
    match k.key {
        PlatformKey::Escape => {
            match m.focused_input() {
                Some(i) if !i.preedit.is_empty() => i.cancel_preedit(),
                _ => close_dialog(m),
            }
            return;
        }
        PlatformKey::Enter => {
            if m.focus == Some(Field::Comment) && !k.ctrl {
                if let Some(i) = m.focused_input() {
                    handle_text(i, "\n");
                }
            } else {
                dialog_msg(m, fx, DlgMsg::Ok);
            }
            return;
        }
        PlatformKey::Tab => {
            let fields = m.dialog.as_ref().map(Dialog::fields).unwrap_or_default();
            if !fields.is_empty() {
                let cur = m.focus.and_then(|f| fields.iter().position(|x| *x == f));
                let n = fields.len();
                let next = match cur {
                    Some(c) if k.shift => (c + n - 1) % n,
                    Some(c) => (c + 1) % n,
                    None => 0,
                };
                set_focus(m, Some(fields[next]));
            }
            return;
        }
        _ => {}
    }
    let multiline = m.focus == Some(Field::Comment);
    let masked = m.focus.and_then(|f| m.input(f)).is_some_and(|i| i.password);
    if k.ctrl
        && let Some(c) = k.shortcut()
        && matches!(c.as_str(), "a" | "c" | "v" | "x" | "z" | "y")
    {
        let selected: Option<String> = m.focused_input().and_then(|i| i.selection().map(|(s, e)| i.text[s..e].to_string()));
        match c.as_str() {
            "c" | "x" => {
                if let Some(t) = selected.filter(|_| !masked) {
                    let _ = fx.copy_text(&t);
                    if c == "x"
                        && let Some(i) = m.focused_input()
                    {
                        handle_keyboard_shortcut(i, "x", None);
                    }
                }
            }
            "v" => {
                if let Some(t) = fx.paste_text() {
                    let t = if multiline { t.replace("\r\n", "\n") } else { t.replace(['\r', '\n'], " ") };
                    if let Some(i) = m.focused_input() {
                        handle_text(i, &t);
                    }
                }
            }
            other => {
                let other = if other == "z" && k.shift { "y" } else { other };
                if let Some(i) = m.focused_input() {
                    handle_keyboard_shortcut(i, other, None);
                }
            }
        }
        return;
    }
    if let Some(i) = m.focused_input() {
        if handle_key(i, &k.key, true, None, k.ctrl, k.shift) {
            return;
        }
        if !k.ctrl
            && !k.alt
            && let Some(t) = k.text.as_deref().filter(|t| !t.is_empty() && !t.chars().any(char::is_control))
        {
            handle_text(i, t);
        }
    }
}

fn set_focus(m: &mut Model, f: Option<Field>) {
    if let Some(old) = m.focus
        && let Some(i) = m.input_mut(old)
    {
        i.focused = false;
    }
    m.focus = f;
    if let Some(f) = f
        && let Some(i) = m.input_mut(f)
    {
        i.focused = true;
    }
}

fn open_dialog(m: &mut Model, d: Dialog) {
    let first = d.fields().first().copied();
    m.popup = None;
    m.dialog = Some(d);
    m.focus = None;
    set_focus(m, first);
}

fn close_dialog(m: &mut Model) {
    if matches!(m.dialog, Some(Dialog::Create(_))) {
        m.create_inputs.clear();
    }
    m.dialog = None;
    m.focus = None;
    m.popup = None;
    m.fallback_open = None;
}

// ------------------------------------------------------------------ commands

pub fn command(m: &mut Model, fx: &mut dyn Effects, c: Cmd) {
    match c {
        Cmd::Open => {
            if let Some(p) = fx.pick_archive() {
                open_archive(m, fx, p);
            }
        }
        Cmd::NewArchive => do_add(m, fx, None),
        Cmd::Add => {
            if m.in_archive() {
                if !m.archive_writable() {
                    return set_status(m, "此格式不支援加入檔案");
                }
                if is_busy(m) {
                    return;
                }
                if let Some(files) = fx.pick_files("選擇要加入壓縮檔的檔案") {
                    add_to_archive(m, fx, files);
                }
            } else {
                do_add(m, fx, None);
            }
        }
        Cmd::AddEach => do_add(m, fx, Some(true)),
        Cmd::Extract => do_extract(m, fx, ExtractScope::Automatic),
        Cmd::ExtractSelected => do_extract(m, fx, ExtractScope::Selected),
        Cmd::ExtractAll => do_extract(m, fx, ExtractScope::All),
        Cmd::ExtractHere => do_extract_each(m, fx, BatchDest::Beside),
        Cmd::ExtractEach => do_extract_each(m, fx, BatchDest::BesideInFolder),
        Cmd::Copy => do_copy(m, fx),
        Cmd::CopyText => {
            let text = match &m.pv.view {
                PvView::Text(t) | PvView::Hex(t) => t.clone(),
                _ => return,
            };
            match fx.copy_text(&text) {
                Ok(()) => set_status(m, "已複製文字到剪貼簿"),
                Err(e) => set_status(m, format!("錯誤：{e}")),
            }
        }
        Cmd::Refresh => {
            m.status.clear();
            match &m.mode {
                Mode::Archive { path, .. } => {
                    let p = path.clone();
                    reload_archive(m, fx, p)
                }
                Mode::Fs(_) => refresh(m, fx),
            }
        }
        Cmd::Test => do_test(m, fx),
        Cmd::View => {
            if let Some(it) = m.selected_items().into_iter().find(|i| !i.is_dir) {
                view_item(m, fx, it);
            }
        }
        Cmd::Delete => do_delete(m, fx),
        Cmd::Find => {
            let f = m.filter.clone();
            open_dialog(m, Dialog::Find(text_input(&f, false)));
        }
        Cmd::Wizard => {
            if m.in_archive() {
                do_extract(m, fx, ExtractScope::All);
            } else {
                do_add(m, fx, None);
            }
        }
        Cmd::Info => {
            let t = info_text(m);
            open_dialog(m, Dialog::Info(t));
        }
        Cmd::Shell => {
            let registered = fx.shell_registered();
            open_dialog(m, Dialog::Shell { registered });
        }
        Cmd::Password => {
            let pw = m.password.clone();
            open_dialog(m, Dialog::Password(text_input(&pw, true)));
        }
        Cmd::SelectAll => select_all(m, fx),
        Cmd::Up => go_up(m, fx),
        Cmd::Cancel => fx.cancel_job(),
        Cmd::About => fx.info(
            "關於 Archiver",
            &format!("Archiver {}\n跨平台壓縮／解壓縮工具（Rust + AcmeUI-Native）\nhttps://github.com/stevenke1981/Archiver\nMIT License", env!("CARGO_PKG_VERSION")),
        ),
        Cmd::Exit => {
            if !m.busy() || fx.confirm("結束 Archiver", "背景工作仍在進行中，確定要取消工作並結束嗎？") {
                fx.close();
            }
        }
        Cmd::TogglePreview => {
            m.preview_visible = !m.preview_visible;
            selection_changed(m, fx);
        }
        Cmd::ShowPreview => {
            if !m.preview_visible {
                m.preview_visible = true;
                selection_changed(m, fx);
            }
        }
        Cmd::Comment => open_comment(m, fx),
        Cmd::Scan => do_scan(m, fx),
        Cmd::FavAdd => {
            let loc = m.location();
            match fx.favorites_add(&loc) {
                Ok(()) => {
                    m.favorites = fx.favorites_load();
                    set_status(m, format!("已加入我的最愛：{}", loc.display()));
                }
                Err(e) => {
                    set_status(m, format!("錯誤：{e}"));
                    fx.error(&format!("無法加入我的最愛：{e}"));
                }
            }
        }
        Cmd::FavManage => {
            m.favorites = fx.favorites_load();
            open_dialog(m, Dialog::Favorites);
        }
        Cmd::FavOpen(i) => {
            if let Some(p) = m.favorites.get(i).cloned() {
                open_location(m, fx, p);
            }
        }
    }
}

/// Open a folder (file-system mode) or an archive.
pub fn open_location(m: &mut Model, fx: &mut dyn Effects, p: PathBuf) {
    if p.is_dir() {
        if m.in_archive() {
            forget_password(m);
        }
        m.mode = Mode::Fs(p);
        m.filter.clear();
        m.status.clear();
        m.pv.cache = None;
        refresh(m, fx);
    } else if p.is_file() && Format::detect(&p).is_some() {
        open_archive(m, fx, p);
    } else {
        set_status(m, format!("找不到 {}", p.display()));
    }
}

pub fn open_archive(m: &mut Model, fx: &mut dyn Effects, path: PathBuf) {
    if Format::detect(&path).is_none() {
        return set_status(m, format!("不支援的格式：{}", path.display()));
    }
    reload_archive(m, fx, path);
}

/// Drop the session password (leaving the archive it was entered for).
fn forget_password(m: &mut Model) {
    m.password.clear();
    m.password_for = None;
}

fn reload_archive(m: &mut Model, fx: &mut dyn Effects, path: PathBuf) {
    // a password belongs to the archive it was entered for
    if m.password_for.as_ref().is_some_and(|p| *p != path) {
        forget_password(m);
    }
    // listing can take long (.tar.gz is read in full): in the background
    let pw = m.opt_pw();
    spawn(m, fx, "讀取中…", move |_| {
        let r = archiver_core::list(&path, pw.as_deref()).map_err(|e| e.to_string());
        Msg::Listed(path, r)
    });
}

/// Show an archive listing (also used synchronously by --screenshot).
pub fn show_listing(m: &mut Model, fx: &mut dyn Effects, path: PathBuf, entries: Vec<archiver_core::Entry>) {
    let fmt = Format::detect(&path).unwrap_or(Format::Zip);
    // re-listing the open archive (F5, after delete) keeps the folder and status
    let prefix = match &m.mode {
        Mode::Archive { path: p, prefix, .. } if *p == path => Some(prefix.clone()),
        _ => None,
    };
    if prefix.is_none() {
        m.filter.clear();
        m.status.clear();
    }
    if !m.password.is_empty() {
        m.password_for = Some(path.clone());
    }
    m.mode = Mode::Archive { path, fmt, entries, prefix: prefix.unwrap_or_default() };
    m.pv.cache = None;
    refresh(m, fx);
}

fn listed(m: &mut Model, fx: &mut dyn Effects, path: PathBuf, r: Result<Vec<archiver_core::Entry>, String>) {
    m.job = None;
    match r {
        Ok(entries) => show_listing(m, fx, path, entries),
        Err(e) => {
            set_status(m, format!("無法開啟 {}: {e}", path.display()));
            if is_password_err(&e) {
                open_dialog(m, Dialog::Password(text_input("", true)));
                m.fallback_open = Some(path);
            } else {
                fx.error(&format!("無法開啟 {}：{e}", path.display()));
            }
        }
    }
}

pub fn go_up(m: &mut Model, fx: &mut dyn Effects) {
    m.status.clear();
    if !m.filter.is_empty() {
        m.filter.clear();
    } else {
        let next = match &mut m.mode {
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
            forget_password(m);
            m.mode = Mode::Fs(d);
            m.pv.cache = None;
        }
    }
    refresh(m, fx);
}

pub fn activate(m: &mut Model, fx: &mut dyn Effects, idx: usize) {
    let Some(it) = m.items.get(idx).cloned() else { return };
    if it.is_parent {
        return go_up(m, fx);
    }
    if it.is_dir {
        m.filter.clear();
        m.status.clear();
        match &mut m.mode {
            Mode::Fs(dir) => *dir = PathBuf::from(&it.full),
            Mode::Archive { prefix, .. } => *prefix = format!("{}/", it.full),
        }
        return refresh(m, fx);
    }
    if m.in_archive() {
        view_item(m, fx, it);
    } else if Format::detect(Path::new(&it.name)).is_some() {
        open_archive(m, fx, PathBuf::from(it.full));
    } else {
        fx.open_path(Path::new(&it.full));
    }
}

/// Extract one archive entry to a temp folder and open it with the system default app.
fn view_item(m: &mut Model, fx: &mut dyn Effects, it: Item) {
    let (arc, pw) = match &m.mode {
        Mode::Archive { path, .. } => (path.clone(), m.opt_pw()),
        Mode::Fs(_) => return fx.open_path(Path::new(&it.full)),
    };
    let dest = view::temp_view_dir();
    spawn(m, fx, "解壓縮檢視中…", move |p| {
        let target = if Format::detect(&arc).is_some_and(|f| f.is_single_file()) { dest.join(&it.name) } else { dest.join(&it.full) };
        match archiver_core::extract(&arc, &dest, pw.as_deref(), Some(std::slice::from_ref(&it.full)), p) {
            Ok(()) => Msg::JobDone(Ok(format!("已開啟 {}", it.name)), After::OpenFile(target)),
            Err(e) => Msg::JobDone(Err(e.to_string()), After::Nothing),
        }
    });
}

// ------------------------------------------------------------------ extraction

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExtractScope {
    Automatic,
    Selected,
    All,
}

pub fn extraction_names(items: &[Item], scope: ExtractScope) -> Result<Option<Vec<String>>, &'static str> {
    let names: Vec<String> = items.iter().filter(|i| !i.is_parent).map(|i| i.full.clone()).collect();
    match scope {
        ExtractScope::Selected if names.is_empty() => Err("請先選取要解壓縮的檔案或資料夾"),
        ExtractScope::All => Ok(None),
        _ => Ok((!names.is_empty()).then_some(names)),
    }
}

fn do_extract(m: &mut Model, fx: &mut dyn Effects, scope: ExtractScope) {
    if is_busy(m) {
        return;
    }
    if scope == ExtractScope::Selected && !m.can_extract_selection() {
        return set_status(m, "請先選取要解壓縮的檔案或資料夾；檔案系統中請只選取壓縮檔");
    }
    let sel = m.selected_items();
    let (sources, names, start) = match &m.mode {
        Mode::Archive { path, .. } => {
            let names = match extraction_names(&sel, scope) {
                Ok(n) => n,
                Err(message) => return set_status(m, message),
            };
            (vec![path.clone()], names, path.parent().map(Path::to_path_buf).unwrap_or_default())
        }
        Mode::Fs(dir) => {
            let arcs: Vec<PathBuf> = sel.iter().filter(|i| !i.is_dir && Format::detect(Path::new(&i.name)).is_some()).map(|i| PathBuf::from(&i.full)).collect();
            (arcs, None, dir.clone())
        }
    };
    let pw = m.opt_pw();
    if sources.is_empty() {
        if let Some(p) = fx.pick_file("選擇要解壓縮的壓縮檔") {
            open_archive(m, fx, p);
        }
        return;
    }
    let title = if names.is_some() { "解壓縮選取項目至…" } else { "解壓縮至…" };
    let Some(dest) = fx.pick_folder(title, Some(&start)) else { return };
    extract_into(m, fx, sources, names, pw, dest);
}

pub fn extract_into(m: &mut Model, fx: &mut dyn Effects, sources: Vec<PathBuf>, sel: Option<Vec<String>>, pw: Option<String>, dest: PathBuf) {
    if sources.len() > 1 {
        let dest = BatchDest::IntoFolders(dest);
        return spawn(m, fx, "解壓縮中…", move |p| batch_msg(&archiver_core::extract_each(&sources, &dest, pw.as_deref(), p)));
    }
    spawn(m, fx, "解壓縮中…", move |p| match archiver_core::extract(&sources[0], &dest, pw.as_deref(), sel.as_deref(), p) {
        Ok(()) => Msg::JobDone(Ok(format!("已解壓縮到 {}", dest.display())), After::ReloadFs),
        Err(e) => Msg::JobDone(Err(format!("{}: {e}", sources[0].display())), After::Nothing),
    });
}

/// "Extract here" / "extract each to its own folder": the selected archives or the open one.
fn do_extract_each(m: &mut Model, fx: &mut dyn Effects, dest: BatchDest) {
    let arcs: Vec<PathBuf> = match &m.mode {
        Mode::Archive { path, .. } => vec![path.clone()],
        Mode::Fs(_) => m.selected_items().into_iter().filter(|i| !i.is_dir && Format::detect(Path::new(&i.name)).is_some()).map(|i| PathBuf::from(i.full)).collect(),
    };
    if arcs.is_empty() {
        return set_status(m, "請先開啟或選取壓縮檔");
    }
    let pw = m.opt_pw();
    spawn(m, fx, "解壓縮中…", move |p| batch_msg(&archiver_core::extract_each(&arcs, &dest, pw.as_deref(), p)));
}

/// Context-menu "extract here / to folder" from the command line.
pub fn extract_batch(m: &mut Model, fx: &mut dyn Effects, files: Vec<PathBuf>, to_folder: bool) {
    m.close_when_done = true;
    let dest = if to_folder { BatchDest::BesideInFolder } else { BatchDest::Beside };
    spawn(m, fx, "解壓縮中…", move |p| batch_msg(&archiver_core::extract_each(&files, &dest, None, p)));
}

/// Shell command: choose a destination; one archive goes directly into it.
pub fn extract_to(m: &mut Model, fx: &mut dyn Effects, files: Vec<PathBuf>) {
    if m.busy() || files.is_empty() {
        return;
    }
    let start = files[0].parent().map(Path::to_path_buf);
    let Some(dest) = fx.pick_folder("解壓縮至…", start.as_deref()) else { return fx.close() };
    m.close_when_done = true;
    if files.len() == 1 {
        m.fallback_open = Some(files[0].clone());
    }
    extract_into(m, fx, files, None, None, dest);
}

/// Shell "test archive": test every file, show the result, then quit.
pub fn test_files(m: &mut Model, fx: &mut dyn Effects, files: Vec<PathBuf>) {
    m.close_when_done = true;
    let pw = m.opt_pw();
    spawn(m, fx, "測試中…", move |p| test_msg(&files, pw.as_deref(), p));
}

/// Shell "compress to <name>.<ext>": create next to the inputs without a dialog.
pub fn compress_to(m: &mut Model, fx: &mut dyn Effects, format: Format, files: Vec<PathBuf>) {
    if files.is_empty() {
        return fx.close();
    }
    m.close_when_done = true;
    let output = unique_path(&archiver_core::suggest_output(&files, format));
    let opts = CreateOptions { format, level: format.level_range().map(|r| r.2), password: None };
    spawn(m, fx, "壓縮中…", move |p| {
        let r = archiver_core::create(&output, &files, &opts, p).map(|_| format!("已建立 {}", output.display())).map_err(|e| e.to_string());
        Msg::JobDone(r, After::ReloadFs)
    });
}

/// The job stopped because the user cancelled it (core's Error::Cancelled).
pub fn is_cancel_err(e: &str) -> bool {
    e == Error::Cancelled.to_string()
}

/// Password for rewriting the open archive (add / delete): only when it is encrypted,
/// so a plain archive is never re-created encrypted. None = asked for one first.
fn rewrite_password(m: &mut Model) -> Option<Option<String>> {
    let Mode::Archive { entries, .. } = &m.mode else { return Some(None) };
    if !entries.iter().any(|e| e.encrypted) {
        return Some(None);
    }
    match m.opt_pw() {
        Some(pw) => Some(Some(pw)),
        None => {
            set_status(m, "此壓縮檔已加密，請先輸入密碼");
            open_dialog(m, Dialog::Password(text_input("", true)));
            None
        }
    }
}

pub fn is_password_err(e: &str) -> bool {
    e.to_lowercase().contains("password")
}

/// "已完成 N 個（成功 X，失敗 Y）" plus one line per failure (at most 20); returns the failure count.
pub fn batch_summary(results: &[BatchResult]) -> (String, usize) {
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
pub fn single_password_failure(results: &[BatchResult]) -> Option<PathBuf> {
    let mut failed = results.iter().filter(|r| r.result.is_err());
    match (failed.next(), failed.next()) {
        (Some(r), None) if r.result.as_ref().is_err_and(|e| is_password_err(&e.to_string())) => Some(r.input.clone()),
        _ => None,
    }
}

pub fn batch_msg(results: &[BatchResult]) -> Msg {
    if results.iter().any(|r| matches!(r.result, Err(Error::Cancelled))) {
        return Msg::JobDone(Err(Error::Cancelled.to_string()), After::Nothing);
    }
    let (text, failed) = batch_summary(results);
    Msg::JobDone(Ok(text), After::Batch { failed, retry: single_password_failure(results) })
}

pub fn test_msg(targets: &[PathBuf], pw: Option<&str>, p: &Progress) -> Msg {
    let mut lines = Vec::new();
    let mut failed = 0;
    for a in targets {
        let name = a.file_name().unwrap_or_default().to_string_lossy();
        match archiver_core::test(a, pw, p) {
            Ok(n) => lines.push(format!("{name}：{n} 個檔案測試正確，沒有發現錯誤。")),
            Err(Error::Cancelled) => return Msg::JobDone(Err(Error::Cancelled.to_string()), After::Nothing),
            // keep going: one broken archive should not hide the others' results
            Err(e) => {
                failed += 1;
                lines.push(format!("✗ {name}：{e}"));
            }
        }
    }
    if failed == 0 {
        return Msg::JobDone(Ok(lines.join("\n")), After::Message);
    }
    let head = format!("已測試 {} 個（正確 {}，錯誤 {failed}）", targets.len(), targets.len() - failed);
    if targets.len() == 1 {
        return Msg::JobDone(Err(lines.join("\n").trim_start_matches("✗ ").to_string()), After::Nothing);
    }
    Msg::JobDone(Err(format!("{head}\n{}", lines.join("\n"))), After::Nothing)
}

fn do_test(m: &mut Model, fx: &mut dyn Effects) {
    let targets: Vec<PathBuf> = match &m.mode {
        Mode::Archive { path, .. } => vec![path.clone()],
        Mode::Fs(_) => m.selected_items().into_iter().filter(|i| !i.is_dir && Format::detect(Path::new(&i.name)).is_some()).map(|i| PathBuf::from(i.full)).collect(),
    };
    if targets.is_empty() {
        return set_status(m, "請先開啟或選取壓縮檔");
    }
    let pw = m.opt_pw();
    spawn(m, fx, "測試中…", move |p| test_msg(&targets, pw.as_deref(), p));
}

fn do_delete(m: &mut Model, fx: &mut dyn Effects) {
    let Mode::Archive { path, .. } = &m.mode else { return };
    let arc = path.clone();
    if !m.can_delete() {
        return set_status(m, "此格式不支援刪除檔案");
    }
    let names: Vec<String> = m.selected_items().into_iter().map(|i| i.full).collect();
    if names.is_empty() {
        return set_status(m, "請先選取要刪除的檔案");
    }
    if is_busy(m) || !fx.confirm("刪除檔案", &format!("確定要從壓縮檔中刪除 {} 個項目嗎？", names.len())) {
        return;
    }
    let Some(pw) = rewrite_password(m) else { return };
    spawn(m, fx, "刪除中…", move |p| {
        let r = archiver_core::delete(&arc, &names, pw.as_deref(), p).map(|_| format!("已刪除 {} 個項目", names.len())).map_err(|e| e.to_string());
        Msg::JobDone(r, After::ReloadArchive)
    });
}

/// Add files to the open archive, into the folder that is shown.
pub fn add_to_archive(m: &mut Model, fx: &mut dyn Effects, files: Vec<PathBuf>) {
    let Mode::Archive { path, fmt, prefix, .. } = &m.mode else { return };
    if files.is_empty() {
        return;
    }
    let (arc, fmt, prefix) = (path.clone(), *fmt, prefix.trim_end_matches('/').to_string());
    let Some(pw) = rewrite_password(m) else { return };
    let opts = CreateOptions { format: fmt, level: fmt.level_range().map(|r| r.2), password: pw.filter(|_| fmt.supports_password()) };
    spawn(m, fx, "加入中…", move |p| {
        let n = files.len();
        let r = archiver_core::add(&arc, &files, &prefix, &opts, p).map(|_| format!("已加入 {n} 個項目")).map_err(|e| e.to_string());
        Msg::JobDone(r, After::ReloadArchive)
    });
}

fn do_add(m: &mut Model, fx: &mut dyn Effects, each: Option<bool>) {
    let fs_sel: Vec<PathBuf> = match &m.mode {
        Mode::Fs(_) => m.selected_items().into_iter().map(|i| PathBuf::from(i.full)).collect(),
        _ => vec![],
    };
    if !fs_sel.is_empty() {
        open_create(m, fs_sel, each);
    } else if let Some(f) = fx.pick_files("選擇要加入的檔案") {
        open_create(m, f, each);
    }
}

// ------------------------------------------------------------------ create dialog

/// Open the create dialog with `files` added. `each`: preset the batch checkbox.
pub fn open_create(m: &mut Model, files: Vec<PathBuf>, each: Option<bool>) {
    for f in files {
        if !m.create_inputs.contains(&f) {
            m.create_inputs.push(f);
        }
    }
    match &mut m.dialog {
        Some(Dialog::Create(d)) => {
            if let Some(e) = each {
                d.each = e;
            }
        }
        _ => {
            let all = Format::writable();
            let format = m.last_format.min(all.len() - 1);
            let d = CreateDlg {
                format,
                level: all[format].level_range().map_or(0, |r| r.2),
                each: each.unwrap_or(false),
                password: text_input("", true),
                output: text_input("", false),
            };
            open_dialog(m, Dialog::Create(d));
            set_focus(m, Some(Field::CreateOutput));
        }
    }
    update_create_fields(m);
}

pub fn update_create_fields(m: &mut Model) {
    let Some(Dialog::Create(d)) = &mut m.dialog else { return };
    let fmt = d.fmt();
    let Some(first) = m.create_inputs.first() else { return };
    let cur = PathBuf::from(d.output.text.trim());
    if !d.each {
        set_input_text(&mut d.output, &archiver_core::suggest_output(&m.create_inputs, fmt).display().to_string());
    } else if cur.as_os_str().is_empty() || (!cur.is_dir() && Format::detect(&cur).is_some()) {
        // empty, or still the single-archive file name: use the first input's folder
        let dir = first.parent().filter(|p| !p.as_os_str().is_empty()).map(Path::to_path_buf).or_else(|| std::env::current_dir().ok()).unwrap_or_default();
        set_input_text(&mut d.output, &dir.display().to_string());
    }
}

fn apply_format(m: &mut Model, idx: usize) {
    let Some(Dialog::Create(d)) = &mut m.dialog else { return };
    d.format = idx.min(Format::writable().len() - 1);
    m.last_format = d.format;
    let fmt = d.fmt();
    if let Some((lo, hi, def)) = fmt.level_range()
        && !(lo..=hi).contains(&d.level)
    {
        d.level = def;
    }
    if !fmt.supports_password() && m.focus == Some(Field::CreatePassword) {
        set_focus(m, Some(Field::CreateOutput));
    }
    update_create_fields(m);
}

fn start_create(m: &mut Model, fx: &mut dyn Effects) {
    let Some(Dialog::Create(d)) = &m.dialog else { return };
    let fmt = d.fmt();
    let output = PathBuf::from(d.output.text.trim());
    let each = d.each;
    let pw = d.password.text.clone();
    let level = fmt.level_range().map(|_| d.level);
    let inputs = m.create_inputs.clone();
    if inputs.is_empty() || output.as_os_str().is_empty() || is_busy(m) {
        return;
    }
    if !each && output.exists() && !fx.confirm("覆寫檔案？", &format!("{} 已存在，要覆寫嗎？", output.display())) {
        return;
    }
    let opts = CreateOptions { format: fmt, level, password: Some(pw).filter(|p| !p.is_empty() && fmt.supports_password()) };
    close_dialog(m);
    if each {
        // output is a folder; core picks a unique archive name per input
        return spawn(m, fx, "分批壓縮中…", move |p| {
            if let Err(e) = std::fs::create_dir_all(&output) {
                return Msg::JobDone(Err(format!("{}: {e}", output.display())), After::Nothing);
            }
            batch_msg(&archiver_core::create_each(&inputs, Some(&output), &opts, p))
        });
    }
    spawn(m, fx, "壓縮中…", move |p| {
        let r = archiver_core::create(&output, &inputs, &opts, p).map(|_| format!("已建立 {}", output.display())).map_err(|e| e.to_string());
        Msg::JobDone(r, After::ReloadFs)
    });
}

fn dialog_msg(m: &mut Model, fx: &mut dyn Effects, d: DlgMsg) {
    if !matches!(d, DlgMsg::OpenFormats { .. }) {
        m.popup = None;
    }
    match d {
        DlgMsg::Ok => match &m.dialog {
            Some(Dialog::Create(_)) => start_create(m, fx),
            Some(Dialog::Password(t)) => {
                let password = t.text.clone();
                // the archive that asked for the password wins over the one that is shown
                let target = m.fallback_open.take().or_else(|| match &m.mode {
                    Mode::Archive { path, .. } => Some(path.clone()),
                    _ => None,
                });
                close_dialog(m);
                m.password = password;
                m.password_for = target.clone();
                if let Some(p) = target {
                    open_archive(m, fx, p);
                }
            }
            Some(Dialog::Find(t)) => {
                m.filter = t.text.trim().to_string();
                close_dialog(m);
                refresh(m, fx);
            }
            Some(Dialog::Comment { .. }) => save_comment(m, fx),
            _ => close_dialog(m),
        },
        DlgMsg::Cancel => close_dialog(m),
        DlgMsg::Focus(f, offset) => {
            if f == Field::CreatePassword && matches!(&m.dialog, Some(Dialog::Create(d)) if !d.fmt().supports_password()) {
                return;
            }
            set_focus(m, Some(f));
            if let (Some(o), Some(i)) = (offset, m.focused_input()) {
                i.move_cursor_to_offset(o.min(i.text.len()));
            }
        }
        DlgMsg::OpenFormats { x, y, w } => {
            m.popup = if matches!(m.popup, Some(Popup::Formats { .. })) { None } else { Some(Popup::Formats { x, y, w }) };
        }
        DlgMsg::SetFormat(i) => apply_format(m, i),
        DlgMsg::SetLevel(v) => {
            if let Some(Dialog::Create(d)) = &mut m.dialog
                && let Some((lo, hi, _)) = d.fmt().level_range()
            {
                d.level = v.clamp(lo, hi);
            }
        }
        DlgMsg::LevelStep(s) => {
            if let Some(Dialog::Create(d)) = &mut m.dialog
                && let Some((lo, hi, _)) = d.fmt().level_range()
            {
                d.level = (d.level as i64 + s as i64).clamp(lo as i64, hi as i64) as u32;
            }
        }
        DlgMsg::ToggleEach => {
            if let Some(Dialog::Create(d)) = &mut m.dialog {
                d.each = !d.each;
            }
            update_create_fields(m);
        }
        DlgMsg::Browse => {
            let Some(Dialog::Create(d)) = &mut m.dialog else { return };
            let cur = PathBuf::from(d.output.text.trim());
            let picked = if d.each {
                fx.pick_folder("選擇輸出資料夾", Some(&cur))
            } else {
                let name = cur.file_name().map(|n| n.to_string_lossy().to_string());
                fx.save_file("儲存壓縮檔", cur.parent(), name.as_deref())
            };
            if let (Some(p), Some(Dialog::Create(d))) = (picked, &mut m.dialog) {
                set_input_text(&mut d.output, &p.display().to_string());
            }
        }
        DlgMsg::ToggleShell => {
            let Some(Dialog::Shell { registered }) = m.dialog else { return };
            let msg = match fx.shell_set(!registered) {
                Ok(t) => t,
                Err(e) => format!("錯誤：{e}"),
            };
            let now = fx.shell_registered();
            m.dialog = Some(Dialog::Shell { registered: now });
            set_status(m, msg);
        }
        DlgMsg::FindClear => {
            m.filter.clear();
            close_dialog(m);
            refresh(m, fx);
        }
        DlgMsg::FavOpen(i) => {
            if let Some(p) = m.favorites.get(i).cloned() {
                close_dialog(m);
                open_location(m, fx, p);
            }
        }
        DlgMsg::FavRemove(i) => {
            if let Some(p) = m.favorites.get(i).cloned() {
                if let Err(e) = fx.favorites_remove(&p) {
                    fx.error(&format!("無法移除：{e}"));
                }
                m.favorites = fx.favorites_load();
            }
        }
    }
}

// ------------------------------------------------------------------ comment / scan

fn open_comment(m: &mut Model, fx: &mut dyn Effects) {
    let Mode::Archive { path, fmt, .. } = &m.mode else { return set_status(m, "請先開啟 ZIP 壓縮檔") };
    if *fmt != Format::Zip {
        return set_status(m, "只有 ZIP 壓縮檔可以加入註解");
    }
    let archive = path.clone();
    match archiver_core::read_comment(&archive) {
        Ok(c) => {
            let text = text_input(&c.unwrap_or_default(), false);
            open_dialog(m, Dialog::Comment { archive, text });
        }
        Err(e) => {
            set_status(m, format!("錯誤：{e}"));
            fx.error(&format!("無法讀取註解：{e}"));
        }
    }
}

fn save_comment(m: &mut Model, fx: &mut dyn Effects) {
    let Some(Dialog::Comment { archive, text }) = &m.dialog else { return };
    let (archive, comment) = (archive.clone(), text.text.clone());
    if is_busy(m) {
        return;
    }
    close_dialog(m);
    spawn(m, fx, "儲存註解中…", move |p| {
        let r = archiver_core::set_comment(&archive, &comment, p).map(|_| "已更新壓縮檔註解".to_string()).map_err(|e| e.to_string());
        Msg::JobDone(r, After::Nothing)
    });
}

fn do_scan(m: &mut Model, fx: &mut dyn Effects) {
    if !m.scan_available {
        return set_status(m, "此系統沒有可用的防毒掃描程式");
    }
    let targets: Vec<PathBuf> = match &m.mode {
        Mode::Archive { path, .. } => vec![path.clone()],
        Mode::Fs(_) => m.selected_items().into_iter().filter(|i| !i.is_dir).map(|i| PathBuf::from(i.full)).collect(),
    };
    if targets.is_empty() {
        return set_status(m, "請先開啟壓縮檔或選取要掃描的檔案");
    }
    spawn(m, fx, "防毒掃描中…", move |_| {
        let mut lines = Vec::new();
        for t in &targets {
            let name = t.file_name().unwrap_or_default().to_string_lossy().to_string();
            match crate::scan::scan_file(t) {
                Ok(crate::scan::ScanOutcome::Clean) => lines.push(format!("{name}：未發現威脅。")),
                Ok(crate::scan::ScanOutcome::Threat(s)) => return Msg::JobDone(Err(format!("{name}：發現威脅！\n{s}")), After::Nothing),
                Err(e) => return Msg::JobDone(Err(format!("{name}：{e}")), After::Nothing),
            }
        }
        Msg::JobDone(Ok(lines.join("\n")), After::Message)
    });
}

// ------------------------------------------------------------------ info / drops / clipboard

pub fn info_text(m: &Model) -> String {
    match &m.mode {
        Mode::Archive { path, fmt, entries, .. } => {
            let files = entries.iter().filter(|e| !e.is_dir).count();
            let dirs = entries.iter().filter(|e| e.is_dir).count();
            let unpacked: u64 = entries.iter().filter(|e| !e.is_dir).map(|e| e.size).sum();
            let packed = std::fs::metadata(path).map(|md| md.len()).unwrap_or(0);
            let ratio = if unpacked > 0 { packed as f64 * 100.0 / unpacked as f64 } else { 0.0 };
            let enc = entries.iter().any(|e| e.encrypted) || !m.password.is_empty();
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
            let files = m.items.iter().filter(|i| !i.is_dir).count();
            let total: u64 = m.items.iter().filter_map(|i| i.size).sum();
            format!("資料夾：{}\n檔案：{files}\n大小：{} 位元組\n\n開啟壓縮檔後可查看壓縮檔資訊。", dir.display(), view::fmt_num(total))
        }
    }
}

fn handle_drops(m: &mut Model, fx: &mut dyn Effects, drops: Vec<PathBuf>) {
    if drops.is_empty() || is_busy(m) {
        return;
    }
    let create_open = matches!(m.dialog, Some(Dialog::Create(_)));
    if m.dialog.is_some() && !create_open {
        return;
    }
    if drops.len() == 1 && drops[0].is_file() && Format::detect(&drops[0]).is_some() && !create_open {
        return open_archive(m, fx, drops[0].clone());
    }
    if !create_open && m.archive_writable() {
        let Mode::Archive { path, prefix, .. } = &m.mode else { return };
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let place = if prefix.is_empty() { name } else { format!("{name}\\{}", prefix.trim_end_matches('/').replace('/', "\\")) };
        if fx.confirm("加入檔案", &format!("要把 {} 個項目加入「{place}」嗎？", drops.len())) {
            add_to_archive(m, fx, drops);
        }
        return;
    }
    open_create(m, drops, None);
}

fn copy_to_clipboard(m: &mut Model, fx: &mut dyn Effects, paths: &[PathBuf]) {
    match fx.copy_files(paths) {
        Ok(()) => {
            // the previous staging copy is no longer on the clipboard: free its disk space now
            let staged = paths.first().and_then(|p| view::clip_dir_of(p));
            let old = std::mem::replace(&mut m.last_clip, staged.clone());
            if let Some(old) = old.filter(|o| Some(o) != staged.as_ref()) {
                fx.remove_dir_later(old);
            }
            set_status(m, format!("已複製 {} 個項目到剪貼簿，可在檔案總管中貼上", paths.len()))
        }
        Err(e) => {
            set_status(m, format!("錯誤：{e}"));
            fx.error(&e);
        }
    }
}

/// Write the cached bytes of a single archive entry into a fresh clipboard staging folder.
pub fn stage_bytes(bytes: &[u8], items: &[Item], single_file: bool) -> std::io::Result<Vec<PathBuf>> {
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
fn do_copy(m: &mut Model, fx: &mut dyn Effects) {
    let sel = m.selected_items();
    if sel.is_empty() {
        return set_status(m, "請先選取要複製的檔案");
    }
    let (arc, fmt) = match &m.mode {
        Mode::Archive { path, fmt, .. } => (path.clone(), *fmt),
        Mode::Fs(_) => {
            let paths: Vec<PathBuf> = sel.iter().map(|i| PathBuf::from(&i.full)).collect();
            return copy_to_clipboard(m, fx, &paths);
        }
    };
    // quick path: the preview already holds the complete entry
    if let [it] = sel.as_slice()
        && !it.is_dir
        && let Some(Ok(paths)) = m.pv.cache.as_ref().filter(|(a, n, _)| *a == arc && *n == it.full).map(|(_, _, b)| stage_bytes(b, &sel, fmt.is_single_file()))
    {
        return copy_to_clipboard(m, fx, &paths);
    }
    let names: Vec<String> = sel.iter().map(|i| i.full.clone()).collect();
    let pw = m.opt_pw();
    spawn(m, fx, "複製到剪貼簿…", move |p| {
        let dest = match view::new_clip_dir() {
            Ok(d) => d,
            Err(e) => return Msg::JobDone(Err(e.to_string()), After::Nothing),
        };
        let r = archiver_core::extract(&arc, &dest, pw.as_deref(), Some(&names), p).map_err(|e| e.to_string()).and_then(|_| {
            // only what was really written (entries core refused are not on disk)
            let paths: Vec<PathBuf> = view::extracted_paths(&dest, &sel, fmt.is_single_file()).into_iter().filter(|p| p.exists()).collect();
            if paths.is_empty() { Err("沒有可複製的檔案".to_string()) } else { Ok(paths) }
        });
        match r {
            Ok(paths) => Msg::JobDone(Ok(String::new()), After::CopyFiles(paths)),
            Err(e) => {
                let _ = std::fs::remove_dir_all(&dest);
                Msg::JobDone(Err(e), After::Nothing)
            }
        }
    });
}

// ------------------------------------------------------------------ job results

fn job_done(m: &mut Model, fx: &mut dyn Effects, r: Result<String, String>, after: After) {
    m.job = None;
    match r {
        Ok(text) => {
            m.status = text.clone();
            let batch_failed = matches!(after, After::Batch { failed, .. } if failed > 0);
            if m.close_when_done && !batch_failed {
                if after == After::Message {
                    fx.info("Archiver", &text);
                }
                return fx.close();
            }
            match after {
                After::Nothing => {}
                After::ReloadFs => {
                    if matches!(m.mode, Mode::Fs(_)) {
                        refresh(m, fx);
                        m.status = text;
                    }
                }
                After::ReloadArchive => {
                    if let Mode::Archive { path, .. } = &m.mode {
                        let p = path.clone();
                        reload_archive(m, fx, p);
                    }
                }
                After::OpenFile(p) => fx.open_path(&p),
                After::Message => fx.info("Archiver", &text),
                After::CopyFiles(paths) => copy_to_clipboard(m, fx, &paths),
                After::Batch { failed, retry } => {
                    let from_cli = std::mem::take(&mut m.close_when_done);
                    if matches!(m.mode, Mode::Fs(_)) {
                        refresh(m, fx);
                    }
                    m.status = text.lines().next().unwrap_or_default().to_string();
                    match retry.filter(|_| from_cli && failed == 1) {
                        // e.g. encrypted archive from the context menu: open it so a password can be entered
                        Some(p) => open_archive(m, fx, p),
                        None if failed > 0 => fx.error(&text),
                        None => {}
                    }
                }
            }
        }
        Err(e) => {
            let from_cli = std::mem::take(&mut m.close_when_done);
            let fallback = m.fallback_open.take();
            if is_cancel_err(&e) {
                set_status(m, "已取消");
                if from_cli {
                    fx.close();
                }
                return;
            }
            set_status(m, format!("錯誤：{e}"));
            match fallback.filter(|_| is_password_err(&e)) {
                // e.g. encrypted archive from the context menu: open it so a password can be entered
                Some(p) => open_archive(m, fx, p),
                None => fx.error(&e),
            }
        }
    }
}

// ------------------------------------------------------------------ preview pane

fn set_preview(m: &mut Model, view: PvView, title: &str, meta: &str) {
    m.pv.view = view;
    m.pv.title = title.to_string();
    m.pv.meta = meta.to_string();
}

/// Selection changed (or the pane was toggled / the listing reloaded): cancel the running
/// preview and show the new one (files are read after a short debounce).
pub fn selection_changed(m: &mut Model, fx: &mut dyn Effects) {
    m.pv.generation += 1;
    m.pv.pending = None;
    m.pv.scroll = 0.0;
    fx.cancel_preview();
    if !m.preview_visible {
        return;
    }
    let sel = m.selected_items();
    let it = match sel.as_slice() {
        [] => return set_preview(m, PvView::None, "", ""),
        [it] => it.clone(),
        many => {
            let bytes: u64 = many.iter().filter_map(|i| i.size).sum();
            let title = format!("已選取 {} 個項目", many.len());
            return set_preview(m, PvView::Info(format!("共 {} 個位元組", view::fmt_num(bytes))), &title, "");
        }
    };
    let meta = item_meta(&it);
    if it.is_dir {
        let text = match &m.mode {
            Mode::Archive { entries, .. } => folder_summary(entries, &it.full),
            Mode::Fs(_) => "檔案資料夾（按兩下開啟）".to_string(),
        };
        return set_preview(m, PvView::Folder(text), &it.name, &meta);
    }
    set_preview(m, PvView::Loading, &it.name, &meta);
    let src = match &m.mode {
        Mode::Archive { path, .. } => PvSrc::Entry { archive: path.clone(), name: it.full.clone(), password: m.opt_pw() },
        Mode::Fs(_) => PvSrc::File(PathBuf::from(&it.full)),
    };
    m.pv.pending = Some(PendingPreview { generation: m.pv.generation, due: Instant::now() + PREVIEW_DEBOUNCE, src, name: it.name.clone() });
}

fn preview_done(m: &mut Model, d: PreviewDone) {
    if d.generation != m.pv.generation {
        return;
    }
    if d.full.is_some() {
        m.pv.cache = d.full;
    }
    m.pv.view = match d.body {
        PvBody::Text(t) => PvView::Text(t),
        PvBody::Hex(h) => PvView::Hex(h),
        PvBody::Empty => PvView::Info("（空白檔案，0 位元組）".to_string()),
        PvBody::Image { width, height, rgba } => match Rgba8Image::new(ImageKey::new(PREVIEW_IMAGE_KEY + d.generation), 0, width, height, rgba) {
            Ok(img) => PvView::Image(img),
            Err(e) => PvView::Error(format!("無法預覽：{e}")),
        },
        PvBody::Error(e) => PvView::Error(if is_password_err(&e) { PREVIEW_PW_MSG.to_string() } else { format!("無法預覽：{e}") }),
    };
}

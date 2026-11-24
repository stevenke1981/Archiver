//! Everything the window shows, without any UI-toolkit types.

use std::path::{Path, PathBuf};
use std::time::Instant;

use acme_core::Rgba8Image;
use acme_textinput::TextInputState;
use archiver_core::{Entry, Format, Overwrite};

use crate::view::{self, Item};

/// Height of one file-list row in logical pixels (shared by layout, scrolling and paint).
pub const ROW_H: f32 = 24.0;

#[derive(Debug, Clone)]
pub enum Mode {
    Fs(PathBuf),
    Archive { path: PathBuf, fmt: Format, entries: Vec<Entry>, prefix: String },
}

/// Progress of the running background job.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct JobView {
    pub title: String,
    pub done: u64,
    pub total: u64,
    pub name: String,
}

impl JobView {
    pub fn fraction(&self) -> f32 {
        if self.total > 0 { (self.done as f32 / self.total as f32).min(1.0) } else { 0.0 }
    }
    pub fn text(&self) -> String {
        if self.name.is_empty() { self.title.clone() } else { format!("{} {}", self.title, self.name) }
    }
}

/// Open popup list (menu-bar dropdown, row context menu or the format picker).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Popup {
    Menu(usize),
    Context { x: f32, y: f32 },
    Formats { x: f32, y: f32, w: f32 },
}

/// Text fields of the in-app dialogs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Field {
    CreatePassword,
    CreateOutput,
    Password,
    Find,
    Comment,
    /// Rename dialog: the new name (last path component only).
    Rename,
    /// Select / deselect group dialog: the file mask(s).
    Mask,
    /// Convert dialog: optional password of the new archives.
    ConvertPassword,
}

#[derive(Debug, Clone)]
pub struct CreateDlg {
    /// Index into [`Format::writable`].
    pub format: usize,
    pub level: u32,
    pub each: bool,
    pub password: TextInputState,
    pub output: TextInputState,
    /// Test the new archive(s) after creating them (persisted in the settings).
    pub test_after: bool,
    /// Delete the source files after archiving (only after a successful create, and after a
    /// successful test when `test_after` is set). Never persisted.
    pub delete_after: bool,
}

impl CreateDlg {
    pub fn fmt(&self) -> Format {
        let all = Format::writable();
        all[self.format.min(all.len() - 1)]
    }
}

/// Convert dialog (Alt+Q): re-pack `sources` into `format`, next to each source.
#[derive(Debug, Clone)]
pub struct ConvertDlg {
    pub sources: Vec<PathBuf>,
    /// Index into [`Format::writable`].
    pub format: usize,
    pub level: u32,
    pub password: TextInputState,
}

impl ConvertDlg {
    pub fn fmt(&self) -> Format {
        let all = Format::writable();
        all[self.format.min(all.len() - 1)]
    }
}

#[derive(Debug, Clone)]
// Only one dialog exists at a time, so the size of the largest variant does not matter.
#[allow(clippy::large_enum_variant)]
pub enum Dialog {
    Create(CreateDlg),
    Password(TextInputState),
    Find(TextInputState),
    Comment { archive: PathBuf, text: TextInputState },
    Info(String),
    Shell { registered: bool },
    Favorites,
    /// F2. `from`: full entry name (archive) or full path (folder view, `fs == true`);
    /// `text`: the new last path component.
    Rename { from: String, fs: bool, text: TextInputState },
    /// Gray + (`select == true`) or Gray − mask dialog.
    SelectMask { select: bool, text: TextInputState },
    Convert(ConvertDlg),
}

impl Dialog {
    pub fn title(&self) -> &'static str {
        match self {
            Dialog::Create(d) if d.each => "分批壓縮",
            Dialog::Create(_) => "建立壓縮檔",
            Dialog::Password(_) => "輸入密碼",
            Dialog::Find(_) => "尋找檔案",
            Dialog::Comment { .. } => "壓縮檔註解",
            Dialog::Info(_) => "壓縮檔資訊",
            Dialog::Shell { .. } => "右鍵選單整合",
            Dialog::Favorites => "整理我的最愛",
            Dialog::Rename { .. } => "重新命名",
            Dialog::SelectMask { select: true, .. } => "選取群組",
            Dialog::SelectMask { select: false, .. } => "取消選取群組",
            Dialog::Convert(_) => "轉換壓縮檔",
        }
    }
    /// Text fields in Tab order.
    pub fn fields(&self) -> Vec<Field> {
        match self {
            Dialog::Create(d) if d.fmt().supports_password() => vec![Field::CreatePassword, Field::CreateOutput],
            Dialog::Create(_) => vec![Field::CreateOutput],
            Dialog::Password(_) => vec![Field::Password],
            Dialog::Find(_) => vec![Field::Find],
            Dialog::Comment { .. } => vec![Field::Comment],
            Dialog::Rename { .. } => vec![Field::Rename],
            Dialog::SelectMask { .. } => vec![Field::Mask],
            Dialog::Convert(d) if d.fmt().supports_password() => vec![Field::ConvertPassword],
            _ => vec![],
        }
    }
}

/// Where the preview data comes from.
#[derive(Debug, Clone, PartialEq)]
pub enum PvSrc {
    Entry { archive: PathBuf, name: String, password: Option<String> },
    File(PathBuf),
}

/// What the preview worker produced.
#[derive(Debug, Clone)]
pub enum PvBody {
    Text(String),
    Hex(String),
    /// Decoded RGBA8 pixels (straight alpha).
    Image { width: u32, height: u32, rgba: Vec<u8> },
    Empty,
    Error(String),
}

#[derive(Debug, Clone)]
pub struct PreviewDone {
    pub generation: u64,
    pub body: PvBody,
    /// Complete bytes of an archive entry: (archive, entry name, bytes).
    pub full: Option<(PathBuf, String, Vec<u8>)>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PvView {
    None,
    Loading,
    Text(String),
    Hex(String),
    Image(Rgba8Image),
    Folder(String),
    Info(String),
    Error(String),
}

/// A preview read that waits for the selection to settle (debounce).
#[derive(Debug, Clone)]
pub struct PendingPreview {
    pub generation: u64,
    pub due: Instant,
    pub src: PvSrc,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct PreviewState {
    pub generation: u64,
    pub view: PvView,
    pub title: String,
    pub meta: String,
    /// Vertical scroll offset of the preview body (logical px).
    pub scroll: f32,
    pub pending: Option<PendingPreview>,
    /// Last fully read archive entry, for the quick copy path.
    pub cache: Option<(PathBuf, String, Vec<u8>)>,
}

impl Default for PreviewState {
    fn default() -> Self {
        PreviewState { generation: 0, view: PvView::None, title: String::new(), meta: String::new(), scroll: 0.0, pending: None, cache: None }
    }
}

/// What to do in the UI thread after a background job succeeds.
#[derive(Debug, Clone, PartialEq)]
pub enum After {
    Nothing,
    ReloadFs,
    ReloadArchive,
    OpenFile(PathBuf),
    Message,
    /// Put these (extracted) files on the clipboard.
    CopyFiles(Vec<PathBuf>),
    /// Batch create/extract finished. `retry`: the single archive that failed because of a
    /// password (opened instead when started from the CLI).
    Batch { failed: usize, retry: Option<PathBuf> },
}

#[derive(Debug, Clone)]
pub struct Model {
    pub mode: Mode,
    pub items: Vec<Item>,
    pub selected: Vec<bool>,
    pub anchor: Option<usize>,
    pub sort_col: i32,
    pub sort_asc: bool,
    pub filter: String,
    /// Session password (password dialog).
    pub password: String,
    /// Archive the password belongs to; None = entered in folder mode, not bound yet.
    pub password_for: Option<PathBuf>,
    pub job: Option<JobView>,
    pub create_inputs: Vec<PathBuf>,
    /// Remembered create-dialog format index.
    pub last_format: usize,
    pub close_when_done: bool,
    pub fallback_open: Option<PathBuf>,
    pub status: String,
    /// Staging folder of the files currently on the clipboard (removed when replaced).
    pub last_clip: Option<PathBuf>,
    pub pv: PreviewState,
    pub preview_visible: bool,
    pub dialog: Option<Dialog>,
    pub focus: Option<Field>,
    pub popup: Option<Popup>,
    /// File-list scroll offset and viewport height (logical px; the height is set by the UI).
    pub list_scroll: f32,
    pub list_view_h: f32,
    pub favorites: Vec<PathBuf>,
    pub scan_available: bool,
    /// Overwrite policy of every extraction (Options menu, persisted).
    pub overwrite: Overwrite,
    /// Default of the create dialog's "test after archiving" checkbox (persisted).
    pub test_after: bool,
    /// Recently opened archives, newest first (File menu, persisted).
    pub recent: Vec<PathBuf>,
    /// Last mask of the select / deselect group dialog.
    pub last_mask: String,
}

impl Model {
    pub fn new(start_dir: PathBuf) -> Self {
        Model {
            mode: Mode::Fs(start_dir),
            items: vec![],
            selected: vec![],
            anchor: None,
            sort_col: 0,
            sort_asc: true,
            filter: String::new(),
            password: String::new(),
            password_for: None,
            job: None,
            create_inputs: vec![],
            last_format: 0,
            close_when_done: false,
            fallback_open: None,
            status: String::new(),
            last_clip: None,
            pv: PreviewState::default(),
            preview_visible: true,
            dialog: None,
            focus: None,
            popup: None,
            list_scroll: 0.0,
            list_view_h: 400.0,
            favorites: vec![],
            scan_available: false,
            overwrite: Overwrite::Replace,
            test_after: false,
            recent: vec![],
            last_mask: "*.*".to_string(),
        }
    }

    pub fn in_archive(&self) -> bool {
        matches!(self.mode, Mode::Archive { .. })
    }
    pub fn busy(&self) -> bool {
        self.job.is_some()
    }
    pub fn archive_fmt(&self) -> Option<Format> {
        match &self.mode {
            Mode::Archive { fmt, .. } => Some(*fmt),
            Mode::Fs(_) => None,
        }
    }
    /// The open archive can be modified (delete / add).
    pub fn archive_writable(&self) -> bool {
        self.archive_fmt().is_some_and(|f| f.can_create() && !f.is_single_file())
    }
    pub fn can_delete(&self) -> bool {
        self.archive_writable()
    }
    pub fn is_zip_open(&self) -> bool {
        self.archive_fmt() == Some(Format::Zip)
    }
    pub fn can_extract_selection(&self) -> bool {
        view::can_extract_selection(self.in_archive(), &self.items, &self.selected)
    }
    pub fn selected_items(&self) -> Vec<Item> {
        self.items.iter().zip(&self.selected).filter(|(i, x)| **x && !i.is_parent).map(|(i, _)| i.clone()).collect()
    }
    pub fn opt_pw(&self) -> Option<String> {
        Some(self.password.clone()).filter(|s| !s.is_empty())
    }
    /// Folder or archive shown right now (for favorites).
    pub fn location(&self) -> PathBuf {
        match &self.mode {
            Mode::Fs(d) => d.clone(),
            Mode::Archive { path, .. } => path.clone(),
        }
    }

    pub fn window_title(&self) -> String {
        match &self.mode {
            Mode::Fs(_) => "Archiver".to_string(),
            Mode::Archive { path, .. } => format!("{} - Archiver", path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()),
        }
    }

    pub fn path_text(&self) -> String {
        match &self.mode {
            Mode::Fs(dir) => dir.display().to_string(),
            Mode::Archive { path, fmt, entries, prefix } => {
                let unpacked: u64 = entries.iter().filter(|e| !e.is_dir).map(|e| e.size).sum();
                let label = fmt.label().split(' ').next().unwrap_or("").to_string();
                let mut pt = view::archive_path_text(path, prefix, &label, unpacked);
                if !self.filter.is_empty() {
                    pt = format!("{pt}  （尋找：{}）", self.filter);
                }
                pt
            }
        }
    }

    pub fn status_left(&self) -> String {
        if !self.status.is_empty() {
            return self.status.lines().next().unwrap_or_default().to_string();
        }
        let sel = self.selected_items();
        if sel.is_empty() {
            return String::new();
        }
        let b: u64 = sel.iter().filter_map(|i| i.size).sum();
        format!("已選取 {} 個項目, {} 個位元組", sel.len(), view::fmt_num(b))
    }

    pub fn status_right(&self) -> String {
        let files: Vec<&Item> = self.items.iter().filter(|i| !i.is_dir).collect();
        let total: u64 = files.iter().filter_map(|i| i.size).sum();
        let dirs = self.items.iter().filter(|i| i.is_dir && !i.is_parent).count();
        if dirs > 0 {
            format!("總共 {dirs} 個資料夾和 {} 個檔案, {} 個位元組", files.len(), view::fmt_num(total))
        } else {
            format!("總共 {} 個檔案, {} 個位元組", files.len(), view::fmt_num(total))
        }
    }

    /// Create dialog: the chosen single-file format cannot hold the inputs.
    pub fn create_warning(&self) -> bool {
        let Some(Dialog::Create(d)) = &self.dialog else { return false };
        let warn = if d.each { self.create_inputs.iter().any(|p| p.is_dir()) } else { self.create_inputs.len() != 1 };
        d.fmt().is_single_file() && warn
    }

    pub fn max_list_scroll(&self) -> f32 {
        (self.items.len() as f32 * ROW_H - self.list_view_h).max(0.0)
    }

    pub fn clamp_list_scroll(&mut self) {
        self.list_scroll = self.list_scroll.clamp(0.0, self.max_list_scroll());
    }

    /// Scroll the list so that row `i` is fully visible.
    pub fn ensure_row_visible(&mut self, i: usize) {
        let top = i as f32 * ROW_H;
        if top < self.list_scroll {
            self.list_scroll = top;
        } else if top + ROW_H > self.list_scroll + self.list_view_h {
            self.list_scroll = top + ROW_H - self.list_view_h;
        }
        self.clamp_list_scroll();
    }

    /// Rows that fit into the list viewport (at least one).
    pub fn page_rows(&self) -> usize {
        ((self.list_view_h / ROW_H).floor() as usize).max(1)
    }

    pub fn focused_input(&mut self) -> Option<&mut TextInputState> {
        let focus = self.focus?;
        self.input_mut(focus)
    }

    pub fn input_mut(&mut self, field: Field) -> Option<&mut TextInputState> {
        match (field, self.dialog.as_mut()?) {
            (Field::CreatePassword, Dialog::Create(d)) => Some(&mut d.password),
            (Field::CreateOutput, Dialog::Create(d)) => Some(&mut d.output),
            (Field::Password, Dialog::Password(t)) | (Field::Find, Dialog::Find(t)) | (Field::Comment, Dialog::Comment { text: t, .. }) => Some(t),
            (Field::Rename, Dialog::Rename { text: t, .. }) | (Field::Mask, Dialog::SelectMask { text: t, .. }) => Some(t),
            (Field::ConvertPassword, Dialog::Convert(d)) => Some(&mut d.password),
            _ => None,
        }
    }

    /// The input state of `field` in the open dialog.
    pub fn input(&self, field: Field) -> Option<&TextInputState> {
        match (field, self.dialog.as_ref()?) {
            (Field::CreatePassword, Dialog::Create(d)) => Some(&d.password),
            (Field::CreateOutput, Dialog::Create(d)) => Some(&d.output),
            (Field::Password, Dialog::Password(t)) | (Field::Find, Dialog::Find(t)) | (Field::Comment, Dialog::Comment { text: t, .. }) => Some(t),
            (Field::Rename, Dialog::Rename { text: t, .. }) | (Field::Mask, Dialog::SelectMask { text: t, .. }) => Some(t),
            (Field::ConvertPassword, Dialog::Convert(d)) => Some(&d.password),
            _ => None,
        }
    }

    /// A text field of the open dialog has keyboard focus (IME should be enabled).
    pub fn text_focus(&self) -> bool {
        self.focus.is_some_and(|f| self.input(f).is_some())
    }
}

/// A fresh text input with `text` and the cursor at its end.
pub fn text_input(text: &str, password: bool) -> TextInputState {
    let mut t = TextInputState::new();
    t.password = password;
    set_input_text(&mut t, text);
    t
}

pub fn set_input_text(t: &mut TextInputState, text: &str) {
    t.text = text.to_string();
    t.move_cursor_to_offset(t.text.len());
    t.clear_history();
}

/// "12,345 位元組　2026/10/10 12:00　CRC32 0000ABCD"
pub fn item_meta(it: &Item) -> String {
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
pub fn folder_summary(entries: &[Entry], dir: &str) -> String {
    let prefix = format!("{}/", dir.trim_end_matches('/'));
    let (mut files, mut bytes, mut dirs) = (0usize, 0u64, std::collections::BTreeSet::new());
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

/// `path`, or `name (2).ext`… when it already exists.
pub fn unique_path(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let stem = archiver_core::archive_stem(path);
    let ext = name.strip_prefix(&stem).unwrap_or("").to_string();
    (2..10_000).map(|n| dir.join(format!("{stem} ({n}){ext}"))).find(|p| !p.exists()).unwrap_or_else(|| path.to_path_buf())
}

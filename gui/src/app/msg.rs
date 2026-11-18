//! Everything that can happen to the model: user intents and background results.

use std::path::PathBuf;
use std::time::Instant;

use acme_platform::PlatformKey;
use archiver_core::Entry;

use super::model::{After, Field, PreviewDone};

/// Commands of the menus, toolbar, context menu and keyboard shortcuts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmd {
    Open,
    /// Create a new archive (create dialog).
    NewArchive,
    /// Toolbar "加入": add to the open archive, or create a new one.
    Add,
    AddEach,
    Extract,
    ExtractSelected,
    ExtractAll,
    ExtractHere,
    ExtractEach,
    Test,
    View,
    Copy,
    CopyText,
    Delete,
    Find,
    Wizard,
    Info,
    Refresh,
    SelectAll,
    Up,
    Password,
    Shell,
    About,
    Exit,
    TogglePreview,
    /// Context menu "預覽…": make sure the pane is visible.
    ShowPreview,
    Comment,
    Scan,
    Cancel,
    FavAdd,
    FavManage,
    FavOpen(usize),
}

#[derive(Debug, Clone, PartialEq)]
pub struct KeyPress {
    pub key: PlatformKey,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    /// Text produced by the key (may be a control character while Ctrl is held).
    pub text: Option<String>,
    /// The key's character, unaffected by Ctrl (for shortcuts).
    pub character: Option<String>,
}

impl KeyPress {
    #[cfg(test)]
    pub fn plain(key: PlatformKey) -> Self {
        KeyPress { key, ctrl: false, shift: false, alt: false, text: None, character: None }
    }
    #[cfg(test)]
    pub fn ctrl_char(c: &str) -> Self {
        KeyPress { key: PlatformKey::Other, ctrl: true, shift: false, alt: false, text: None, character: Some(c.into()) }
    }
    /// Lower-case shortcut character.
    pub fn shortcut(&self) -> Option<String> {
        self.character.as_deref().or(self.text.as_deref()).map(str::to_lowercase)
    }
}

/// Dialog interactions.
#[derive(Debug, Clone, PartialEq)]
pub enum DlgMsg {
    Ok,
    Cancel,
    /// Focus a text field; `Some(offset)` also moves the caret to that byte offset.
    Focus(Field, Option<usize>),
    OpenFormats { x: f32, y: f32, w: f32 },
    SetFormat(usize),
    SetLevel(u32),
    LevelStep(i32),
    ToggleEach,
    Browse,
    ToggleShell,
    FindClear,
    FavOpen(usize),
    FavRemove(usize),
}

#[derive(Debug, Clone)]
pub enum Msg {
    Cmd(Cmd),
    RowPressed { index: usize, ctrl: bool, shift: bool },
    RowActivated(usize),
    RowContext { index: usize, x: f32, y: f32 },
    ClearSelection,
    Sort(i32),
    /// Scroll the file list by this many logical pixels (positive = down).
    ScrollList(f32),
    ScrollListTo(f32),
    ScrollPreview(f32),
    Key(KeyPress),
    /// Typed or IME-committed text for the focused field.
    Text(String),
    Preedit(String, Option<(usize, usize)>),
    Dropped(Vec<PathBuf>),
    OpenMenu(usize),
    /// Pointer moved over another menu title while a menu is open.
    HoverMenu(usize),
    ClosePopup,
    Tick(Instant),
    Dialog(DlgMsg),
    // ---- background results
    Listed(PathBuf, Result<Vec<Entry>, String>),
    JobDone(Result<String, String>, After),
    PreviewDone(PreviewDone),
}

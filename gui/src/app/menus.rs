//! Contents of the menu bar dropdowns and the row context menu (pure, tested).

use archiver_core::Format;

use super::model::{Model, Popup};
use super::msg::{Cmd, DlgMsg, Msg};

pub const MENU_TITLES: [&str; 6] = ["檔案(F)", "指令(C)", "工具(S)", "我的最愛(O)", "選項(N)", "說明(H)"];
/// Alt+letter opens the menu with this index.
pub const MENU_KEYS: [&str; 6] = ["f", "c", "s", "o", "n", "h"];

#[derive(Debug, Clone)]
pub struct MenuEntry {
    pub label: String,
    pub shortcut: &'static str,
    /// `None` for separators.
    pub msg: Option<Msg>,
    pub enabled: bool,
    pub checked: Option<bool>,
}

impl MenuEntry {
    fn item(label: impl Into<String>, msg: Msg) -> Self {
        MenuEntry { label: label.into(), shortcut: "", msg: Some(msg), enabled: true, checked: None }
    }
    fn cmd(label: &str, cmd: Cmd) -> Self {
        Self::item(label, Msg::Cmd(cmd))
    }
    fn sep() -> Self {
        MenuEntry { label: String::new(), shortcut: "", msg: None, enabled: false, checked: None }
    }
    fn key(mut self, s: &'static str) -> Self {
        self.shortcut = s;
        self
    }
    fn on(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
    fn check(mut self, c: bool) -> Self {
        self.checked = Some(c);
        self
    }
    pub fn is_separator(&self) -> bool {
        self.msg.is_none() && self.label.is_empty()
    }
}

/// Entries of the open popup.
pub fn popup_entries(m: &Model, popup: Popup) -> Vec<MenuEntry> {
    match popup {
        Popup::Menu(i) => menu_entries(m, i),
        Popup::Context { .. } => context_entries(m),
        Popup::Formats { .. } => {
            let cur = match &m.dialog {
                Some(super::model::Dialog::Create(d)) => d.format,
                _ => usize::MAX,
            };
            Format::writable().iter().enumerate().map(|(i, f)| MenuEntry::item(f.label(), Msg::Dialog(DlgMsg::SetFormat(i))).check(i == cur)).collect()
        }
    }
}

pub fn menu_entries(m: &Model, menu: usize) -> Vec<MenuEntry> {
    let busy = m.busy();
    let arc = m.in_archive();
    match menu {
        0 => vec![
            MenuEntry::cmd("開啟壓縮檔…", Cmd::Open).key("Ctrl+O"),
            MenuEntry::cmd("新增壓縮檔…", Cmd::NewArchive),
            MenuEntry::sep(),
            MenuEntry::cmd("壓縮檔密碼…", Cmd::Password),
            MenuEntry::cmd("全選", Cmd::SelectAll).key("Ctrl+A"),
            MenuEntry::sep(),
            MenuEntry::cmd("結束", Cmd::Exit),
        ],
        1 => vec![
            MenuEntry::cmd("加入檔案到壓縮檔…", Cmd::Add).on(!busy && (!arc || m.archive_writable())),
            MenuEntry::cmd("分批壓縮（各自建立壓縮檔）…", Cmd::AddEach).on(!busy),
            MenuEntry::sep(),
            MenuEntry::cmd("解壓縮至…", Cmd::Extract).on(!busy),
            MenuEntry::cmd("解壓縮選取項目…", Cmd::ExtractSelected).on(!busy && m.can_extract_selection()),
            MenuEntry::cmd("解壓縮全部…", Cmd::ExtractAll).on(!busy && arc),
            MenuEntry::cmd("解壓縮到這裡", Cmd::ExtractHere).on(!busy),
            MenuEntry::cmd("分別解壓縮到各自資料夾", Cmd::ExtractEach).on(!busy),
            MenuEntry::sep(),
            MenuEntry::cmd("測試壓縮檔", Cmd::Test).on(!busy),
            MenuEntry::cmd("檢視檔案", Cmd::View).key("F3"),
            MenuEntry::cmd("複製到剪貼簿", Cmd::Copy).key("Ctrl+C").on(!busy),
            MenuEntry::cmd("刪除檔案", Cmd::Delete).key("Del").on(!busy && m.can_delete()),
            MenuEntry::sep(),
            MenuEntry::cmd("註解…", Cmd::Comment).on(!busy && m.is_zip_open()),
            MenuEntry::cmd("防毒掃描", Cmd::Scan).on(!busy && m.scan_available),
        ],
        2 => vec![
            MenuEntry::cmd("精靈", Cmd::Wizard).on(!busy),
            MenuEntry::cmd("尋找檔案…", Cmd::Find).key("Ctrl+F"),
            MenuEntry::cmd("重新整理", Cmd::Refresh).key("F5"),
            MenuEntry::cmd("顯示資訊", Cmd::Info),
        ],
        3 => {
            let mut v = vec![MenuEntry::cmd("加入目前位置到我的最愛", Cmd::FavAdd), MenuEntry::cmd("整理我的最愛…", Cmd::FavManage), MenuEntry::sep()];
            if m.favorites.is_empty() {
                v.push(MenuEntry { label: "（尚無項目）".into(), shortcut: "", msg: Some(Msg::ClosePopup), enabled: false, checked: None });
            }
            for (i, f) in m.favorites.iter().enumerate().take(20) {
                v.push(MenuEntry::cmd(&f.display().to_string(), Cmd::FavOpen(i)));
            }
            v
        }
        4 => vec![MenuEntry::cmd("預覽窗格", Cmd::TogglePreview).check(m.preview_visible), MenuEntry::cmd("右鍵選單整合…", Cmd::Shell)],
        _ => vec![MenuEntry::cmd("關於 Archiver", Cmd::About)],
    }
}

/// Right-click menu of the file list (the selection was already adjusted).
pub fn context_entries(m: &Model) -> Vec<MenuEntry> {
    let busy = m.busy();
    let arc = m.in_archive();
    let sel = m.selected_items();
    let any_file = sel.iter().any(|i| !i.is_dir);
    let mut v = vec![
        MenuEntry::cmd("解壓縮至…", Cmd::Extract).on(!busy && (arc || m.can_extract_selection())),
        MenuEntry::cmd("解壓縮選取項目…", Cmd::ExtractSelected).on(!busy && m.can_extract_selection()),
        MenuEntry::cmd("解壓縮全部…", Cmd::ExtractAll).on(!busy && arc),
    ];
    if !arc {
        v.push(MenuEntry::cmd("加入壓縮檔…", Cmd::NewArchive).on(!busy && !sel.is_empty()));
    }
    v.extend([
        MenuEntry::sep(),
        MenuEntry::cmd("複製", Cmd::Copy).key("Ctrl+C").on(!busy && !sel.is_empty()),
        MenuEntry::cmd("刪除", Cmd::Delete).key("Del").on(!busy && m.can_delete() && !sel.is_empty()),
        MenuEntry::cmd("檢視", Cmd::View).key("F3").on(any_file),
        MenuEntry::cmd("預覽…", Cmd::ShowPreview).on(sel.len() == 1),
    ]);
    v
}

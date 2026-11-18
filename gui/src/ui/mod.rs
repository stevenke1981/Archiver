//! Immediate-mode painter for the main window: bands (menu, toolbar, path, list, preview,
//! status), dialogs and popups. Each frame records hit regions that map back to [`Msg`]s.

pub mod icons;
pub mod text;
mod dialogs;
mod widgets;

use acme_core::{Color, CursorHint, HitRegionSpec, HitTestLayer, Logical, NodeId, Painter, Rect, Scene};
use acme_theme::{Theme, ThemeColor};

use crate::app::menus::{self, MENU_TITLES};
use crate::app::{Cmd, Field, Model, Msg, Popup, PvView, ROW_H};
use crate::view;
use icons::{Icon, Icons};
use text::{Font, Text};

/// Hit id of list row 0 (row ids are stable across frames).
pub const ROW_BASE: u64 = 1 << 32;
pub const MENU_H: f32 = 28.0;
pub const TOOL_H: f32 = 74.0;
pub const PATH_H: f32 = 38.0;
pub const HEADER_H: f32 = 28.0;
pub const STATUS_H: f32 = 28.0;
const SCROLLBAR_W: f32 = 10.0;
const POPUP_ITEM_H: f32 = 28.0;
const POPUP_SEP_H: f32 = 9.0;

pub const BODY: Font = Font::new(13.0);
pub const SMALL: Font = Font::new(12.0);
pub const TITLE: Font = Font::new(16.0).bold();
pub const HEX: Font = Font::new(12.0).mono();

pub type R = Rect<Logical>;

pub fn rect(x: f32, y: f32, w: f32, h: f32) -> R {
    Rect::new(x, y, w.max(0.0), h.max(0.0))
}
pub fn rx(r: &R) -> (f32, f32, f32, f32) {
    (r.origin.x.get(), r.origin.y.get(), r.size.width.get(), r.size.height.get())
}
fn contains(r: &R, (x, y): (f32, f32)) -> bool {
    r.contains(acme_core::Point::new(x, y))
}
pub fn col(c: ThemeColor) -> Color {
    Color::rgba(c.red, c.green, c.blue, c.alpha)
}

/// What a hit region does when it is pressed or clicked.
#[derive(Debug, Clone)]
pub enum Hit {
    /// Send this message on click (press + release on the same region).
    Click(Msg),
    Row(usize),
    MenuTitle(usize),
    /// Outside an open popup: closes it.
    Dismiss,
    /// Swallows the pointer (modal scrim, disabled popup items).
    Block,
    Field(Field, R),
    LevelTrack { x: f32, w: f32, lo: u32, hi: u32 },
    ListBlank,
    ListScroll { track_y: f32, track_h: f32 },
}

/// Fixed bands of the main window for a given size.
#[derive(Debug, Clone, Copy)]
pub struct Geometry {
    pub w: f32,
    pub h: f32,
    pub menubar: R,
    pub toolbar: R,
    pub pathbar: R,
    pub header: R,
    /// Row viewport of the file list.
    pub list: R,
    pub preview: Option<R>,
    pub status: R,
}

pub fn geometry(w: f32, h: f32, preview_visible: bool) -> Geometry {
    let top = MENU_H + TOOL_H + PATH_H;
    let body_h = (h - top - STATUS_H).max(HEADER_H + ROW_H);
    let pw = if preview_visible { (w - 480.0).clamp(0.0, 300.0) } else { 0.0 };
    let pw = if pw < 200.0 { 0.0 } else { pw };
    let lw = w - pw;
    Geometry {
        w,
        h,
        menubar: rect(0.0, 0.0, w, MENU_H),
        toolbar: rect(0.0, MENU_H, w, TOOL_H),
        pathbar: rect(0.0, MENU_H + TOOL_H, w, PATH_H),
        header: rect(0.0, top, lw, HEADER_H),
        list: rect(0.0, top + HEADER_H, lw, body_h - HEADER_H),
        preview: (pw > 0.0).then(|| rect(lw, top, pw, body_h)),
        status: rect(0.0, h - STATUS_H, w, STATUS_H),
    }
}

/// A file-list column: id (sort key, see [`view::sort_items`]), title, x, width, right-aligned.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Column {
    pub id: i32,
    pub title: &'static str,
    pub x: f32,
    pub w: f32,
    pub right: bool,
}

/// Columns that fit into `width`; 封裝後 / CRC32 only exist for archives and are dropped
/// first on narrow windows, then 類型 and 修改日期.
pub fn columns(width: f32, in_archive: bool) -> Vec<Column> {
    const MIN_NAME: f32 = 180.0;
    let all: [(i32, &str, f32, bool); 5] = [(1, "大小", 92.0, true), (2, "封裝後", 92.0, true), (3, "類型", 130.0, false), (4, "修改日期", 132.0, false), (5, "CRC32", 84.0, false)];
    let mut keep: Vec<(i32, &str, f32, bool)> = all.into_iter().filter(|c| in_archive || !matches!(c.0, 2 | 5)).collect();
    for drop in [5, 2, 3, 4] {
        let fixed: f32 = keep.iter().map(|c| c.2).sum();
        if width - SCROLLBAR_W - fixed >= MIN_NAME {
            break;
        }
        keep.retain(|c| c.0 != drop);
    }
    let fixed: f32 = keep.iter().map(|c| c.2).sum();
    let name_w = (width - SCROLLBAR_W - fixed).max(80.0);
    let mut out = vec![Column { id: 0, title: "名稱", x: 0.0, w: name_w, right: false }];
    let mut x = name_w;
    for (id, title, w, right) in keep {
        out.push(Column { id, title, x, w, right });
        x += w;
    }
    out
}

/// Toolbar button: icon, label, command, enabled, checked (None = separator).
type Tool = (Icon, &'static str, Cmd, bool, bool);

/// Lines of the text preview laid out for one width.
struct PvLines {
    key: (u64, u32),
    lines: Vec<(f32, String)>,
    total: f32,
}

pub struct Ui {
    pub text: Text,
    icons: Icons,
    pub theme: Theme,
    pub layer: HitTestLayer,
    /// Hit regions of the last frame by id. Ids must not shift between a press and its
    /// release (the frame in between may enable buttons), so rows use [`ROW_BASE`] + index
    /// and disabled controls still take their id (as [`Hit::Block`]).
    hits: Vec<(NodeId, Hit)>,
    next_id: u64,
    /// Pointer position (logical) for hover effects.
    pub pointer: Option<(f32, f32)>,
    /// Region currently held down.
    pub pressed: Option<NodeId>,
    pub scale: f32,
    menu_titles: Vec<R>,
    pub ime_rect: Option<[f32; 4]>,
    /// Largest useful preview scroll offset of the last frame.
    pub pv_max_scroll: f32,
    pv_lines: Option<PvLines>,
    z: i32,
    level: u8,
    top_level: u8,
}

impl Ui {
    pub fn new(theme: Theme) -> Self {
        Ui {
            text: Text::new(),
            icons: Icons::default(),
            theme,
            layer: HitTestLayer::new(),
            hits: vec![],
            next_id: 1,
            pointer: None,
            pressed: None,
            scale: 1.0,
            menu_titles: vec![],
            ime_rect: None,
            pv_max_scroll: 0.0,
            pv_lines: None,
            z: 0,
            level: 0,
            top_level: 0,
        }
    }

    pub fn hit(&self, id: NodeId) -> Option<&Hit> {
        self.hits.iter().find(|(i, _)| *i == id).map(|(_, h)| h)
    }

    /// Index of the menu-bar title under (x, y).
    pub fn menu_title_at(&self, x: f32, y: f32) -> Option<usize> {
        self.menu_titles.iter().position(|r| contains(r, (x, y)))
    }

    fn push_hit(&mut self, r: R, hit: Hit, cursor: CursorHint) -> NodeId {
        let id = NodeId::new(self.next_id);
        self.next_id += 1;
        self.push_hit_id(id, r, hit, cursor)
    }

    fn push_hit_id(&mut self, id: NodeId, r: R, hit: Hit, cursor: CursorHint) -> NodeId {
        self.layer.push_region(HitRegionSpec::rect(id, r).cursor(cursor).z_layer(self.z));
        self.hits.push((id, hit));
        id
    }

    fn click(&mut self, r: R, msg: Msg) -> NodeId {
        self.push_hit(r, Hit::Click(msg), CursorHint::Pointer)
    }

    /// Pointer is over `r` and nothing modal covers this layer.
    fn hover(&self, r: &R) -> bool {
        self.level == self.top_level && self.pointer.is_some_and(|p| contains(r, p))
    }

    fn icon_px(&self, logical: f32) -> u32 {
        (logical * self.scale).round() as u32
    }

    fn icon(&mut self, p: &mut Painter, icon: Icon, x: f32, y: f32, size: f32, faded: bool) {
        if let Some(img) = self.icons.get(icon, self.icon_px(size), faded) {
            p.image(rect(x, y, size, size), img);
        }
    }

    /// Paint the whole window.
    pub fn paint(&mut self, m: &Model, w: f32, h: f32, scale: f32) -> Scene {
        self.scale = scale;
        self.text.begin_frame(scale);
        self.layer.clear();
        self.hits.clear();
        self.next_id = 1;
        self.ime_rect = None;
        self.top_level = if m.popup.is_some() { 2 } else if m.dialog.is_some() { 1 } else { 0 };
        let g = geometry(w, h, m.preview_visible);
        let c = self.theme.colors;
        let mut p = Painter::with_clear(col(c.background));

        self.level = 0;
        self.z = 0;
        self.menubar(&mut p, m, &g);
        self.toolbar(&mut p, m, &g);
        self.pathbar(&mut p, m, &g);
        self.list(&mut p, m, &g);
        if let Some(pr) = g.preview {
            self.preview(&mut p, m, pr);
        }
        self.statusbar(&mut p, m, &g);
        if m.dialog.is_some() {
            self.level = 1;
            self.z = 30;
            self.dialog(&mut p, m, &g);
        }
        if let Some(popup) = m.popup {
            self.level = 2;
            self.z = 40;
            self.popup(&mut p, m, popup, &g);
        }
        p.finish()
    }

    // ------------------------------------------------------------------ bands

    fn menubar(&mut self, p: &mut Painter, m: &Model, g: &Geometry) {
        let c = self.theme.colors;
        let (_, y, w, h) = rx(&g.menubar);
        p.fill_rect(g.menubar, col(c.surface));
        p.fill_rect(rect(0.0, y + h - 1.0, w, 1.0), col(c.border));
        self.menu_titles.clear();
        let mut x = 4.0;
        for (i, t) in MENU_TITLES.iter().enumerate() {
            let tw = self.text.width(t, BODY) + 20.0;
            let r = rect(x, y + 2.0, tw, h - 4.0);
            let open = m.popup == Some(Popup::Menu(i));
            if open {
                p.rounded_rect(r, 4.0, col(c.accent));
            } else if self.hover(&r) {
                p.rounded_rect(r, 4.0, col(c.ghost_hover));
            }
            let fg = if open { c.accent_foreground } else { c.foreground };
            self.text.draw_line(p, t, x + 10.0, y, h, BODY, col(fg));
            self.push_hit(r, Hit::MenuTitle(i), CursorHint::Default);
            self.menu_titles.push(r);
            x += tw + 2.0;
        }
    }

    fn toolbar(&mut self, p: &mut Painter, m: &Model, g: &Geometry) {
        let c = self.theme.colors;
        let (_, y, w, h) = rx(&g.toolbar);
        p.fill_rect(g.toolbar, col(c.surface));
        p.fill_rect(rect(0.0, y + h - 1.0, w, 1.0), col(c.border));
        let busy = m.busy();
        let sel = m.selected_items();
        let has_file = sel.iter().any(|i| !i.is_dir);
        let tools: [Option<Tool>; 13] = [
            Some((Icon::Add, "加入", Cmd::Add, !busy && (!m.in_archive() || m.archive_writable()), false)),
            Some((Icon::Extract, "解壓縮到", Cmd::Extract, !busy, false)),
            Some((Icon::Test, "測試", Cmd::Test, !busy, false)),
            Some((Icon::View, "檢視", Cmd::View, has_file && !busy, false)),
            Some((Icon::Delete, "刪除", Cmd::Delete, m.can_delete() && !busy && !sel.is_empty(), false)),
            Some((Icon::Find, "尋找", Cmd::Find, true, false)),
            Some((Icon::Wizard, "精靈", Cmd::Wizard, !busy, false)),
            Some((Icon::Info, "資訊", Cmd::Info, true, false)),
            Some((Icon::Preview, "預覽", Cmd::TogglePreview, true, m.preview_visible)),
            None,
            Some((Icon::Copy, "複製", Cmd::Copy, !busy && !sel.is_empty(), false)),
            Some((Icon::Antivirus, "防毒", Cmd::Scan, m.scan_available && !busy, false)),
            Some((Icon::Comment, "註解", Cmd::Comment, m.is_zip_open() && !busy, false)),
        ];
        let (bw, bh) = (62.0, h - 10.0);
        let mut x = 8.0;
        for t in tools {
            let Some((icon, label, cmd, enabled, checked)) = t else {
                p.fill_rect(rect(x + 5.0, y + 12.0, 1.0, h - 24.0), col(c.border));
                x += 11.0;
                continue;
            };
            if x + bw > w {
                break;
            }
            let r = rect(x, y + 5.0, bw, bh);
            let hovered = enabled && self.hover(&r);
            let id = if enabled { Some(self.click(r, Msg::Cmd(cmd))) } else { self.push_hit(r, Hit::Block, CursorHint::Default); None };
            let pressed = id.is_some() && id == self.pressed;
            if pressed {
                p.rounded_rect_bordered(r, 6.0, col(c.ghost_pressed), 1.0, col(c.border));
            } else if checked {
                p.rounded_rect_bordered(r, 6.0, col(c.accent), 1.0, col(c.accent_foreground.with_alpha(0.35)));
            } else if hovered {
                p.rounded_rect_bordered(r, 6.0, col(c.ghost_hover), 1.0, col(c.border));
            }
            self.icon(p, icon, x + (bw - 32.0) / 2.0, y + 9.0, 32.0, !enabled);
            let fg = if enabled { c.foreground } else { c.disabled_text };
            let tw = self.text.width(label, SMALL);
            self.text.draw_line(p, label, x + (bw - tw) / 2.0, y + 43.0, 20.0, SMALL, col(fg));
            x += bw + 2.0;
        }
    }

    fn pathbar(&mut self, p: &mut Painter, m: &Model, g: &Geometry) {
        let c = self.theme.colors;
        let (_, y, w, h) = rx(&g.pathbar);
        p.fill_rect(g.pathbar, col(c.background));
        p.fill_rect(rect(0.0, y + h - 1.0, w, 1.0), col(c.border));
        let up = rect(8.0, y + 5.0, 30.0, h - 10.0);
        if self.hover(&up) {
            p.rounded_rect(up, 5.0, col(c.ghost_hover));
        }
        self.click(up, Msg::Cmd(Cmd::Up));
        self.icon(p, Icon::Up, 15.0, y + (h - 16.0) / 2.0, 16.0, false);
        let field = rect(44.0, y + 5.0, w - 52.0, h - 10.0);
        p.rounded_rect_bordered(field, 5.0, col(c.surface), 1.0, col(c.input));
        let icon = if m.in_archive() { Icon::Archive } else { Icon::Folder };
        self.icon(p, icon, 52.0, y + (h - 16.0) / 2.0, 16.0, false);
        let text = self.text.elide(&m.path_text(), BODY, w - 52.0 - 40.0);
        self.text.draw_line(p, &text, 74.0, y, h, BODY, col(c.foreground));
    }

    fn list(&mut self, p: &mut Painter, m: &Model, g: &Geometry) {
        let c = self.theme.colors;
        let (hx, hy, hw, hh) = rx(&g.header);
        let cols = columns(hw, m.in_archive());
        p.fill_rect(g.header, col(c.surface));
        p.fill_rect(rect(hx, hy + hh - 1.0, hw, 1.0), col(c.border));
        for cl in &cols {
            let r = rect(cl.x, hy, cl.w, hh - 1.0);
            if self.hover(&r) {
                p.fill_rect(r, col(c.ghost_hover));
            }
            p.fill_rect(rect(cl.x + cl.w - 1.0, hy + 6.0, 1.0, hh - 12.0), col(c.border));
            let sorted = m.sort_col == cl.id;
            let f = if sorted { SMALL.bold() } else { SMALL };
            let tw = self.text.width(cl.title, f);
            let tx = if cl.right { cl.x + cl.w - tw - 18.0 } else { cl.x + 8.0 };
            self.text.draw_line(p, cl.title, tx, hy, hh, f, col(c.muted_foreground));
            if sorted {
                let arrow = if m.sort_asc { "▲" } else { "▼" };
                let ax = if cl.right { cl.x + cl.w - 14.0 } else { tx + tw + 6.0 };
                self.text.draw_line(p, arrow, ax, hy, hh, Font::new(8.0), col(c.muted_foreground));
            }
            self.click(r, Msg::Sort(cl.id));
        }

        let (lx, ly, lw, lh) = rx(&g.list);
        p.fill_rect(g.list, col(c.surface));
        let content_h = m.items.len() as f32 * ROW_H;
        let scroll = m.list_scroll.clamp(0.0, (content_h - lh).max(0.0));
        let row_w = lw - SCROLLBAR_W;
        p.push_clip(g.list);
        self.layer.push_clip(g.list);
        let first = (scroll / ROW_H).floor() as usize;
        let count = (lh / ROW_H).ceil() as usize + 1;
        let (sel_bg, sel_fg) = (c.accent, c.foreground);
        for i in first..(first + count).min(m.items.len()) {
            let it = &m.items[i];
            let y = ly + i as f32 * ROW_H - scroll;
            let r = rect(lx, y, row_w, ROW_H);
            let selected = m.selected.get(i).copied().unwrap_or(false);
            if selected {
                p.fill_rect(r, col(sel_bg));
            } else if self.hover(&r) {
                p.fill_rect(r, col(c.ghost_hover));
            }
            if m.anchor == Some(i) && m.dialog.is_none() {
                p.stroke_rect(rect(lx + 1.0, y, row_w - 2.0, ROW_H), 1.0, col(c.ring.with_alpha(0.6)));
            }
            self.push_hit_id(NodeId::new(ROW_BASE + i as u64), r, Hit::Row(i), CursorHint::Default);
            let fg = if selected { sel_fg } else { c.foreground };
            let muted = if selected { sel_fg } else { c.muted_foreground };
            for cl in &cols {
                let (text, color) = match cl.id {
                    0 => (String::new(), fg),
                    1 => (it.size.map(view::fmt_num).unwrap_or_default(), fg),
                    2 => (it.packed.map(view::fmt_num).unwrap_or_default(), muted),
                    3 => (if it.is_parent { "檔案資料夾".to_string() } else { view::type_name(&it.name, it.is_dir) }, muted),
                    4 => (it.modified.replace('-', "/"), muted),
                    _ => (it.crc.map(|v| format!("{v:08X}")).unwrap_or_default(), muted),
                };
                if cl.id == 0 {
                    self.icon(p, Icon::for_kind(view::icon(it)), lx + 8.0, y + (ROW_H - 16.0) / 2.0, 16.0, false);
                    let name = self.text.elide(&it.name, BODY, cl.w - 40.0);
                    self.text.draw_line(p, &name, lx + 30.0, y, ROW_H, BODY, col(fg));
                    continue;
                }
                if text.is_empty() {
                    continue;
                }
                let f = if cl.id == 5 { SMALL.mono() } else { SMALL };
                let t = self.text.elide(&text, f, cl.w - 16.0);
                let tw = self.text.width(&t, f);
                let tx = if cl.right { cl.x + cl.w - tw - 18.0 } else { cl.x + 8.0 };
                self.text.draw_line(p, &t, tx, y, ROW_H, f, col(color));
            }
        }
        let rows_end = ly + content_h - scroll;
        if rows_end < ly + lh {
            self.push_hit(rect(lx, rows_end.max(ly), row_w, ly + lh - rows_end.max(ly)), Hit::ListBlank, CursorHint::Default);
        }
        if m.items.iter().all(|i| i.is_parent) {
            let msg = if m.filter.is_empty() { "（沒有項目）" } else { "（沒有符合的項目）" };
            let tw = self.text.width(msg, BODY);
            self.text.draw_line(p, msg, lx + (row_w - tw) / 2.0, ly + ROW_H * 2.0, ROW_H, BODY, col(c.muted_foreground));
        }
        self.layer.pop_clip();
        p.pop_clip();
        // scrollbar
        let track = rect(lx + lw - SCROLLBAR_W, ly, SCROLLBAR_W, lh);
        p.fill_rect(track, col(c.muted));
        if content_h > lh {
            let th = (lh * lh / content_h).max(24.0);
            let ty = ly + (lh - th) * (scroll / (content_h - lh));
            let thumb = rect(lx + lw - SCROLLBAR_W + 2.0, ty, SCROLLBAR_W - 4.0, th);
            let hot = self.hover(&track);
            p.rounded_rect(thumb, 3.0, col(if hot { c.muted_foreground } else { c.border.darken(0.15) }));
            self.push_hit(track, Hit::ListScroll { track_y: ly, track_h: lh }, CursorHint::Default);
        }
    }

    fn preview(&mut self, p: &mut Painter, m: &Model, r: R) {
        let c = self.theme.colors;
        let (x, y, w, h) = rx(&r);
        p.fill_rect(r, col(c.background));
        p.fill_rect(rect(x, y, 1.0, h), col(c.border));
        let (ix, iw) = (x + 12.0, w - 24.0);
        let mut cy = y + 10.0;
        let title = if m.pv.title.is_empty() { "預覽" } else { m.pv.title.as_str() };
        let title = self.text.elide(title, BODY.bold(), iw);
        self.text.draw_line(p, &title, ix, cy, 22.0, BODY.bold(), col(c.foreground));
        cy += 24.0;
        if !m.pv.meta.is_empty() {
            cy += self.text.draw_block(p, &m.pv.meta, ix, cy, iw, SMALL, col(c.muted_foreground)) + 4.0;
        }
        let btn_h = 30.0;
        let body = rect(ix, cy + 2.0, iw, (y + h - 12.0 - btn_h - 8.0) - (cy + 2.0));
        let (bx, by, bw, bh) = rx(&body);
        p.rounded_rect_bordered(body, 6.0, col(c.surface), 1.0, col(c.border));
        let inner = rect(bx + 1.0, by + 1.0, bw - 2.0, bh - 2.0);
        p.push_clip(inner);
        self.pv_max_scroll = 0.0;
        match &m.pv.view {
            PvView::None | PvView::Loading => {
                let t = if m.pv.view == PvView::Loading { "載入中…" } else { "選取檔案即可預覽" };
                let tw = self.text.width(t, BODY);
                self.text.draw_line(p, t, bx + (bw - tw) / 2.0, by, bh, BODY, col(c.muted_foreground));
            }
            PvView::Text(t) => self.preview_text(p, m, t, inner),
            PvView::Hex(t) => self.preview_hex(p, m, t, inner),
            PvView::Image(img) => p.image(rect(bx + 8.0, by + 8.0, bw - 16.0, bh - 16.0), img.clone()),
            PvView::Folder(t) | PvView::Info(t) => {
                self.text.draw_block(p, t, bx + 10.0, by + 10.0, bw - 20.0, BODY, col(c.foreground));
            }
            PvView::Error(t) => {
                self.text.draw_block(p, t, bx + 10.0, by + 10.0, bw - 20.0, BODY, col(c.danger));
            }
        }
        p.pop_clip();
        let v = &m.pv.view;
        let bwid = (iw - 12.0) / 3.0;
        let yb = y + h - 12.0 - btn_h;
        let some = !matches!(v, PvView::None);
        let texty = matches!(v, PvView::Text(_) | PvView::Hex(_));
        let openable = !matches!(v, PvView::None | PvView::Folder(_) | PvView::Info(_));
        self.button(p, rect(ix, yb, bwid, btn_h), "複製檔案", some && !m.busy(), false, Msg::Cmd(Cmd::Copy));
        self.button(p, rect(ix + bwid + 6.0, yb, bwid, btn_h), "複製文字", texty, false, Msg::Cmd(Cmd::CopyText));
        self.button(p, rect(ix + 2.0 * (bwid + 6.0), yb, bwid, btn_h), "開啟", openable && !m.busy(), false, Msg::Cmd(Cmd::View));
    }

    fn preview_text(&mut self, p: &mut Painter, m: &Model, t: &str, inner: R) {
        let c = self.theme.colors;
        let (x, y, w, h) = rx(&inner);
        let (tx, tw) = (x + 8.0, w - 16.0);
        let key = (m.pv.generation, tw.to_bits());
        if self.pv_lines.as_ref().is_none_or(|l| l.key != key) {
            let mut lines = Vec::new();
            let mut top = 0.0;
            for line in t.split('\n') {
                lines.push((top, line.to_string()));
                top += if line.is_empty() { BODY.line_height() } else { self.text.height(line, BODY, tw) };
            }
            self.pv_lines = Some(PvLines { key, lines, total: top });
        }
        let Some(pl) = self.pv_lines.take() else { return };
        self.pv_max_scroll = (pl.total + 16.0 - h).max(0.0);
        let scroll = m.pv.scroll.min(self.pv_max_scroll);
        let start = pl.lines.partition_point(|(top, _)| *top + 200.0 < scroll);
        for (top, line) in &pl.lines[start.saturating_sub(1)..] {
            let ly = y + 8.0 + top - scroll;
            if ly > y + h {
                break;
            }
            self.text.draw_block(p, line, tx, ly, tw, BODY, col(c.foreground));
        }
        self.pv_lines = Some(pl);
    }

    fn preview_hex(&mut self, p: &mut Painter, m: &Model, t: &str, inner: R) {
        let c = self.theme.colors;
        let (x, y, _, h) = rx(&inner);
        let lh = HEX.line_height();
        let lines: Vec<&str> = t.lines().collect();
        self.pv_max_scroll = (lines.len() as f32 * lh + 16.0 - h).max(0.0);
        let scroll = m.pv.scroll.min(self.pv_max_scroll);
        let first = (scroll / lh).floor() as usize;
        for (i, line) in lines.iter().enumerate().skip(first).take((h / lh) as usize + 2) {
            let ly = y + 8.0 + i as f32 * lh - scroll;
            self.text.draw_line(p, line, x + 8.0, ly, lh, HEX, col(c.foreground));
        }
    }

    fn statusbar(&mut self, p: &mut Painter, m: &Model, g: &Geometry) {
        let c = self.theme.colors;
        let (_, y, w, h) = rx(&g.status);
        p.fill_rect(g.status, col(c.muted));
        p.fill_rect(rect(0.0, y, w, 1.0), col(c.border));
        if let Some(job) = &m.job {
            let tw = (w * 0.4).min(420.0);
            let t = self.text.elide(&job.text(), SMALL, tw);
            self.text.draw_line(p, &t, 10.0, y, h, SMALL, col(c.foreground));
            let bar = rect(20.0 + tw, y + 9.0, (w - tw - 110.0).max(40.0), 10.0);
            p.rounded_rect_bordered(bar, 5.0, col(c.surface), 1.0, col(c.border));
            let (bx, by, bw, bh) = rx(&bar);
            if job.total > 0 {
                p.rounded_rect(rect(bx, by, bw * job.fraction(), bh), 5.0, col(c.primary));
            }
            self.button(p, rect(w - 78.0, y + 3.0, 70.0, h - 6.0), "取消", true, false, Msg::Cmd(Cmd::Cancel));
            return;
        }
        let right = m.status_right();
        let rw = self.text.width(&right, SMALL).min(w * 0.5);
        let right = self.text.elide(&right, SMALL, rw + 1.0);
        self.text.draw_line(p, &right, w - rw - 12.0, y, h, SMALL, col(c.muted_foreground));
        let left = self.text.elide(&m.status_left(), SMALL, w - rw - 40.0);
        let fg = if m.status.starts_with("錯誤") || m.status.starts_with("無法") { c.danger } else { c.foreground };
        self.text.draw_line(p, &left, 10.0, y, h, SMALL, col(fg));
    }

    // ------------------------------------------------------------------ popups

    fn popup(&mut self, p: &mut Painter, m: &Model, popup: Popup, g: &Geometry) {
        let c = self.theme.colors;
        let entries = menus::popup_entries(m, popup);
        self.push_hit(rect(0.0, 0.0, g.w, g.h), Hit::Dismiss, CursorHint::Default);
        self.z = 41;
        let (ax, ay, min_w) = match popup {
            Popup::Menu(i) => {
                let r = self.menu_titles.get(i).copied().unwrap_or(rect(0.0, 0.0, 0.0, MENU_H));
                (r.origin.x.get(), MENU_H, 220.0)
            }
            Popup::Context { x, y } => (x, y, 210.0),
            Popup::Formats { x, y, w } => (x, y, w),
        };
        let mut width: f32 = min_w;
        let mut height = 8.0;
        for e in &entries {
            if e.is_separator() {
                height += POPUP_SEP_H;
            } else {
                let sw = if e.shortcut.is_empty() { 0.0 } else { self.text.width(e.shortcut, SMALL) + 24.0 };
                width = width.max(self.text.width(&e.label, BODY) + sw + 56.0);
                height += POPUP_ITEM_H;
            }
        }
        let width = width.min(g.w - 8.0);
        let x = ax.min(g.w - width - 4.0).max(4.0);
        let y = if ay + height > g.h - 4.0 { (ay - height).max(4.0) } else { ay };
        let card = rect(x, y, width, height);
        p.rounded_rect(rect(x + 1.0, y + 3.0, width, height), 8.0, Color::rgba(0.0, 0.0, 0.0, 0.10));
        p.rounded_rect_bordered(card, 8.0, col(c.surface_elevated), 1.0, col(c.border));
        self.push_hit(card, Hit::Block, CursorHint::Default);
        let mut cy = y + 4.0;
        for e in entries {
            if e.is_separator() {
                p.fill_rect(rect(x + 10.0, cy + 4.0, width - 20.0, 1.0), col(c.border));
                cy += POPUP_SEP_H;
                continue;
            }
            let r = rect(x + 4.0, cy, width - 8.0, POPUP_ITEM_H);
            if e.enabled && self.hover(&r) {
                p.rounded_rect(r, 5.0, col(c.accent));
            }
            let fg = if e.enabled { c.foreground } else { c.disabled_text };
            if e.checked == Some(true) {
                self.text.draw_line(p, "✓", x + 12.0, cy, POPUP_ITEM_H, BODY, col(c.primary));
            }
            let label = self.text.elide(&e.label, BODY, width - 60.0);
            self.text.draw_line(p, &label, x + 32.0, cy, POPUP_ITEM_H, BODY, col(fg));
            if !e.shortcut.is_empty() {
                let sw = self.text.width(e.shortcut, SMALL);
                self.text.draw_line(p, e.shortcut, x + width - sw - 14.0, cy, POPUP_ITEM_H, SMALL, col(c.muted_foreground));
            }
            match e.msg.filter(|_| e.enabled) {
                Some(msg) => {
                    self.click(r, msg);
                }
                None => {
                    self.push_hit(r, Hit::Block, CursorHint::Default);
                }
            }
            cy += POPUP_ITEM_H;
        }
    }
}

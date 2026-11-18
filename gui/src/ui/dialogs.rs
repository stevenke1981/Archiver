//! In-app modal dialogs: create archive, password, find, comment, info, shell, favorites.

use acme_core::{Color, CursorHint, Painter};
use archiver_core::Format;

use super::{BODY, Geometry, Hit, R, SMALL, TITLE, Ui, col, rect, rx};
use crate::app::{Dialog, DlgMsg, Field, Model, Msg};

const PAD: f32 = 22.0;
const ROW: f32 = 34.0;
const GAP: f32 = 10.0;
const LABEL_W: f32 = 92.0;

impl Ui {
    pub(super) fn dialog(&mut self, p: &mut Painter, m: &Model, g: &Geometry) {
        let Some(d) = &m.dialog else { return };
        p.fill_rect(rect(0.0, 0.0, g.w, g.h), Color::rgba(0.0, 0.0, 0.0, 0.32));
        self.push_hit(rect(0.0, 0.0, g.w, g.h), Hit::Block, CursorHint::Default);
        let width: f32 = match d {
            Dialog::Create(_) => 600.0,
            Dialog::Comment { .. } | Dialog::Favorites => 560.0,
            _ => 460.0,
        };
        let width = width.min(g.w - 24.0);
        let iw = width - 2.0 * PAD;
        // content height
        let body_h = match d {
            Dialog::Create(_) => 20.0 + 74.0 + GAP + 28.0 + GAP + 3.0 * (ROW + GAP) + if m.create_warning() { 24.0 } else { 0.0 } + ROW,
            Dialog::Password(_) | Dialog::Find(_) => 22.0 + 6.0 + ROW,
            Dialog::Comment { .. } => 22.0 + 6.0 + 220.0 + 20.0,
            Dialog::Info(t) => self.text.height(t, BODY, iw),
            Dialog::Shell { .. } => {
                let desc = shell_description();
                self.text.height(desc, BODY, iw) + GAP + 28.0 + 4.0 + 20.0
            }
            Dialog::Favorites => (m.favorites.len().clamp(1, 8) as f32) * 34.0,
        };
        let height = PAD + 30.0 + body_h + 18.0 + 34.0 + PAD;
        let x = ((g.w - width) / 2.0).round();
        let y = ((g.h - height) / 2.0).max(8.0).round();
        let c = self.theme.colors;
        p.rounded_rect(rect(x + 2.0, y + 6.0, width, height), 12.0, Color::rgba(0.0, 0.0, 0.0, 0.12));
        p.rounded_rect_bordered(rect(x, y, width, height), 12.0, col(c.surface_overlay), 1.0, col(c.border));
        let (ix, mut cy) = (x + PAD, y + PAD);
        self.text.draw_line(p, d.title(), ix, cy, 26.0, TITLE, col(c.foreground));
        cy += 30.0;
        let buttons_y = y + height - PAD - 34.0;
        match d {
            Dialog::Create(dlg) => {
                let label = |s: &mut Self, p: &mut Painter, y: f32, t: &str| s.label(p, ix, y, ROW, t, col(c.foreground));
                self.text.draw_line(p, "來源（可拖曳更多檔案進來）：", ix, cy, 20.0, SMALL, col(c.muted_foreground));
                cy += 20.0;
                let src = rect(ix, cy, iw, 74.0);
                p.rounded_rect_bordered(src, 6.0, col(c.muted), 1.0, col(c.border));
                let n = m.create_inputs.len();
                for (k, f) in m.create_inputs.iter().take(3).enumerate() {
                    let t = self.text.elide(&f.display().to_string(), SMALL, iw - 16.0);
                    self.text.draw_line(p, &t, ix + 8.0, cy + 4.0 + k as f32 * 20.0, 20.0, SMALL, col(c.foreground));
                }
                if n > 3 {
                    self.text.draw_line(p, &format!("…還有 {} 個", n - 3), ix + 8.0, cy + 4.0 + 60.0 - 8.0, 16.0, SMALL, col(c.muted_foreground));
                }
                cy += 74.0 + GAP;
                self.checkbox(p, ix, cy, 28.0, "每個檔案／資料夾分別建立壓縮檔（分批壓縮）", dlg.each, true, Msg::Dialog(DlgMsg::ToggleEach));
                cy += 28.0 + GAP;
                // format
                label(self, p, cy, "格式：");
                let fr = rect(ix + LABEL_W, cy, 220.0, ROW);
                let open = matches!(m.popup, Some(crate::app::Popup::Formats { .. }));
                self.field_box(p, fr, open);
                self.text.draw_line(p, dlg.fmt().label(), ix + LABEL_W + 10.0, cy, ROW, BODY, col(c.foreground));
                self.text.draw_line(p, "▾", ix + LABEL_W + 200.0, cy, ROW, BODY, col(c.muted_foreground));
                let (fx_, fy, fw, fh) = rx(&fr);
                self.click(fr, Msg::Dialog(DlgMsg::OpenFormats { x: fx_, y: fy + fh + 2.0, w: fw }));
                cy += ROW + GAP;
                // level
                label(self, p, cy, "壓縮等級：");
                self.level_slider(p, m, rect(ix + LABEL_W, cy, iw - LABEL_W, ROW), dlg.fmt(), dlg.level);
                cy += ROW + GAP;
                // password
                label(self, p, cy, "密碼：");
                let pw_ok = dlg.fmt().supports_password();
                let ph = if pw_ok { "（選用，AES-256）" } else { "此格式不支援密碼" };
                self.input(p, rect(ix + LABEL_W, cy, iw - LABEL_W, ROW), Field::CreatePassword, &dlg.password, pw_ok, ph);
                cy += ROW + GAP;
                if m.create_warning() {
                    self.text.draw_line(p, "⚠ 此格式只能壓縮單一檔案，多個檔案請用 TAR.* 格式", ix + LABEL_W, cy - 4.0, 22.0, SMALL, col(c.warning));
                    cy += 24.0;
                }
                label(self, p, cy, if dlg.each { "輸出資料夾：" } else { "輸出：" });
                self.input(p, rect(ix + LABEL_W, cy, iw - LABEL_W - 96.0, ROW), Field::CreateOutput, &dlg.output, true, "");
                self.button(p, rect(ix + iw - 88.0, cy, 88.0, ROW), "瀏覽…", true, false, Msg::Dialog(DlgMsg::Browse));
                let ok = !m.create_inputs.is_empty() && !dlg.output.text.trim().is_empty() && !m.busy();
                self.buttons(p, x + width - PAD, buttons_y, &[("確定", ok, true, DlgMsg::Ok), ("取消", true, false, DlgMsg::Cancel)]);
            }
            Dialog::Password(t) => {
                self.text.draw_line(p, "壓縮檔密碼（用於開啟、解壓縮與測試）：", ix, cy, 22.0, BODY, col(c.foreground));
                self.input(p, rect(ix, cy + 28.0, iw, ROW), Field::Password, t, true, "");
                self.buttons(p, x + width - PAD, buttons_y, &[("確定", true, true, DlgMsg::Ok), ("取消", true, false, DlgMsg::Cancel)]);
            }
            Dialog::Find(t) => {
                let q = if m.in_archive() { "在整個壓縮檔中尋找名稱包含：" } else { "在目前資料夾中尋找名稱包含：" };
                self.text.draw_line(p, q, ix, cy, 22.0, BODY, col(c.foreground));
                self.input(p, rect(ix, cy + 28.0, iw, ROW), Field::Find, t, true, "例如 .txt 或 報告");
                self.buttons(p, x + width - PAD, buttons_y, &[("尋找", true, true, DlgMsg::Ok), ("清除", true, false, DlgMsg::FindClear), ("取消", true, false, DlgMsg::Cancel)]);
            }
            Dialog::Comment { archive, text } => {
                let name = archive.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                let t = self.text.elide(&format!("{name} 的註解（ZIP）："), BODY, iw);
                self.text.draw_line(p, &t, ix, cy, 22.0, BODY, col(c.foreground));
                self.textarea(p, rect(ix, cy + 28.0, iw, 220.0), Field::Comment, text);
                self.text.draw_line(p, "Enter 換行，Ctrl+Enter 確定", ix, cy + 28.0 + 222.0, 18.0, SMALL, col(c.muted_foreground));
                self.buttons(p, x + width - PAD, buttons_y, &[("確定", !m.busy(), true, DlgMsg::Ok), ("取消", true, false, DlgMsg::Cancel)]);
            }
            Dialog::Info(t) => {
                self.text.draw_block(p, t, ix, cy, iw, BODY, col(c.foreground));
                self.buttons(p, x + width - PAD, buttons_y, &[("確定", true, true, DlgMsg::Ok)]);
            }
            Dialog::Shell { registered } => {
                let desc = shell_description();
                cy += self.text.draw_block(p, desc, ix, cy, iw, BODY, col(c.foreground)) + GAP;
                self.checkbox(p, ix, cy, 28.0, "在檔案總管 / 檔案管理員的右鍵選單中顯示 Archiver", *registered, true, Msg::Dialog(DlgMsg::ToggleShell));
                cy += 32.0;
                self.text.draw_line(p, "若搬移了程式位置，請重新啟用一次。", ix, cy, 20.0, SMALL, col(c.muted_foreground));
                self.buttons(p, x + width - PAD, buttons_y, &[("關閉", true, true, DlgMsg::Ok)]);
            }
            Dialog::Favorites => {
                if m.favorites.is_empty() {
                    self.text.draw_line(p, "尚無項目。用「我的最愛 → 加入目前位置到我的最愛」加入資料夾或壓縮檔。", ix, cy, 34.0, SMALL, col(c.muted_foreground));
                }
                for (i, f) in m.favorites.iter().enumerate().take(8) {
                    let r = rect(ix, cy, iw - 86.0, 30.0);
                    if self.hover(&r) {
                        p.rounded_rect(r, 5.0, col(c.ghost_hover));
                    }
                    let icon = if f.is_dir() { super::Icon::Folder } else { super::Icon::Archive };
                    self.icon(p, icon, ix + 6.0, cy + 7.0, 16.0, false);
                    let t = self.text.elide(&f.display().to_string(), BODY, iw - 120.0);
                    self.text.draw_line(p, &t, ix + 28.0, cy, 30.0, BODY, col(c.foreground));
                    self.click(r, Msg::Dialog(DlgMsg::FavOpen(i)));
                    self.button(p, rect(ix + iw - 78.0, cy, 78.0, 30.0), "移除", true, false, Msg::Dialog(DlgMsg::FavRemove(i)));
                    cy += 34.0;
                }
                self.buttons(p, x + width - PAD, buttons_y, &[("關閉", true, true, DlgMsg::Ok)]);
            }
        }
    }

    /// Right-aligned dialog buttons ending at `right`.
    fn buttons(&mut self, p: &mut Painter, right: f32, y: f32, bs: &[(&str, bool, bool, DlgMsg)]) {
        let mut x = right;
        for (label, enabled, primary, msg) in bs.iter().rev() {
            let w = (self.text.width(label, BODY) + 36.0).max(88.0);
            x -= w;
            self.button(p, rect(x, y, w, 34.0), label, *enabled, *primary, Msg::Dialog(msg.clone()));
            x -= 8.0;
        }
    }

    fn field_box(&mut self, p: &mut Painter, r: R, active: bool) {
        let c = self.theme.colors;
        let border = if active { c.ring } else if self.hover(&r) { c.muted_foreground } else { c.input.darken(0.12) };
        p.rounded_rect_bordered(r, 6.0, col(c.surface), if active { 2.0 } else { 1.0 }, col(border));
    }

    fn level_slider(&mut self, p: &mut Painter, m: &Model, r: R, fmt: Format, level: u32) {
        let c = self.theme.colors;
        let (x, y, w, h) = rx(&r);
        let Some((lo, hi, _)) = fmt.level_range() else {
            self.text.draw_line(p, "（此格式不壓縮）", x, y, h, BODY, col(c.muted_foreground));
            return;
        };
        let _ = m;
        let minus = rect(x, y + 3.0, 28.0, h - 6.0);
        self.button(p, minus, "−", level > lo, false, Msg::Dialog(DlgMsg::LevelStep(-1)));
        let tx = x + 36.0;
        let tw = w - 36.0 - 36.0 - 56.0;
        let track = rect(tx, y + h / 2.0 - 3.0, tw, 6.0);
        p.rounded_rect(track, 3.0, col(c.muted.darken(0.08)));
        let frac = if hi > lo { (level - lo) as f32 / (hi - lo) as f32 } else { 0.0 };
        p.rounded_rect(rect(tx, y + h / 2.0 - 3.0, tw * frac, 6.0), 3.0, col(c.primary));
        let knob = rect(tx + tw * frac - 8.0, y + h / 2.0 - 8.0, 16.0, 16.0);
        let hot = self.hover(&rect(tx - 8.0, y, tw + 16.0, h));
        p.rounded_rect_bordered(knob, 8.0, col(c.surface), 2.0, col(if hot { c.primary_hover } else { c.primary }));
        self.push_hit(rect(tx - 8.0, y, tw + 16.0, h), Hit::LevelTrack { x: tx, w: tw, lo, hi }, CursorHint::Pointer);
        self.button(p, rect(tx + tw + 8.0, y + 3.0, 28.0, h - 6.0), "+", level < hi, false, Msg::Dialog(DlgMsg::LevelStep(1)));
        let label = format!("{level}");
        self.text.draw_line(p, &label, tx + tw + 48.0, y, h, BODY.bold(), col(c.foreground));
    }
}

fn shell_description() -> &'static str {
    if cfg!(windows) {
        "Windows 檔案總管右鍵選單（目前使用者 HKCU，不需系統管理員）：開啟、解壓縮至…、解壓縮到這裡、解壓縮到資料夾、加入壓縮檔、測試、壓縮成 .zip / .7z。Windows 11 請在「顯示其他選項」（Shift+右鍵）中找到。"
    } else if cfg!(target_os = "macos") {
        "Finder 快速動作（~/Library/Services）。"
    } else {
        "Nautilus 腳本、Dolphin 服務選單、Nemo 動作與 .desktop MIME 關聯。"
    }
}

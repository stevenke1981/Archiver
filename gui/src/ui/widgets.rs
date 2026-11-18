//! Small controls drawn with the theme tokens: buttons, check boxes and text fields.

use acme_core::{Color, CursorHint, Painter};
use acme_textinput::{TextInputState, byte_offset_to_x};

use super::text::{Font, style};
use super::{BODY, Hit, R, Ui, col, rect, rx};
use crate::app::{Field, Msg};

impl Ui {
    pub(super) fn button(&mut self, p: &mut Painter, r: R, label: &str, enabled: bool, primary: bool, msg: Msg) {
        let c = self.theme.colors;
        let hovered = enabled && self.hover(&r);
        let id = enabled.then(|| self.click(r, msg));
        let pressed = id.is_some() && id == self.pressed;
        let (bg, border, fg) = match (enabled, primary) {
            (false, _) => (c.disabled_bg, c.border, c.disabled_text),
            (true, true) => (if pressed { c.primary_pressed } else if hovered { c.primary_hover } else { c.primary }, c.primary, c.primary_foreground),
            (true, false) => (if pressed { c.ghost_pressed } else if hovered { c.ghost_hover } else { c.surface }, c.border, c.foreground),
        };
        p.rounded_rect_bordered(r, 6.0, col(bg), 1.0, col(border));
        let (x, y, w, h) = rx(&r);
        let label = self.text.elide(label, BODY, w - 12.0);
        let tw = self.text.width(&label, BODY);
        self.text.draw_line(p, &label, x + (w - tw) / 2.0, y, h, BODY, col(fg));
    }

    /// Check box with a label; returns the width used.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn checkbox(&mut self, p: &mut Painter, x: f32, y: f32, h: f32, label: &str, checked: bool, enabled: bool, msg: Msg) -> f32 {
        let c = self.theme.colors;
        let tw = self.text.width(label, BODY);
        let r = rect(x, y, tw + 30.0, h);
        let hovered = enabled && self.hover(&r);
        if enabled {
            self.click(r, msg);
        }
        let b = rect(x, y + (h - 16.0) / 2.0, 16.0, 16.0);
        if checked {
            p.rounded_rect(b, 4.0, col(if enabled { c.primary } else { c.disabled_text }));
            let (bx, by, _, _) = rx(&b);
            let white = col(c.primary_foreground);
            p.line(acme_core::Point::new(bx + 3.5, by + 8.5), acme_core::Point::new(bx + 6.5, by + 11.5), 2.0, white);
            p.line(acme_core::Point::new(bx + 6.5, by + 11.5), acme_core::Point::new(bx + 12.5, by + 4.5), 2.0, white);
        } else {
            p.rounded_rect_bordered(b, 4.0, col(c.surface), 1.5, col(if hovered { c.ring } else { c.muted_foreground }));
        }
        let fg = if enabled { c.foreground } else { c.disabled_text };
        self.text.draw_line(p, label, x + 24.0, y, h, BODY, col(fg));
        tw + 30.0
    }

    fn field_frame(&mut self, p: &mut Painter, r: R, focused: bool, enabled: bool) {
        let c = self.theme.colors;
        let bg = if enabled { c.surface } else { c.disabled_bg };
        if focused {
            p.rounded_rect_bordered(r, 6.0, col(bg), 2.0, col(c.ring));
        } else {
            let border = if enabled && self.hover(&r) { c.muted_foreground } else { c.input.darken(0.12) };
            p.rounded_rect_bordered(r, 6.0, col(bg), 1.0, col(border));
        }
    }

    /// Single-line text field.
    pub(super) fn input(&mut self, p: &mut Painter, r: R, field: Field, s: &TextInputState, enabled: bool, placeholder: &str) {
        let c = self.theme.colors;
        let focused = s.focused && enabled;
        self.field_frame(p, r, focused, enabled);
        if enabled {
            self.push_hit(r, Hit::Field(field, r), CursorHint::Text);
        }
        let (x, y, w, h) = rx(&r);
        let (ix, iw) = (x + 8.0, w - 16.0);
        let f = BODY;
        // display text: masked for passwords, preedit shown inline at the caret
        let cursor = s.cursor().min(s.text.len());
        let (disp, caret, pre) = if s.password {
            let n = s.text[..cursor].chars().count();
            (s.display_text(), n * s.password_char.len_utf8(), None)
        } else if !s.preedit.is_empty() {
            let d = format!("{}{}{}", &s.text[..cursor], s.preedit, &s.text[cursor..]);
            (d, cursor + s.preedit.len(), Some((cursor, cursor + s.preedit.len())))
        } else {
            (s.text.clone(), cursor, None)
        };
        let st = style(f);
        let caret_x = if focused { byte_offset_to_x(&disp, caret, &mut self.text.fonts, &st, self.scale) } else { 0.0 };
        let scroll = (caret_x - iw + 2.0).max(0.0);
        p.push_clip(rect(x + 2.0, y + 2.0, w - 4.0, h - 4.0));
        if disp.is_empty() && !placeholder.is_empty() && s.preedit.is_empty() {
            self.text.draw_line(p, placeholder, ix, y, h, f, col(c.muted_foreground));
        } else {
            if focused
                && !s.password
                && let Some((a, b)) = s.selection().filter(|(a, b)| a < b)
            {
                let xa = byte_offset_to_x(&disp, a, &mut self.text.fonts, &st, self.scale);
                let xb = byte_offset_to_x(&disp, b, &mut self.text.fonts, &st, self.scale);
                p.fill_rect(rect(ix + xa - scroll, y + 5.0, xb - xa, h - 10.0), col(c.ring.with_alpha(0.25)));
            }
            let fg = if enabled { c.foreground } else { c.disabled_text };
            self.text.draw_line(p, &disp, ix - scroll, y, h, f, col(fg));
            if let Some((a, b)) = pre {
                let xa = byte_offset_to_x(&disp, a, &mut self.text.fonts, &st, self.scale);
                let xb = byte_offset_to_x(&disp, b, &mut self.text.fonts, &st, self.scale);
                p.fill_rect(rect(ix + xa - scroll, y + h - 8.0, xb - xa, 1.0), col(c.foreground));
            }
        }
        if focused {
            let cx = ix + caret_x - scroll;
            p.fill_rect(rect(cx, y + 6.0, 1.5, h - 12.0), col(c.foreground));
            self.ime_rect = Some([cx, y, 2.0, h]);
        }
        p.pop_clip();
    }

    /// Byte offset in `s` nearest to the logical x coordinate (for click-to-place-caret).
    pub fn offset_at(&mut self, s: &TextInputState, r: R, x: f32) -> usize {
        if s.password {
            return s.text.len();
        }
        let (rx0, _, w, _) = rx(&r);
        let st = style(BODY);
        let iw = w - 16.0;
        let caret_x = byte_offset_to_x(&s.text, s.cursor().min(s.text.len()), &mut self.text.fonts, &st, self.scale);
        let scroll = (caret_x - iw + 2.0).max(0.0);
        let target = x - (rx0 + 8.0) + scroll;
        let mut best = (0usize, f32::MAX);
        for (i, _) in s.text.char_indices().chain(std::iter::once((s.text.len(), ' '))) {
            let d = (byte_offset_to_x(&s.text, i, &mut self.text.fonts, &st, self.scale) - target).abs();
            if d < best.1 {
                best = (i, d);
            }
        }
        best.0
    }

    /// Multi-line text field (archive comment).
    pub(super) fn textarea(&mut self, p: &mut Painter, r: R, field: Field, s: &TextInputState) {
        let c = self.theme.colors;
        let focused = s.focused;
        self.field_frame(p, r, focused, true);
        self.push_hit(r, Hit::Field(field, r), CursorHint::Text);
        let (x, y, w, h) = rx(&r);
        let (ix, iy, iw, ih) = (x + 10.0, y + 8.0, w - 20.0, h - 16.0);
        let f: Font = BODY;
        let lh = f.line_height();
        let cursor = s.cursor().min(s.text.len());
        let disp = if s.preedit.is_empty() { s.text.clone() } else { format!("{}{}{}", &s.text[..cursor], s.preedit, &s.text[cursor..]) };
        let caret = cursor + s.preedit.len();
        let (cx, cy) = {
            let l = self.text.layout(&disp, f, Some(iw));
            caret_pos(l, &disp, caret, lh)
        };
        let scroll = (cy + lh - ih).max(0.0);
        p.push_clip(rect(x + 2.0, y + 2.0, w - 4.0, h - 4.0));
        if disp.is_empty() {
            self.text.draw_line(p, "（沒有註解）", ix, iy, lh, f, col(c.muted_foreground));
        } else {
            self.text.draw_block(p, &disp, ix, iy - scroll, iw, f, col(c.foreground));
        }
        if focused {
            let (px, py) = (ix + cx, iy + cy - scroll);
            p.fill_rect(rect(px, py + 2.0, 1.5, lh - 4.0), col(c.foreground));
            self.ime_rect = Some([px, py, 2.0, lh]);
        }
        p.pop_clip();
    }

    pub(super) fn label(&mut self, p: &mut Painter, x: f32, y: f32, h: f32, t: &str, color: Color) {
        self.text.draw_line(p, t, x, y, h, BODY, color);
    }
}

/// Caret position (x, line top) for byte offset `off` in wrapped `text`.
fn caret_pos(l: &acme_text::TextLayout, text: &str, off: usize, lh: f32) -> (f32, f32) {
    if off == 0 || l.glyphs.is_empty() {
        return (0.0, text[..off.min(text.len())].matches('\n').count() as f32 * lh);
    }
    let top = |g: &acme_text::ShapedGlyph| (g.y - l.baseline).max(0.0);
    let last = l.glyphs.iter().filter(|g| g.byte_range.start < off && !text[g.byte_range.clone()].contains('\n')).max_by_key(|g| g.byte_range.start);
    match last {
        Some(g) => {
            let newlines = text[g.byte_range.end.min(off)..off].matches('\n').count();
            if newlines > 0 { (0.0, top(g) + newlines as f32 * lh) } else { (g.x + g.advance, top(g)) }
        }
        None => (0.0, text[..off].matches('\n').count() as f32 * lh),
    }
}

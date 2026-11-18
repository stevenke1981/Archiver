//! Text shaping with a per-frame-reused layout cache, elision and drawing into a [`Painter`].

use std::collections::HashMap;

use acme_core::{AtlasUpload, Color, GlyphDraw, GlyphFormat, Painter, Point, TextPrimitive};
use acme_text::{AtlasFormat, FontSystem, GlyphAtlas, TextConstraints, TextLayout, TextStyle, TextWrap};

const MAX_CACHE: usize = 6000;

#[derive(Clone, PartialEq, Eq, Hash)]
struct Key {
    text: String,
    size: u32,
    bold: bool,
    mono: bool,
    wrap: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Font {
    pub size: f32,
    pub bold: bool,
    pub mono: bool,
}

impl Font {
    pub const fn new(size: f32) -> Self {
        Font { size, bold: false, mono: false }
    }
    pub const fn bold(mut self) -> Self {
        self.bold = true;
        self
    }
    pub const fn mono(mut self) -> Self {
        self.mono = true;
        self
    }
    pub fn line_height(self) -> f32 {
        (self.size * 1.4).round()
    }
}

pub struct Text {
    pub fonts: FontSystem,
    pub atlas: GlyphAtlas,
    cache: HashMap<Key, TextLayout>,
    elided: HashMap<(String, u32, bool, u32), String>,
    scale: f32,
}

fn family(mono: bool) -> &'static str {
    if cfg!(windows) {
        if mono { "Consolas" } else { "Microsoft JhengHei UI" }
    } else if mono {
        "monospace"
    } else {
        "sans-serif"
    }
}

pub fn style(f: Font) -> TextStyle {
    TextStyle { font_size: f.size, line_height: f.line_height(), family: family(f.mono).into(), bold: f.bold, italic: false }
}

fn map_format(f: AtlasFormat) -> GlyphFormat {
    match f {
        AtlasFormat::Alpha8 => GlyphFormat::Alpha8,
        AtlasFormat::Rgba8 => GlyphFormat::Rgba8,
    }
}

impl Default for Text {
    fn default() -> Self {
        Self::new()
    }
}

impl Text {
    pub fn new() -> Self {
        Text { fonts: FontSystem::new(), atlas: GlyphAtlas::default(), cache: HashMap::new(), elided: HashMap::new(), scale: 1.0 }
    }

    pub fn begin_frame(&mut self, scale: f32) {
        self.atlas.begin_frame();
        if (scale - self.scale).abs() > f32::EPSILON || self.cache.len() > MAX_CACHE {
            self.cache.clear();
        }
        if self.elided.len() > MAX_CACHE {
            self.elided.clear();
        }
        self.scale = scale;
    }

    /// Drop everything (GPU device recovered: the atlas is empty again).
    pub fn reset(&mut self) {
        self.atlas.clear();
        self.cache.clear();
    }

    fn key(text: &str, f: Font, wrap: Option<f32>) -> Key {
        Key { text: text.to_string(), size: f.size.to_bits(), bold: f.bold, mono: f.mono, wrap: wrap.map_or(0, |w| w.max(1.0).to_bits()) }
    }

    pub fn layout(&mut self, text: &str, f: Font, wrap: Option<f32>) -> &TextLayout {
        let key = Self::key(text, f, wrap);
        let (fonts, scale) = (&mut self.fonts, self.scale);
        self.cache.entry(key).or_insert_with(|| {
            let c = TextConstraints { max_width: wrap, wrap: if wrap.is_some() { TextWrap::WordOrGlyph } else { TextWrap::None } };
            fonts.shape(text, &style(f), c, scale)
        })
    }

    pub fn width(&mut self, text: &str, f: Font) -> f32 {
        if text.is_empty() {
            return 0.0;
        }
        self.layout(text, f, None).width
    }

    /// Height of `text` wrapped at `width` (at least one line).
    pub fn height(&mut self, text: &str, f: Font, width: f32) -> f32 {
        let lh = f.line_height();
        self.layout(text, f, Some(width)).height.max(lh)
    }

    /// `text` shortened with "…" to fit into `max` logical pixels.
    pub fn elide(&mut self, text: &str, f: Font, max: f32) -> String {
        if self.width(text, f) <= max {
            return text.to_string();
        }
        let key = (text.to_string(), f.size.to_bits(), f.bold, max.to_bits());
        if let Some(s) = self.elided.get(&key) {
            return s.clone();
        }
        let chars: Vec<char> = text.chars().collect();
        let (mut lo, mut hi) = (0usize, chars.len());
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            let s: String = chars[..mid].iter().collect::<String>() + "…";
            if self.width(&s, f) <= max { lo = mid } else { hi = mid - 1 }
        }
        let out = if lo == 0 { String::new() } else { chars[..lo].iter().collect::<String>() + "…" };
        self.elided.insert(key, out.clone());
        out
    }

    /// Draw `text` with its first baseline at (x, baseline).
    #[allow(clippy::too_many_arguments)]
    pub fn draw_at(&mut self, p: &mut Painter, text: &str, x: f32, baseline: f32, f: Font, color: Color, wrap: Option<f32>) {
        if text.is_empty() {
            return;
        }
        let key = Self::key(text, f, wrap);
        if !self.cache.contains_key(&key) {
            self.layout(text, f, wrap);
        }
        let Some(layout) = self.cache.get(&key) else { return };
        let prepared = self.fonts.prepare(layout, &mut self.atlas);
        let uploads = prepared
            .uploads
            .into_iter()
            .map(|r| AtlasUpload { page: r.page, origin: [r.x, r.y], size: [r.width, r.height], format: map_format(r.format), pixels: r.pixels })
            .collect();
        let glyphs = prepared
            .glyphs
            .into_iter()
            .map(|g| GlyphDraw {
                x: g.x as f32,
                y: g.y as f32,
                width: g.width as f32,
                height: g.height as f32,
                atlas_x: g.atlas_x,
                atlas_y: g.atlas_y,
                atlas_page: g.atlas_page,
                format: map_format(g.format),
            })
            .collect();
        p.text_primitive(TextPrimitive { origin: Point::new(x, baseline), color, glyphs, uploads });
    }

    /// Draw `text` vertically centred in the band [top, top + h), starting at `x`.
    /// Returns the drawn width.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_line(&mut self, p: &mut Painter, text: &str, x: f32, top: f32, h: f32, f: Font, color: Color) -> f32 {
        if text.is_empty() {
            return 0.0;
        }
        let (w, lh, base) = {
            let l = self.layout(text, f, None);
            (l.width, l.height, l.baseline)
        };
        let baseline = (top + (h - lh) / 2.0 + base).round();
        self.draw_at(p, text, x, baseline, f, color, None);
        w
    }

    /// Wrapped paragraph(s) starting at `top`; returns the height used.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_block(&mut self, p: &mut Painter, text: &str, x: f32, top: f32, width: f32, f: Font, color: Color) -> f32 {
        if text.is_empty() {
            return 0.0;
        }
        let (h, base) = {
            let l = self.layout(text, f, Some(width));
            (l.height, l.baseline)
        };
        self.draw_at(p, text, x, top + base, f, color, Some(width));
        h.max(f.line_height())
    }
}

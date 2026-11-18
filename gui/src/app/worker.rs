//! Background part of the preview pane: read the bytes, classify them and decode images.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use archiver_core::Progress;

use super::model::{PreviewDone, PvBody, PvSrc};
use crate::preview::{self, ImageKind, Preview};

/// Bytes shown as hex when an image cannot be decoded.
pub const HEX_FALLBACK: usize = 16 << 10;
/// Larger images are scaled down before they reach the renderer.
const MAX_IMAGE_EDGE: u32 = 2048;

/// First `max` bytes of a file and whether it is longer.
pub fn read_head(path: &Path, max: usize) -> std::io::Result<(Vec<u8>, bool)> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?.take(max as u64 + 1).read_to_end(&mut bytes)?;
    let truncated = bytes.len() > max;
    bytes.truncate(max);
    Ok((bytes, truncated))
}

/// Decode PNG / JPEG into straight-alpha RGBA8.
pub fn decode_raster(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let img = image::load_from_memory(bytes).ok()?;
    let img = if img.width() > MAX_IMAGE_EDGE || img.height() > MAX_IMAGE_EDGE { img.thumbnail(MAX_IMAGE_EDGE, MAX_IMAGE_EDGE) } else { img };
    let rgba = img.to_rgba8();
    Some((rgba.width(), rgba.height(), rgba.into_raw()))
}

/// Rasterise an SVG so that its longer edge is about `edge` pixels.
pub fn render_svg(data: &[u8], edge: u32) -> Option<(u32, u32, Vec<u8>)> {
    use resvg::{tiny_skia, usvg};
    let tree = usvg::Tree::from_data(data, &usvg::Options::default()).ok()?;
    let size = tree.size();
    let (w, h) = (size.width().max(1.0), size.height().max(1.0));
    let scale = edge as f32 / w.max(h);
    let (pw, ph) = (((w * scale).round() as u32).max(1), ((h * scale).round() as u32).max(1));
    let mut pixmap = tiny_skia::Pixmap::new(pw, ph)?;
    resvg::render(&tree, tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    Some((pw, ph, unpremultiply(pixmap.take())))
}

/// tiny-skia pixmaps are premultiplied; the scene expects straight alpha.
pub fn unpremultiply(mut px: Vec<u8>) -> Vec<u8> {
    for p in px.chunks_exact_mut(4) {
        let a = p[3] as u32;
        if a > 0 && a < 255 {
            for c in &mut p[..3] {
                *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    px
}

/// Read the data and classify it (runs on a worker thread).
pub fn preview_worker(generation: u64, src: PvSrc, name: &str, cancel: Arc<AtomicBool>) -> PreviewDone {
    let limit = preview::read_limit(name);
    let read = match &src {
        PvSrc::Entry { archive, name: entry, password } => {
            let noop = |_: u64, _: u64, _: &str| {};
            archiver_core::read_entry(archive, entry, password.as_deref(), limit, &Progress { on: &noop, cancel }).map(|d| (d.bytes, d.truncated)).map_err(|e| e.to_string())
        }
        PvSrc::File(p) => read_head(p, limit).map_err(|e| e.to_string()),
    };
    let (bytes, truncated) = match read {
        Ok(r) => r,
        Err(e) => return PreviewDone { generation, body: PvBody::Error(e), full: None },
    };
    let body = classify(name, &bytes, truncated);
    let full = match src {
        PvSrc::Entry { archive, name, .. } if !truncated => Some((archive, name, bytes)),
        _ => None,
    };
    PreviewDone { generation, body, full }
}

pub fn classify(name: &str, bytes: &[u8], truncated: bool) -> PvBody {
    let image = |r: Option<(u32, u32, Vec<u8>)>| match r {
        Some((width, height, rgba)) => PvBody::Image { width, height, rgba },
        None => PvBody::Hex(preview::hex_dump(bytes, HEX_FALLBACK)),
    };
    match preview::classify(name, bytes, truncated) {
        Preview::Text(t) => PvBody::Text(t),
        Preview::Hex(h) => PvBody::Hex(h),
        Preview::Empty => PvBody::Empty,
        Preview::Image(ImageKind::Svg) => image(render_svg(bytes, 1024)),
        Preview::Image(ImageKind::Raster) => image(decode_raster(bytes)),
    }
}

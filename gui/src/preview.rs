//! Pure logic of the preview pane: how much of an entry to read and how to show the bytes
//! (text, image or hex dump). Kept free of UI types for tests.

use std::path::Path;

/// Bytes read for a text / hex preview.
pub const TEXT_LIMIT: usize = 256 << 10;
/// Bytes read for an image preview (larger images are shown as hex).
pub const IMAGE_LIMIT: usize = 32 << 20;

const HEX_MAX: usize = 4096;
const MAX_LINES: usize = 2000;
const MAX_CHARS: usize = 64 << 10;
const NUL_SCAN: usize = 8 << 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageKind {
    /// PNG / JPEG, decoded with `slint::Image::load_from_path`.
    Raster,
    /// SVG, decoded with `slint::Image::load_from_svg_data`.
    Svg,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Preview {
    /// Display text (already capped for display, with a trailing note when cut).
    Text(String),
    Image(ImageKind),
    /// Hex dump of the first bytes.
    Hex(String),
    /// Zero-length file.
    Empty,
}

fn ext_lower(name: &str) -> String {
    Path::new(name).extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default()
}

/// How many bytes to read for an entry called `name`.
pub fn read_limit(name: &str) -> usize {
    match ext_lower(name).as_str() {
        "png" | "jpg" | "jpeg" | "svg" => IMAGE_LIMIT,
        _ => TEXT_LIMIT,
    }
}

/// Decide how to show `bytes` (the first bytes of file `name`; `truncated` = file is longer).
pub fn classify(name: &str, bytes: &[u8], truncated: bool) -> Preview {
    if bytes.is_empty() && !truncated {
        return Preview::Empty;
    }
    if !truncated {
        match ext_lower(name).as_str() {
            "png" if bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47]) => return Preview::Image(ImageKind::Raster),
            "jpg" | "jpeg" if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) => return Preview::Image(ImageKind::Raster),
            "svg" if String::from_utf8_lossy(bytes).contains("<svg") => return Preview::Image(ImageKind::Svg),
            _ => {}
        }
    }
    if let Some(text) = decode_text(bytes, truncated) {
        return Preview::Text(display_text(&text, truncated));
    }
    let mut s = hex_dump(bytes, HEX_MAX);
    if bytes.len() > HEX_MAX || truncated {
        s.push_str(&format!("\n…（僅顯示前 {HEX_MAX} 位元組）"));
    }
    Preview::Hex(s)
}

/// Text decoded from `bytes`, or `None` when the bytes look binary.
fn decode_text(bytes: &[u8], truncated: bool) -> Option<String> {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return Some(String::from_utf8_lossy(rest).into_owned());
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return Some(utf16_text(rest, u16::from_le_bytes));
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return Some(utf16_text(rest, u16::from_be_bytes));
    }
    if bytes[..bytes.len().min(NUL_SCAN)].contains(&0) {
        return None;
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => Some(s.to_string()),
        Err(e) if truncated && e.error_len().is_none() => std::str::from_utf8(&bytes[..e.valid_up_to()]).ok().map(str::to_string),
        Err(_) => None,
    }
}

fn utf16_text(bytes: &[u8], to_u16: fn([u8; 2]) -> u16) -> String {
    let units: Vec<u16> = bytes.chunks_exact(2).map(|c| to_u16([c[0], c[1]])).collect();
    String::from_utf16_lossy(&units)
}

/// Normalise line endings and tabs, then cap by line and character count.
fn display_text(text: &str, truncated: bool) -> String {
    let norm = text.replace("\r\n", "\n").replace('\r', "\n").replace('\t', "    ");
    let mut out = String::new();
    let mut chars = 0usize;
    let mut capped = truncated;
    for (i, line) in norm.split('\n').enumerate() {
        if i == MAX_LINES {
            capped = true;
            break;
        }
        if i > 0 {
            if chars == MAX_CHARS {
                capped = true;
                break;
            }
            out.push('\n');
            chars += 1;
        }
        let n = line.chars().count();
        let take = n.min(MAX_CHARS - chars);
        out.extend(line.chars().take(take));
        chars += take;
        if take < n {
            capped = true;
            break;
        }
    }
    if capped {
        out.push_str("\n\n…（僅顯示部分內容）");
    }
    out
}

/// Classic hex dump of at most `max` bytes: `00000000  4D 5A 90 00 …  |MZ..|`.
pub fn hex_dump(bytes: &[u8], max: usize) -> String {
    let bytes = &bytes[..bytes.len().min(max)];
    let mut out = String::new();
    for (row, chunk) in bytes.chunks(16).enumerate() {
        if row > 0 {
            out.push('\n');
        }
        out.push_str(&format!("{:08X}  ", row * 16));
        for i in 0..16 {
            match chunk.get(i) {
                Some(b) => out.push_str(&format!("{b:02X} ")),
                None => out.push_str("   "),
            }
            if i == 7 {
                out.push(' ');
            }
        }
        out.push_str(" |");
        out.extend(chunk.iter().map(|&b| if (0x20..=0x7E).contains(&b) { b as char } else { '.' }));
        out.push('|');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTE: &str = "\n\n…（僅顯示部分內容）";

    #[test]
    fn limits() {
        assert_eq!(read_limit("a.PNG"), IMAGE_LIMIT);
        assert_eq!(read_limit("dir/b.Jpeg"), IMAGE_LIMIT);
        assert_eq!(read_limit("c.svg"), IMAGE_LIMIT);
        assert_eq!(read_limit("d.txt"), TEXT_LIMIT);
        assert_eq!(read_limit("noext"), TEXT_LIMIT);
    }

    #[test]
    fn empty() {
        assert_eq!(classify("a.txt", b"", false), Preview::Empty);
    }

    #[test]
    fn png_magic_and_wrong_magic() {
        let png = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0];
        assert_eq!(classify("A.PNG", &png, false), Preview::Image(ImageKind::Raster));
        let fake = [0u8, 1, 2];
        assert_eq!(classify("x.png", &fake, false), Preview::Hex(hex_dump(&fake, HEX_MAX)));
    }

    #[test]
    fn truncated_png_is_hex() {
        let png = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0];
        let want = format!("{}\n…（僅顯示前 4096 位元組）", hex_dump(&png, HEX_MAX));
        assert_eq!(classify("x.png", &png, true), Preview::Hex(want));
    }

    #[test]
    fn jpeg_magic() {
        assert_eq!(classify("a.jpg", &[0xFF, 0xD8, 0xFF, 0xE0, 0], false), Preview::Image(ImageKind::Raster));
        assert_eq!(classify("a.jpeg", &[0xFF, 0xD8, 0xFF], false), Preview::Image(ImageKind::Raster));
    }

    #[test]
    fn svg() {
        let svg = b"<?xml version=\"1.0\"?><svg xmlns=\"http://www.w3.org/2000/svg\"></svg>";
        assert_eq!(classify("a.svg", svg, false), Preview::Image(ImageKind::Svg));
        assert_eq!(classify("a.svg", b"hello", false), Preview::Text("hello".into()));
    }

    #[test]
    fn utf8() {
        assert_eq!(classify("a.txt", "中文 hello".as_bytes(), false), Preview::Text("中文 hello".into()));
    }

    #[test]
    fn utf8_bom_stripped() {
        assert_eq!(classify("a.txt", &[0xEF, 0xBB, 0xBF, b'h', b'i'], false), Preview::Text("hi".into()));
    }

    #[test]
    fn utf16_le_and_be() {
        assert_eq!(classify("a.txt", &[0xFF, 0xFE, 0x41, 0x00, 0x42, 0x00, 0x43, 0x00, 0x44, 0x00, 0x45], false), Preview::Text("ABCD".into()));
        assert_eq!(classify("a.txt", &[0xFE, 0xFF, 0x00, 0x41, 0x00, 0x42], false), Preview::Text("AB".into()));
    }

    #[test]
    fn truncated_utf8_mid_character() {
        let mut b = b"ab".to_vec();
        b.extend_from_slice(&[0xE4, 0xB8]);
        assert_eq!(classify("a.txt", &b, true), Preview::Text(format!("ab{NOTE}")));
        assert_eq!(classify("a.txt", &b, false), Preview::Hex(hex_dump(&b, HEX_MAX)));
    }

    #[test]
    fn binary_with_nul_is_hex() {
        let b = b"MZ\x00\x01";
        assert_eq!(classify("a.bin", b, false), Preview::Hex(hex_dump(b, HEX_MAX)));
    }

    #[test]
    fn crlf_normalised() {
        assert_eq!(classify("a.txt", b"a\r\nb\rc\n", false), Preview::Text("a\nb\nc\n".into()));
    }

    #[test]
    fn tabs_expanded() {
        assert_eq!(classify("a.txt", b"\tx\ty", false), Preview::Text("    x    y".into()));
    }

    #[test]
    fn truncated_text_gets_note() {
        assert_eq!(classify("a.txt", b"abc", true), Preview::Text(format!("abc{NOTE}")));
    }

    #[test]
    fn line_cap() {
        let text = "x\n".repeat(2500);
        let Preview::Text(s) = classify("a.txt", text.as_bytes(), false) else { panic!("not text") };
        assert!(s.ends_with(NOTE));
        assert_eq!(s.matches('x').count(), MAX_LINES);
    }

    #[test]
    fn char_cap_is_char_boundary_safe() {
        let text = "中".repeat(70_000);
        let Preview::Text(s) = classify("a.txt", text.as_bytes(), false) else { panic!("not text") };
        assert!(s.ends_with(NOTE));
        assert_eq!(s.matches('中').count(), MAX_CHARS);
    }

    #[test]
    fn large_hex_has_note() {
        let mut b = vec![0u8; 5000];
        b[0] = 0x7F;
        let Preview::Hex(s) = classify("a.bin", &b, false) else { panic!("not hex") };
        assert_eq!(s.lines().count(), HEX_MAX / 16 + 1);
        assert!(s.ends_with("\n…（僅顯示前 4096 位元組）"));
    }

    #[test]
    fn hex_dump_exact_short() {
        let want = format!("00000000  4D 5A 90 00{}|MZ..|", " ".repeat(39));
        assert_eq!(hex_dump(b"MZ\x90\x00", 4096), want);
    }

    #[test]
    fn hex_dump_full_and_partial_rows() {
        let b: Vec<u8> = (0u8..20).collect();
        let s = hex_dump(&b, 4096);
        let lines: Vec<&str> = s.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "00000000  00 01 02 03 04 05 06 07  08 09 0A 0B 0C 0D 0E 0F  |................|");
        assert!(lines[1].starts_with("00000010  10 11 12 13 "));
        assert_eq!(lines[0].find('|'), lines[1].find('|'));
        assert!(!s.ends_with('\n'));
    }

    #[test]
    fn hex_dump_printable_and_max() {
        assert_eq!(hex_dump(b"Hi~\x7F", 4096), format!("00000000  48 69 7E 7F{}|Hi~.|", " ".repeat(39)));
        assert_eq!(hex_dump(&[0u8; 100], 20).lines().count(), 2);
        assert_eq!(hex_dump(&[], 4096), "");
    }

    #[test]
    fn read_limit_matches_extension_case() {
        assert_eq!(read_limit("README"), TEXT_LIMIT);
    }
}

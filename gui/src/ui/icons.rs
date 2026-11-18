//! Toolbar and file-type icons: SVGs rasterised once per size and DPI with resvg.

use std::collections::HashMap;

use acme_core::{ImageKey, Rgba8Image};

use crate::app::worker::unpremultiply;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Icon {
    Add,
    Extract,
    Test,
    View,
    Delete,
    Find,
    Wizard,
    Info,
    Preview,
    Copy,
    Antivirus,
    Comment,
    Up,
    Folder,
    File,
    Archive,
    Exe,
    Text,
    Image,
}

impl Icon {
    fn svg(self) -> &'static str {
        match self {
            Icon::Add => include_str!("../../assets/icons/add.svg"),
            Icon::Extract => include_str!("../../assets/icons/extract.svg"),
            Icon::Test => include_str!("../../assets/icons/test.svg"),
            Icon::View => include_str!("../../assets/icons/view.svg"),
            Icon::Delete => include_str!("../../assets/icons/delete.svg"),
            Icon::Find => include_str!("../../assets/icons/find.svg"),
            Icon::Wizard => include_str!("../../assets/icons/wizard.svg"),
            Icon::Info => include_str!("../../assets/icons/info.svg"),
            Icon::Preview => include_str!("../../assets/icons/preview.svg"),
            Icon::Copy => include_str!("../../assets/icons/copy.svg"),
            Icon::Antivirus => include_str!("../../assets/icons/antivirus.svg"),
            Icon::Comment => include_str!("../../assets/icons/comment.svg"),
            Icon::Up => include_str!("../../assets/icons/up.svg"),
            Icon::Folder => include_str!("../../assets/icons/folder.svg"),
            Icon::File => include_str!("../../assets/icons/file.svg"),
            Icon::Archive => include_str!("../../assets/icons/archive.svg"),
            Icon::Exe => include_str!("../../assets/icons/exe.svg"),
            Icon::Text => include_str!("../../assets/icons/text.svg"),
            Icon::Image => include_str!("../../assets/icons/image.svg"),
        }
    }

    /// File-list icon for [`crate::view::icon`] ids.
    pub fn for_kind(kind: i32) -> Icon {
        match kind {
            0 | 1 => Icon::Folder,
            3 => Icon::Archive,
            4 => Icon::Exe,
            5 => Icon::Text,
            6 => Icon::Image,
            _ => Icon::File,
        }
    }
}

#[derive(Default)]
pub struct Icons {
    cache: HashMap<(Icon, u32, bool), Option<Rgba8Image>>,
}

impl Icons {
    /// The icon rasterised at `px` physical pixels; `faded` for disabled buttons.
    pub fn get(&mut self, icon: Icon, px: u32, faded: bool) -> Option<Rgba8Image> {
        let px = px.clamp(8, 512);
        let next = self.cache.len() as u64 + 1;
        self.cache
            .entry((icon, px, faded))
            .or_insert_with(|| {
                let (w, h, mut rgba) = rasterise(icon.svg().as_bytes(), px)?;
                if faded {
                    for p in rgba.as_chunks_mut::<4>().0 {
                        let g = ((p[0] as u32 * 3 + p[1] as u32 * 6 + p[2] as u32) / 10) as u8;
                        p[0] = g;
                        p[1] = g;
                        p[2] = g;
                        p[3] = (p[3] as u32 * 2 / 5) as u8;
                    }
                }
                Rgba8Image::new(ImageKey::new(next), 0, w, h, rgba).ok()
            })
            .clone()
    }
}

fn rasterise(data: &[u8], px: u32) -> Option<(u32, u32, Vec<u8>)> {
    use resvg::{tiny_skia, usvg};
    let tree = usvg::Tree::from_data(data, &usvg::Options::default()).ok()?;
    let size = tree.size();
    let scale = px as f32 / size.width().max(size.height()).max(1.0);
    let mut pixmap = tiny_skia::Pixmap::new(px, px)?;
    let dx = (px as f32 - size.width() * scale) / 2.0;
    let dy = (px as f32 - size.height() * scale) / 2.0;
    resvg::render(&tree, tiny_skia::Transform::from_row(scale, 0.0, 0.0, scale, dx, dy), &mut pixmap.as_mut());
    Some((px, px, unpremultiply(pixmap.take())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_rasterises() {
        let mut icons = Icons::default();
        for i in [Icon::Add, Icon::Extract, Icon::Test, Icon::View, Icon::Delete, Icon::Find, Icon::Wizard, Icon::Info, Icon::Preview, Icon::Copy, Icon::Antivirus, Icon::Comment, Icon::Up, Icon::Folder, Icon::File, Icon::Archive, Icon::Exe, Icon::Text, Icon::Image] {
            let img = icons.get(i, 32, false).unwrap_or_else(|| panic!("{i:?}"));
            assert_eq!((img.width(), img.height()), (32, 32));
            assert!(img.pixels().chunks(4).any(|p| p[3] > 0), "{i:?} is blank");
        }
        let a = icons.get(Icon::Add, 32, false).unwrap();
        let b = icons.get(Icon::Add, 32, true).unwrap();
        assert_ne!(a.key(), b.key());
    }
}

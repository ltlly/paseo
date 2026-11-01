use std::{borrow::Cow, collections::VecDeque};

use gpui::{Font, Pixels, ShapedLine, SharedString, TextRun, WindowTextSystem, px, rgba};
use hashbrown::HashMap;
use paseo_desktop::terminal::Cell;

const MAX_ENTRIES: usize = 256;
const MAX_TEXT_BYTES: usize = 16 * 1024;
const MAX_GLYPH_BYTES: usize = 512;

#[derive(Clone, Hash, PartialEq, Eq)]
struct Key {
    text: SharedString,
    foreground: u32,
    bold: bool,
    italic: bool,
    underline: bool,
    opacity: u8,
    strikethrough: bool,
}

/// Reuse complete shaped cells, including their decorations. This avoids the
/// text-system locks on every repeated character; the grid still owns placement.
#[derive(Default)]
pub struct Glyphs {
    font: Option<(Font, Pixels)>,
    entries: HashMap<Key, Box<ShapedLine>>,
    order: VecDeque<Key>,
    text_bytes: usize,
}

impl Glyphs {
    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.text_bytes = 0;
    }

    pub fn prepare(&mut self, font: Font, size: Pixels) {
        if self.font.as_ref() != Some(&(font.clone(), size)) {
            self.clear();
            self.font = Some((font, size));
        }
    }

    pub fn line<'a>(
        &'a mut self,
        cell: &Cell<'_>,
        text_system: &WindowTextSystem,
    ) -> Cow<'a, ShapedLine> {
        self.line_with_cursor(cell, text_system, false)
    }

    pub fn line_with_cursor<'a>(
        &'a mut self,
        cell: &Cell<'_>,
        text_system: &WindowTextSystem,
        block_cursor: bool,
    ) -> Cow<'a, ShapedLine> {
        let key = Key {
            text: if cell.zerowidth.is_empty() {
                cell.character.into()
            } else {
                cell.chars().collect::<String>().into()
            },
            foreground: if block_cursor {
                crate::theme::terminal::CURSOR_ACCENT
            } else {
                cell.foreground
            },
            bold: cell.bold && !block_cursor,
            italic: cell.italic && !block_cursor,
            underline: cell.underline && !block_cursor,
            opacity: if block_cursor { 255 } else { cell.opacity },
            strikethrough: cell.strikethrough && !block_cursor,
        };
        let (base_font, size) = self
            .font
            .as_ref()
            .expect("prepare terminal glyph font before painting");
        if key.text.len() > MAX_GLYPH_BYTES {
            // Long combining sequences remain complete, but cannot pin arbitrarily
            // large shaped lines in the retained glyph cache.
            return Cow::Owned(shape(&key, base_font, *size, text_system));
        }
        if !self.entries.contains_key(&key) {
            let line = shape(&key, base_font, *size, text_system);
            while self.entries.len() >= MAX_ENTRIES
                || self.text_bytes + key.text.len() > MAX_TEXT_BYTES
            {
                let oldest = self
                    .order
                    .pop_front()
                    .expect("cache budgets have an entry to evict");
                self.text_bytes -= oldest.text.len();
                self.entries.remove(&oldest);
            }
            self.text_bytes += key.text.len();
            self.order.push_back(key.clone());
            self.entries.insert(key.clone(), Box::new(line));
        }
        Cow::Borrowed(self.entries.get(&key).expect("terminal glyph was cached"))
    }
}

fn shape(key: &Key, base_font: &Font, size: Pixels, text_system: &WindowTextSystem) -> ShapedLine {
    let font = match (key.bold, key.italic) {
        (true, true) => base_font.clone().bold().italic(),
        (true, false) => base_font.clone().bold(),
        (false, true) => base_font.clone().italic(),
        _ => base_font.clone(),
    };
    let run = TextRun {
        len: key.text.len(),
        font,
        color: rgba((key.foreground << 8) | u32::from(key.opacity)).into(),
        background_color: None,
        underline: key.underline.then_some(gpui::UnderlineStyle {
            thickness: px(1.),
            color: Some(rgba((key.foreground << 8) | u32::from(key.opacity)).into()),
            wavy: false,
        }),
        strikethrough: key.strikethrough.then_some(gpui::StrikethroughStyle {
            thickness: px(1.),
            color: Some(rgba((key.foreground << 8) | u32::from(key.opacity)).into()),
        }),
    };
    text_system.shape_line(key.text.clone(), size, &[run], None)
}

use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

/// Byte offsets internally; OS text services use UTF-16. Selection is always ordered.
#[derive(Default)]
pub struct Editing {
    pub text: String,
    pub selection: Range<usize>,
    pub reversed: bool,
    pub marked: Option<Range<usize>>,
    pub revision: u64,
}

impl Editing {
    pub fn cursor(&self) -> usize {
        if self.reversed {
            self.selection.start
        } else {
            self.selection.end
        }
    }

    pub fn utf16_offset(&self, byte: usize) -> usize {
        self.text[..self.boundary(byte)].encode_utf16().count()
    }

    pub fn utf16_range(&self, range: Range<usize>) -> Range<usize> {
        self.utf16_offset(range.start)..self.utf16_offset(range.end)
    }

    fn byte_offset(&self, units: usize, round_up: bool) -> usize {
        let mut seen = 0;
        for (byte, ch) in self.text.char_indices() {
            if seen >= units {
                return byte;
            }
            seen += ch.len_utf16();
            if seen > units {
                return byte + if round_up { ch.len_utf8() } else { 0 };
            }
        }
        self.text.len()
    }

    pub fn byte_range(&self, range: Range<usize>) -> Range<usize> {
        let start = self.byte_offset(range.start, false);
        let end = if range.is_empty() {
            start
        } else {
            self.byte_offset(range.end, true)
        };
        start..end.max(start)
    }

    fn boundary(&self, byte: usize) -> usize {
        let mut byte = byte.min(self.text.len());
        while !self.text.is_char_boundary(byte) {
            byte -= 1;
        }
        byte
    }

    pub fn select(&mut self, byte: usize, extend: bool) {
        let byte = self.boundary(byte);
        let anchor = if extend {
            if self.reversed {
                self.selection.end
            } else {
                self.selection.start
            }
        } else {
            byte
        };
        self.selection = byte.min(anchor)..byte.max(anchor);
        self.reversed = byte < anchor;
    }

    pub fn horizontal(&mut self, forward: bool, extend: bool) {
        let cursor = self.cursor();
        let target = if !extend && !self.selection.is_empty() {
            if forward {
                self.selection.end
            } else {
                self.selection.start
            }
        } else if forward {
            self.text
                .grapheme_indices(true)
                .find_map(|(i, _)| (i > cursor).then_some(i))
                .unwrap_or(self.text.len())
        } else {
            self.text
                .grapheme_indices(true)
                .rev()
                .find_map(|(i, _)| (i < cursor).then_some(i))
                .unwrap_or(0)
        };
        self.select(target, extend);
    }

    pub fn delete(&mut self, forward: bool) {
        if self.selection.is_empty() {
            self.horizontal(forward, true);
        }
        self.replace(None, "", false, None);
    }

    pub fn replace(
        &mut self,
        utf16: Option<Range<usize>>,
        text: &str,
        mark: bool,
        selected: Option<Range<usize>>,
    ) {
        let range = utf16
            .map(|r| self.byte_range(r))
            .or_else(|| self.marked.clone())
            .unwrap_or_else(|| self.selection.clone());
        self.text.replace_range(range.clone(), text);
        self.marked = (mark && !text.is_empty()).then_some(range.start..range.start + text.len());
        self.selection = if mark && let Some(selected) = selected {
            let start = self.utf16_offset(range.start);
            let len = text.encode_utf16().count();
            self.byte_range(start + selected.start.min(len)..start + selected.end.min(len))
        } else {
            range.start + text.len()..range.start + text.len()
        };
        self.reversed = false;
        self.revision += 1;
    }

    /// Programmatic replacement of a complete field value, independent of
    /// cursor/selection/preedit. User input continues to use `replace`.
    pub fn replace_all(&mut self, text: &str) {
        let end = self.text.encode_utf16().count();
        self.replace(Some(0..end), text, false, None);
    }

    pub fn clear(&mut self) {
        self.text = String::new();
        self.selection = 0..0;
        self.reversed = false;
        self.marked = None;
        self.revision += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacing_a_field_value_never_inserts_at_the_cursor_or_preedit() {
        let mut e = Editing::default();
        e.replace_all("12a😀");
        e.select(2, false);
        e.replace_all("16");
        assert_eq!(e.text, "16");
        e.replace(None, "preedit", true, None);
        e.replace_all("");
        assert!(e.text.is_empty());
        assert!(e.marked.is_none());
        assert_eq!(e.selection, 0..0);
        e.replace_all("localhost:41819");
        e.replace_all("http://localhost:41819/");
        assert_eq!(e.text, "http://localhost:41819/");
    }

    #[test]
    fn os_ranges_never_split_surrogates_or_utf8() {
        let mut e = Editing::default();
        e.replace(None, "a😀中", false, None);
        assert_eq!(e.utf16_range(1..5), 1..3);
        assert_eq!(e.byte_range(2..2), 1..1);
        assert_eq!(e.byte_range(2..3), 1..5);
        e.replace(Some(2..3), "🙂", false, None);
        assert_eq!(e.text, "a🙂中");
        assert_eq!(e.byte_range(99..100), e.text.len()..e.text.len());
    }

    #[test]
    fn movement_and_deletion_respect_graphemes_and_selection_direction() {
        let mut e = Editing::default();
        e.replace(None, "a👩‍💻e\u{301}", false, None);
        e.horizontal(false, true);
        assert_eq!(&e.text[e.selection.clone()], "e\u{301}");
        assert!(e.reversed);
        e.horizontal(false, true);
        assert_eq!(&e.text[e.selection.clone()], "👩‍💻e\u{301}");
        e.horizontal(true, false);
        e.delete(false);
        assert_eq!(e.text, "a👩‍💻");
        e.delete(false);
        assert_eq!(e.text, "a");
    }

    #[test]
    fn composition_replaces_preedit_and_commits_once() {
        let mut e = Editing::default();
        e.replace(None, "prefix ", false, None);
        e.replace(None, "ni", true, Some(2..2));
        e.replace(None, "你", true, Some(1..1));
        assert_eq!(e.text, "prefix 你");
        assert_eq!(e.marked, Some(7..10));
        e.replace(None, "你好", false, None);
        assert_eq!(e.text, "prefix 你好");
        assert_eq!(e.marked, None);
        assert_eq!(e.selection, 13..13);
    }
}

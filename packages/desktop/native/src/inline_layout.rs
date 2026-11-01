use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LineMetrics {
    pub above: f32,
    pub below: f32,
}
impl LineMetrics {
    pub fn new(ascent: f32, descent: f32, line: f32) -> Self {
        let ascent = ascent.abs();
        let descent = descent.abs();
        let above = ascent.round() + ((line - ascent.round() - descent.round()) / 2.).floor();
        Self {
            above,
            below: line - above,
        }
    }
    pub fn include(&mut self, other: Self) {
        self.above = self.above.max(other.above);
        self.below = self.below.max(other.below);
    }
    pub fn height(self) -> f32 {
        self.above + self.below
    }
}

/// A shaped grapheme/ligature cluster. Indices remain in the unmodified UTF-8 message.
#[derive(Clone, Debug)]
pub struct Cluster {
    pub range: Range<usize>,
    pub width: f32,
    pub break_after: bool,
    pub hard_break: bool,
    pub whitespace: bool,
    pub break_inside: bool,
}

/// Wrap at Unicode opportunities; only `overflow-wrap: anywhere` ranges break long tokens.
/// Invisible line-edge spaces keep their source indices for selection/copy.
pub fn wrap(clusters: &[Cluster], width: f32) -> Vec<Range<usize>> {
    let mut rows = Vec::new();
    let mut start = 0;
    while start < clusters.len() {
        let mut end = start;
        let mut advance = 0.;
        let mut last_break = None;
        let mut hard = false;
        while end < clusters.len() {
            let cluster = &clusters[end];
            if cluster.hard_break {
                hard = true;
                break;
            }
            if advance + cluster.width > width.max(0.) && end > start && !cluster.whitespace {
                if let Some(boundary) = last_break.filter(|b| *b > start) {
                    end = boundary;
                    break;
                }
                if cluster.break_inside {
                    break;
                }
            }
            advance += cluster.width;
            end += 1;
            if cluster.break_after {
                last_break = Some(end);
            }
        }
        let mut visible_end = end;
        while visible_end > start && clusters[visible_end - 1].whitespace {
            visible_end -= 1;
        }
        rows.push(start..visible_end);
        start = end + usize::from(hard);
        if !hard {
            while start < clusters.len() && clusters[start].whitespace {
                start += 1;
            }
        }
    }
    if clusters.is_empty() || clusters.last().is_some_and(|c| c.hard_break) {
        rows.push(clusters.len()..clusters.len());
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_segmentation::UnicodeSegmentation;
    fn clusters(text: &str) -> Vec<Cluster> {
        let breaks = unicode_linebreak::linebreaks(text)
            .map(|(i, _)| i)
            .collect::<Vec<_>>();
        text.grapheme_indices(true)
            .map(|(i, g)| Cluster {
                range: i..i + g.len(),
                width: 1.,
                break_after: breaks.contains(&(i + g.len())),
                hard_break: g == "\n",
                whitespace: g == " ",
                break_inside: true,
            })
            .collect()
    }
    fn lines(text: &str, width: f32) -> Vec<String> {
        let c = clusters(text);
        wrap(&c, width)
            .into_iter()
            .map(|r| r.map(|i| &text[c[i].range.clone()]).collect())
            .collect()
    }
    #[test]
    fn wraps_words_and_keeps_hard_blank_lines() {
        assert_eq!(
            lines("one two three\n\nend\n", 7.),
            ["one two", "three", "", "end", ""]
        );
    }
    #[test]
    fn emergency_wrap_never_splits_emoji_or_combining_sequences() {
        assert_eq!(lines("a👩‍💻e\u{301}中b", 2.), ["a👩‍💻", "e\u{301}中", "b"]);
        assert_eq!(lines("👩‍💻", 0.), ["👩‍💻"]);
    }
    #[test]
    fn unicode_no_break_space_keeps_the_phrase_together_when_it_fits() {
        assert_eq!(lines("a x\u{a0}y z", 4.), ["a", "x\u{a0}y", "z"]);
    }
    #[test]
    fn normal_code_overflows_an_unbreakable_token_but_links_wrap_anywhere() {
        let source = "abcdefgh next";
        let mut plain = clusters(source);
        for c in &mut plain {
            c.break_inside = false;
        }
        let rows = wrap(&plain, 4.);
        assert_eq!(rows, [0..8, 9..13]);
        assert_eq!(lines(source, 4.), ["abcd", "efgh", "next"]);
    }
    #[test]
    fn heading_code_keeps_the_descender_room_observed_in_electron() {
        // SF Pro Text 21px and Noto Sans Mono 16.8px, from the matched Linux capture.
        let mut heading = LineMetrics::new(19.995, 5.066, 27.);
        heading.include(LineMetrics::new(17.959, 4.922, 27.));
        assert_eq!(heading.above, 21.);
        assert_eq!(heading.height(), 28.);
        assert_eq!(217. + heading.height() + 16., 261.);
        let mut paragraph = LineMetrics::new(14.282, 3.618, 21.);
        paragraph.include(LineMetrics::new(12.828, 3.516, 21.));
        assert_eq!(paragraph.above, 15.);
        assert_eq!(paragraph.height(), 21.);
        // FontMetrics uses signed descents; ShapedLine exposes their positive magnitude.
        assert_eq!(paragraph, LineMetrics::new(14.282, -3.618, 21.));
    }
}

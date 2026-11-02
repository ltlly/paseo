use crate::{message_geometry as g, message_style::MessageStyle};
use gpui::{
    App, AvailableSpace, Bounds, Corners, Element, ElementId, FontStyle, FontWeight,
    GlobalElementId, InspectorElementId, IntoElement, LayoutId, Pixels, Point, ShapedLine, Size,
    TextAlign, TextRun, Window, fill, point, px, rgb, rgba, size,
};
use paseo_desktop::{
    inline_layout::{self, Cluster, LineMetrics},
    message_document::{InlineStyle, Text},
};
use std::{cell::RefCell, collections::BTreeSet, ops::Range, rc::Rc};
use unicode_segmentation::UnicodeSegmentation;

pub fn run(style: &MessageStyle, inline: &InlineStyle, len: usize, bold: bool) -> TextRun {
    let mut font = gpui::font(if inline.code {
        style.mono_family.clone()
    } else {
        style.ui_family.clone()
    });
    if inline.bold || bold {
        font.weight = FontWeight::BOLD;
    }
    if inline.italic {
        font.style = FontStyle::Italic;
    }
    TextRun {
        len,
        font,
        color: rgb(if inline.target.is_some() {
            style.palette.accent_bright
        } else {
            style.palette.foreground
        })
        .into(),
        background_color: None,
        underline: None,
        strikethrough: inline.strike.then_some(gpui::StrikethroughStyle {
            thickness: px(1.),
            color: None,
        }),
    }
}
// CSS inline layout rounds the font metrics before splitting its integer leading.
// Keep the baseline shared across fonts; GPUI's default centers each font independently.
pub fn css_baseline(ascent: Pixels, descent: Pixels, line: Pixels) -> Pixels {
    px(LineMetrics::new(ascent.into(), descent.into(), line.into()).above)
}

struct Part {
    range: Range<usize>,
    shaped: ShapedLine,
    code: Option<usize>,
    leading: bool,
    trailing: bool,
    ascent: Pixels,
    descent: Pixels,
    break_inside: bool,
}
struct Fragment {
    range: Range<usize>,
    shaped: ShapedLine,
    x: Pixels,
    left: Pixels,
    right: Pixels,
    code: Option<usize>,
    ascent: Pixels,
    descent: Pixels,
}
struct Row {
    fragments: Vec<Fragment>,
    range: Range<usize>,
    top: Pixels,
    height: Pixels,
    baseline: Pixels,
}
#[derive(Default)]
pub struct Layout {
    rows: Vec<Row>,
    pub bounds: Option<Bounds<Pixels>>,
    pub size: Size<Pixels>,
    line: Pixels,
}
impl Layout {
    pub fn index_at(&self, position: Point<Pixels>) -> usize {
        let Some(bounds) = self.bounds else { return 0 };
        let y = position.y - bounds.top();
        let Some(row) = self
            .rows
            .iter()
            .find(|row| y < row.top + row.height)
            .or_else(|| self.rows.last())
        else {
            return 0;
        };
        let x = position.x - bounds.left();
        for fragment in &row.fragments {
            if x < fragment.x + fragment.shaped.width() + fragment.right {
                return fragment.range.start + fragment.shaped.closest_index_for_x(x - fragment.x);
            }
        }
        row.range.end
    }
}
/// GPUI's StyledText shapes one font size per paragraph. Keep native glyph shaping,
/// but lay out mixed-size inline code with the original CSS box metrics and baseline.
pub struct RichText {
    text: Text,
    style: MessageStyle,
    font_size: f32,
    line: f32,
    bold: bool,
    selection: Range<usize>,
    layout: Rc<RefCell<Layout>>,
}
impl RichText {
    pub fn new(
        text: Text,
        style: MessageStyle,
        font_size: f32,
        line: f32,
        bold: bool,
        selection: Range<usize>,
    ) -> (Self, Rc<RefCell<Layout>>) {
        let layout = Rc::new(RefCell::new(Layout::default()));
        (
            Self {
                text,
                style,
                font_size,
                line,
                bold,
                selection,
                layout: layout.clone(),
            },
            layout,
        )
    }
    fn parts(&self, window: &Window) -> Vec<Part> {
        let mut parts = Vec::new();
        let mut code_group = None;
        for (i, span) in self.text.spans.iter().enumerate() {
            let previous = self.text.spans.get(i.wrapping_sub(1));
            let following = self.text.spans.get(i + 1);
            let leading = span.style.code
                && !previous.is_some_and(|p| p.style.code && p.range.end == span.range.start);
            let trailing = span.style.code
                && !following.is_some_and(|p| p.style.code && p.range.start == span.range.end);
            if leading {
                code_group = Some(i);
            }
            let font = run(&self.style, &span.style, 1, self.bold).font;
            let font_id = window.text_system().resolve_font(&font);
            let font_size = px(self.font_size
                * if span.style.code {
                    self.style.geometry.inline_code_scale
                } else {
                    1.
                });
            let ascent = window.text_system().ascent(font_id, font_size);
            let descent = window.text_system().descent(font_id, font_size).abs();
            let value = &self.text.value[span.range.clone()];
            let mut start = span.range.start;
            for segment in value.split_inclusive('\n') {
                let visible = segment.strip_suffix('\n').unwrap_or(segment);
                if !visible.is_empty() {
                    parts.push(Part {
                        range: start..start + visible.len(),
                        shaped: window.text_system().shape_line(
                            visible.to_owned().into(),
                            px(self.font_size
                                * if span.style.code {
                                    self.style.geometry.inline_code_scale
                                } else {
                                    1.
                                }),
                            &[run(&self.style, &span.style, visible.len(), self.bold)],
                            None,
                        ),
                        code: span.style.code.then_some(code_group.unwrap_or(i)),
                        leading: leading && start == span.range.start,
                        trailing: trailing && start + segment.len() == span.range.end,
                        ascent,
                        descent,
                        break_inside: span.style.target.is_some(),
                    });
                }
                start += segment.len();
            }
        }
        parts
    }
}
impl IntoElement for RichText {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}
impl Element for RichText {
    type RequestLayoutState = ();
    type PrepaintState = ();
    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }
    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        _: &mut App,
    ) -> (LayoutId, ()) {
        let parts = Rc::new(self.parts(window));
        let text = self.text.value.clone();
        let breaks = unicode_linebreak::linebreaks(&text)
            .map(|(i, _)| i)
            .collect::<BTreeSet<_>>();
        let mut clusters = Vec::new();
        let mut owners = Vec::new();
        let mut previous = 0;
        for (i, part) in parts.iter().enumerate() {
            for (offset, ch) in text[previous..part.range.start].char_indices() {
                if ch == '\n' {
                    clusters.push(Cluster {
                        range: previous + offset..previous + offset + 1,
                        width: 0.,
                        break_after: true,
                        hard_break: true,
                        whitespace: false,
                        break_inside: false,
                    });
                    owners.push(None);
                }
            }
            let starts = part
                .shaped
                .runs
                .iter()
                .flat_map(|run| run.glyphs.iter().map(|glyph| glyph.index))
                .collect::<BTreeSet<_>>();
            let mut boundaries = part
                .shaped
                .text
                .grapheme_indices(true)
                .map(|(i, _)| i)
                .filter(|i| *i == 0 || starts.contains(i))
                .collect::<Vec<_>>();
            boundaries.push(part.shaped.len());
            for pair in boundaries.windows(2) {
                let range = part.range.start + pair[0]..part.range.start + pair[1];
                let left = if pair[0] == 0 && part.leading {
                    g::INLINE_CODE_PADDING_X
                } else {
                    0.
                };
                let right = if pair[1] == part.shaped.len() && part.trailing {
                    g::INLINE_CODE_PADDING_X
                } else {
                    0.
                };
                clusters.push(Cluster {
                    range: range.clone(),
                    width: f32::from(
                        part.shaped.x_for_index(pair[1]) - part.shaped.x_for_index(pair[0]),
                    )
                    .abs()
                        + left
                        + right,
                    break_after: breaks.contains(&range.end),
                    hard_break: false,
                    whitespace: text[range].chars().all(|c| c == ' ' || c == '\t'),
                    break_inside: part.break_inside,
                });
                owners.push(Some(i));
            }
            previous = part.range.end;
        }
        for (offset, ch) in text[previous..].char_indices() {
            if ch == '\n' {
                clusters.push(Cluster {
                    range: previous + offset..previous + offset + 1,
                    width: 0.,
                    break_after: true,
                    hard_break: true,
                    whitespace: false,
                    break_inside: false,
                });
                owners.push(None);
            }
        }
        let line = px(self.line);
        let sample = window.text_system().shape_line(
            "x".into(),
            px(self.font_size),
            &[run(&self.style, &InlineStyle::default(), 1, self.bold)],
            None,
        );
        let strut = LineMetrics::new(sample.ascent.into(), sample.descent.into(), line.into());
        let state = self.layout.clone();
        let id =
            window.request_measured_layout(Default::default(), move |known, available, _, _| {
                let width = known
                    .width
                    .or(match available.width {
                        AvailableSpace::Definite(w) => Some(w),
                        _ => None,
                    })
                    .map(f32::from)
                    .unwrap_or(f32::MAX);
                let rows = inline_layout::wrap(&clusters, width);
                let mut result = Vec::new();
                let mut measured = px(0.);
                let mut top = px(0.);
                let mut cursors = parts
                    .iter()
                    .map(|part| part.shaped.cursor())
                    .collect::<Vec<_>>();
                for indices in rows {
                    let source_start = clusters
                        .get(indices.start)
                        .map(|c| c.range.start)
                        .unwrap_or(text.len());
                    let source_end = indices
                        .clone()
                        .last()
                        .map(|i| clusters[i].range.end)
                        .unwrap_or(source_start);
                    let mut fragments = Vec::new();
                    let mut metrics = strut;
                    let mut x = px(0.);
                    let mut j = indices.start;
                    while j < indices.end {
                        let Some(owner) = owners[j] else {
                            j += 1;
                            continue;
                        };
                        let part = &parts[owner];
                        metrics.include(LineMetrics::new(
                            part.ascent.into(),
                            part.descent.into(),
                            line.into(),
                        ));
                        let start = clusters[j].range.start;
                        let mut end = clusters[j].range.end;
                        j += 1;
                        while j < indices.end && owners[j] == Some(owner) {
                            end = clusters[j].range.end;
                            j += 1;
                        }
                        let cursor = &mut cursors[owner];
                        cursor.take_until(start - part.range.start);
                        let shaped = cursor.take_until(end - part.range.start);
                        let left = px(if start == part.range.start && part.leading {
                            g::INLINE_CODE_PADDING_X
                        } else {
                            0.
                        });
                        let right = px(if end == part.range.end && part.trailing {
                            g::INLINE_CODE_PADDING_X
                        } else {
                            0.
                        });
                        x += left;
                        fragments.push(Fragment {
                            range: start..end,
                            x,
                            left,
                            right,
                            code: part.code,
                            shaped: shaped.clone(),
                            ascent: part.ascent,
                            descent: part.descent,
                        });
                        x += shaped.width() + right;
                    }
                    measured = measured.max(x);
                    result.push(Row {
                        fragments,
                        range: source_start..source_end,
                        top,
                        height: px(metrics.height()),
                        baseline: px(metrics.above),
                    });
                    top += px(metrics.height());
                }
                let measured = size(measured.ceil(), top);
                *state.borrow_mut() = Layout {
                    rows: result,
                    bounds: None,
                    size: measured,
                    line,
                };
                measured
            });
        (id, ())
    }
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        _: &mut Window,
        _: &mut App,
    ) {
        self.layout.borrow_mut().bounds = Some(bounds);
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let layout = self.layout.borrow();
        for row in &layout.rows {
            let y = row.top;
            let mut f = 0;
            while f < row.fragments.len() {
                let first = &row.fragments[f];
                let mut end = f + 1;
                if let Some(group) = first.code {
                    while end < row.fragments.len() && row.fragments[end].code == Some(group) {
                        end += 1;
                    }
                    let last = &row.fragments[end - 1];
                    let top = row.baseline - first.ascent.round() - px(g::INLINE_CODE_PADDING_Y);
                    let code_bounds = Bounds::new(
                        bounds.origin + point(first.x - first.left, y + top),
                        size(
                            last.x + last.shaped.width() + last.right - (first.x - first.left),
                            first.ascent.round()
                                + first.descent.round()
                                + px(2. * g::INLINE_CODE_PADDING_Y),
                        ),
                    );
                    let radius = px(g::INLINE_CODE_RADIUS);
                    window.paint_quad(
                        fill(code_bounds, rgb(self.style.palette.surface2)).corner_radii(Corners {
                            top_left: if first.left > px(0.) { radius } else { px(0.) },
                            bottom_left: if first.left > px(0.) { radius } else { px(0.) },
                            top_right: if last.right > px(0.) { radius } else { px(0.) },
                            bottom_right: if last.right > px(0.) { radius } else { px(0.) },
                        }),
                    );
                }
                f = end;
            }
            for fragment in &row.fragments {
                let selected = self.selection.start.max(fragment.range.start)
                    ..self.selection.end.min(fragment.range.end);
                if selected.start < selected.end {
                    let left = fragment
                        .shaped
                        .x_for_index(selected.start - fragment.range.start);
                    let right = fragment
                        .shaped
                        .x_for_index(selected.end - fragment.range.start);
                    window.paint_quad(fill(
                        Bounds::new(
                            bounds.origin + point(fragment.x + left, y),
                            size(right - left, row.height),
                        ),
                        rgba(self.style.palette.terminal.selection_background),
                    ));
                }
                let offset = row.baseline
                    - (layout.line - fragment.shaped.ascent - fragment.shaped.descent) / 2.
                    - fragment.shaped.ascent;
                let origin = bounds.origin + point(fragment.x, y + offset);
                let _ =
                    fragment
                        .shaped
                        .paint(origin, layout.line, TextAlign::Left, None, window, cx);
            }
        }
    }
}

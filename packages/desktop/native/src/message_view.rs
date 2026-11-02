use crate::{
    code_view,
    image_view::ImageCache,
    message_geometry as g,
    message_style::MessageStyle,
    settings_view::{SharedSettings, active_palette},
    theme::colors as c,
};
use gpui::{
    App, ClipboardItem, Context, EventEmitter, FocusHandle, Focusable, FontWeight, KeyDownEvent,
    MouseButton, Pixels, Point, Render, SharedString, StyledText, TextLayout, TextRun, Window, div,
    prelude::*, px, rgb,
};
use paseo_desktop::message_document::{Block, BlockKind, Document, Text};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    ops::Range,
    rc::Rc,
    sync::Arc,
};

pub enum MessageEvent {
    OpenUrl(String),
}

/// A sent attachment shown inside the user bubble. Native owns only the optimistic copy: the
/// daemon timeline stores user text, not attachment metadata.
pub struct PreviewAttachment {
    pub name: String,
    pub subtitle: String,
    pub image: Option<Arc<gpui::RenderImage>>,
}
enum Geometry {
    Plain(TextLayout),
    Mixed(Rc<RefCell<crate::rich_text::Layout>>),
}
impl Geometry {
    fn bounds(&self) -> gpui::Bounds<Pixels> {
        match self {
            Self::Plain(layout) => layout.bounds(),
            Self::Mixed(layout) => layout.borrow().bounds.expect("prepainted inline text"),
        }
    }
    fn index_at(&self, position: Point<Pixels>) -> usize {
        match self {
            Self::Plain(layout) => layout.index_for_position(position).unwrap_or_else(|i| i),
            Self::Mixed(layout) => layout.borrow().index_at(position),
        }
    }
    fn height(&self) -> Pixels {
        match self {
            Self::Plain(layout) => layout.bounds().size.height,
            Self::Mixed(layout) => layout.borrow().size.height,
        }
    }
}
struct Layout {
    geometry: Geometry,
    offset: usize,
    len: usize,
    ready: Rc<Cell<bool>>,
    links: Vec<(Range<usize>, String)>,
}

fn selected_runs(runs: Vec<TextRun>, selected: Range<usize>, color: u32) -> Vec<TextRun> {
    if selected.is_empty() {
        return runs;
    }
    let mut out = Vec::with_capacity(runs.len() + 2);
    let mut offset = 0;
    for run in runs {
        let start = offset;
        let end = offset + run.len;
        offset = end;
        if end <= selected.start || start >= selected.end {
            out.push(run);
            continue;
        }
        let mut cuts = vec![
            start,
            selected.start.clamp(start, end),
            selected.end.clamp(start, end),
            end,
        ];
        cuts.sort_unstable();
        cuts.dedup();
        for pair in cuts.windows(2) {
            if pair[0] == pair[1] {
                continue;
            }
            let mut part = run.clone();
            part.len = pair[1] - pair[0];
            if pair[0] >= selected.start && pair[1] <= selected.end {
                part.background_color = Some(gpui::rgba(color).into());
            }
            out.push(part);
        }
    }
    out
}
pub struct MessageView {
    source: String,
    user: bool,
    timestamp: Option<String>,
    attachments: Vec<PreviewAttachment>,
    footer_below: bool,
    document: Arc<Document>,
    shared: SharedSettings,
    layouts: Vec<Layout>,
    focus: FocusHandle,
    anchor: usize,
    selection: Range<usize>,
    selecting: bool,
    expanded: bool,
    overflowing: bool,
    copied: BTreeMap<usize, u64>,
    copy_revision: u64,
    images: Option<gpui::Entity<ImageCache>>,
    image_root: Option<String>,
}
impl EventEmitter<MessageEvent> for MessageView {}
impl Focusable for MessageView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
impl MessageView {
    pub fn new(
        source: String,
        user: bool,
        timestamp: Option<String>,
        shared: SharedSettings,
        cx: &mut Context<Self>,
    ) -> Self {
        let document = if user {
            Document::plain(&source)
        } else {
            Document::markdown(&source).unwrap_or_else(|_| Document::plain(&source))
        };
        Self {
            source,
            user,
            timestamp,
            attachments: Vec::new(),
            footer_below: false,
            document: Arc::new(document),
            shared,
            layouts: vec![],
            focus: cx.focus_handle(),
            anchor: 0,
            selection: 0..0,
            selecting: false,
            expanded: false,
            overflowing: false,
            copied: BTreeMap::new(),
            copy_revision: 0,
            images: None,
            image_root: None,
        }
    }
    pub fn set_attachments(&mut self, attachments: Vec<PreviewAttachment>, cx: &mut Context<Self>) {
        self.attachments = attachments;
        cx.notify();
    }
    pub fn set_images(&mut self, images: gpui::Entity<ImageCache>, root: Option<String>) {
        self.images = Some(images);
        self.image_root = root;
    }
    /// Inline image sources in this message, for cache prefetch.
    pub fn image_sources(&self) -> Vec<String> {
        self.document.image_sources()
    }
    /// The turn footer sits directly below this message, so its bottom padding collapses.
    pub fn set_footer_below(&mut self, footer_below: bool) {
        self.footer_below = footer_below;
    }
    pub fn set_text(&mut self, source: &str, user: bool, cx: &mut Context<Self>) {
        if self.source == source && self.user == user {
            return;
        }
        let changed_kind = self.user != user;
        self.user = user;
        if changed_kind {
            self.expanded = false;
        }
        self.copied.clear();
        let old_plain = self.document.plain.clone();
        self.document = Arc::new(if self.user {
            Document::plain(source)
        } else {
            Document::markdown(source).unwrap_or_else(|_| Document::plain(source))
        });
        self.source = source.into();
        if changed_kind
            || self.document.plain.get(..self.selection.end) != old_plain.get(..self.selection.end)
        {
            self.selection = 0..0;
            self.anchor = 0;
            self.selecting = false;
        }
        self.layouts.clear();
        cx.notify();
    }
    fn copy(&mut self, key: usize, value: String, cx: &mut Context<Self>) {
        if value.is_empty() {
            return;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(value));
        self.copy_revision = self.copy_revision.wrapping_add(1);
        let revision = self.copy_revision;
        self.copied.insert(key, revision);
        cx.notify();
        let timer = cx
            .background_executor()
            .timer(std::time::Duration::from_millis(1500));
        cx.spawn(async move |this, cx| {
            timer.await;
            let _ = this.update(cx, |this, cx| {
                if this.copied.get(&key) == Some(&revision) {
                    this.copied.remove(&key);
                    cx.notify();
                }
            });
        })
        .detach();
    }
    fn attachment_pill(attachment: &PreviewAttachment) -> gpui::AnyElement {
        let body = if let Some(image) = &attachment.image {
            gpui::img(image.clone())
                .size(px(48.))
                .object_fit(gpui::ObjectFit::Cover)
                .into_any_element()
        } else {
            div()
                .h(px(48.))
                .max_w(px(260.))
                .px_3()
                .gap_2()
                .bg(rgb(c::SURFACE1))
                .flex()
                .items_center()
                .child(
                    div()
                        .w(px(18.))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(crate::chrome::icon("file-text", 16.)),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().truncate().child(attachment.name.clone()))
                        .child(
                            div()
                                .text_size(px(crate::theme::font_size::SM))
                                .text_color(rgb(c::FOREGROUND_MUTED))
                                .truncate()
                                .child(attachment.subtitle.clone()),
                        ),
                )
                .into_any_element()
        };
        div()
            .flex_shrink_0()
            .rounded(px(crate::theme::radius::MD))
            .border_1()
            .border_color(rgb(c::BORDER_ACCENT))
            .overflow_hidden()
            .child(body)
            .into_any_element()
    }
    fn attachments(&self) -> Option<gpui::AnyElement> {
        if self.attachments.is_empty() {
            return None;
        }
        Some(
            div()
                .flex()
                .flex_wrap()
                .gap_2()
                .when(!self.source.trim().is_empty(), |row| row.mb_2())
                .children(self.attachments.iter().map(Self::attachment_pill))
                .into_any_element(),
        )
    }
    fn index_at(&self, position: Point<Pixels>) -> usize {
        let Some(layout) = self
            .layouts
            .iter()
            .filter(|l| l.ready.get())
            .min_by(|a, b| {
                let distance = |item: &Layout| {
                    let bounds = item.geometry.bounds();
                    let y = if position.y < bounds.top() {
                        bounds.top() - position.y
                    } else if position.y > bounds.bottom() {
                        position.y - bounds.bottom()
                    } else {
                        px(0.)
                    };
                    let x = if position.x < bounds.left() {
                        bounds.left() - position.x
                    } else if position.x > bounds.right() {
                        position.x - bounds.right()
                    } else {
                        px(0.)
                    };
                    f32::from(y) * 10000. + f32::from(x)
                };
                distance(a).total_cmp(&distance(b))
            })
        else {
            return 0;
        };
        layout.offset + layout.geometry.index_at(position).min(layout.len)
    }
    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = &event.keystroke;
        let shortcut = if cfg!(target_os = "macos") {
            key.modifiers.platform
        } else {
            key.modifiers.control
        };
        if !shortcut {
            return;
        }
        match key.key.as_str() {
            "a" => {
                self.anchor = 0;
                self.selection = 0..self.document.plain.len();
                cx.notify();
            }
            "c" => {
                if let Some(text) = self
                    .document
                    .selected(self.selection.clone())
                    .filter(|s| !s.is_empty())
                {
                    cx.write_to_clipboard(ClipboardItem::new_string(text.into()));
                }
            }
            _ => return,
        }
        window.prevent_default();
        cx.stop_propagation();
    }
    fn text(
        &mut self,
        text: &Text,
        size: f32,
        line: f32,
        bold: bool,
        code: Option<&str>,
        style: &MessageStyle,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let code_block = code.is_some();
        let links = text
            .spans
            .iter()
            .filter_map(|span| {
                span.style
                    .target
                    .as_ref()
                    .map(|target| (span.range.clone(), target.clone()))
            })
            .collect();
        let mixed = !code_block && text.spans.iter().any(|span| span.style.code);
        let (element, geometry) = if code_block {
            let selected = self.selection.start.saturating_sub(text.offset)
                ..self.selection.end.saturating_sub(text.offset);
            let language = code.unwrap_or("");
            let runs = selected_runs(
                code_view::block_runs(&text.value, language, style),
                selected,
                style.palette.terminal.selection_background,
            );
            let element =
                StyledText::new(SharedString::from(text.value.to_string())).with_runs(runs);
            let layout = element.layout().clone();
            (element.into_any_element(), Geometry::Plain(layout))
        } else if mixed {
            let selected = self.selection.start.saturating_sub(text.offset)
                ..self.selection.end.saturating_sub(text.offset);
            let (element, layout) = crate::rich_text::RichText::new(
                text.clone(),
                style.clone(),
                size,
                line,
                bold,
                selected,
            );
            (element.into_any_element(), Geometry::Mixed(layout))
        } else {
            let mut runs = Vec::new();
            for span in &text.spans {
                let mut cuts = vec![span.range.start, span.range.end];
                for point in [
                    self.selection.start.saturating_sub(text.offset),
                    self.selection.end.saturating_sub(text.offset),
                ] {
                    if span.range.contains(&point) {
                        cuts.push(point);
                    }
                }
                cuts.sort_unstable();
                cuts.dedup();
                for pair in cuts.windows(2) {
                    let mut run =
                        crate::rich_text::run(style, &span.style, pair[1] - pair[0], bold);
                    if self.selection.contains(&(text.offset + pair[0])) {
                        run.background_color = Some(
                            gpui::rgba(if self.user {
                                0x3390ff66
                            } else {
                                style.palette.terminal.selection_background
                            })
                            .into(),
                        );
                    }
                    runs.push(run);
                }
            }
            let element =
                StyledText::new(SharedString::from(text.value.to_string())).with_runs(runs);
            let layout = element.layout().clone();
            (element.into_any_element(), Geometry::Plain(layout))
        };
        let ready = Rc::new(Cell::new(false));
        let mark = ready.clone();
        let id = self.layouts.len();
        self.layouts.push(Layout {
            geometry,
            offset: text.offset,
            len: text.value.len(),
            ready,
            links,
        });
        let font = crate::rich_text::run(
            style,
            &paseo_desktop::message_document::InlineStyle {
                code: code_block,
                ..Default::default()
            },
            1,
            bold,
        )
        .font;
        let font_id = cx.text_system().resolve_font(&font);
        let ascent = cx.text_system().ascent(font_id, px(size));
        let descent = cx.text_system().descent(font_id, px(size)).abs();
        let offset = if mixed {
            px(0.)
        } else {
            crate::rich_text::css_baseline(ascent, descent, px(line))
                - ((px(line) - ascent - descent) / 2. + ascent)
        };
        div()
            .id(("message-text", id))
            .min_w_0()
            .text_size(px(size))
            .line_height(px(line))
            .child(div().relative().top(offset).child(element))
            .child(
                gpui::canvas(move |_, _, _| mark.set(true), |_, (), _, _| {})
                    .h(px(0.))
                    .w(px(0.)),
            )
            .into_any_element()
    }
    fn blocks(
        &mut self,
        blocks: &[Block],
        tight: bool,
        style: &MessageStyle,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        div()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(if tight {
                g::LIST_ITEM_GAP
            } else {
                g::BLOCK_GAP
            }))
            .children(blocks.iter().map(|block| self.block(block, style, cx)))
            .into_any_element()
    }
    fn block(
        &mut self,
        block: &Block,
        style: &MessageStyle,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        match &block.kind {
            BlockKind::Paragraph | BlockKind::Heading(_) | BlockKind::Html => {
                let (size, line) = if let BlockKind::Heading(level) = block.kind {
                    style.geometry.headings[level.clamp(1, 6) as usize - 1]
                } else {
                    (style.geometry.body_size, style.geometry.body_line)
                };
                self.text(
                    block.text.as_ref().unwrap(),
                    size,
                    line,
                    matches!(block.kind, BlockKind::Heading(_)),
                    None,
                    style,
                    cx,
                )
            }
            BlockKind::Code(language) => {
                let text = block.text.as_ref().unwrap();
                let code = paseo_desktop::message_document::copied_code(&text.value).to_owned();
                let key = text.offset;
                let group: SharedString = format!("code-{}-{key}", cx.entity_id()).into();
                div()
                    .group(group.clone())
                    .relative()
                    .w_full()
                    .min_w_0()
                    .p(px(g::CODE_PADDING))
                    .rounded(px(8.))
                    .bg(rgb(style.palette.surface2))
                    .child(self.text(
                        text,
                        style.geometry.code_size,
                        style.geometry.code_line,
                        false,
                        Some(language),
                        style,
                        cx,
                    ))
                    .child(
                        div()
                            .id(("copy-code", text.offset))
                            .absolute()
                            .top_2()
                            .right_2()
                            .size(px(22.))
                            .p_1()
                            .invisible()
                            .group_hover(group, |style| style.visible())
                            .cursor_pointer()
                            .text_color(rgb(style.palette.foreground_muted))
                            .hover(|hovered| hovered.text_color(rgb(style.palette.foreground)))
                            .child(crate::chrome::icon(
                                if self.copied.contains_key(&key) {
                                    "check"
                                } else {
                                    "copy"
                                },
                                14.,
                            ))
                            .tooltip({
                                let title = if language.is_empty() {
                                    "Copy code".into()
                                } else {
                                    format!("Copy {language} code")
                                };
                                move |_, cx| {
                                    cx.new(|_| crate::chrome::Tooltip(title.clone())).into()
                                }
                            })
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.copy(key, code.clone(), cx);
                                }),
                            ),
                    )
                    .into_any_element()
            }
            BlockKind::Quote(title) => div()
                .min_w_0()
                .border_l(px(g::QUOTE_BORDER))
                .border_color(rgb(style.palette.surface2))
                .bg(rgb(style.palette.surface1))
                .p(px(g::QUOTE_PADDING))
                .when(!title.is_empty(), |v| {
                    v.child(
                        div()
                            .mb_2()
                            .font_weight(FontWeight::BOLD)
                            .child(title.clone()),
                    )
                })
                .child(self.blocks(&block.children, false, style, cx))
                .into_any_element(),
            BlockKind::List(ordered, start) => div()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(g::LIST_ITEM_GAP))
                .children(block.children.iter().enumerate().map(|(i, item)| {
                    let marker = match item.kind {
                        BlockKind::Item(1) => "☐".into(),
                        BlockKind::Item(2) => "☑".into(),
                        _ => {
                            if *ordered {
                                format!("{}.", *start + i as u32)
                            } else {
                                "•".into()
                            }
                        }
                    };
                    div()
                        .min_w_0()
                        .flex()
                        .gap(px(7.))
                        .child(
                            div()
                                .w(px(g::LIST_INDENT - 7.))
                                .flex_shrink_0()
                                .text_size(px(style.geometry.body_size))
                                .line_height(px(style.geometry.body_line))
                                .text_color(rgb(style.palette.foreground_muted))
                                .child(marker),
                        )
                        .child(div().flex_1().min_w_0().child(self.blocks(
                            &item.children,
                            true,
                            style,
                            cx,
                        )))
                }))
                .into_any_element(),
            BlockKind::Table => div()
                .w_full()
                .min_w_0()
                .rounded(px(6.))
                .border_1()
                .border_color(rgb(style.palette.border))
                .overflow_hidden()
                .child(self.blocks(&block.children, true, style, cx))
                .into_any_element(),
            BlockKind::Row => div()
                .w_full()
                .min_w_0()
                .flex()
                .children(block.children.iter().map(|cell| {
                    div()
                        .flex_1()
                        .min_w_0()
                        .p_2()
                        .border_r_1()
                        .border_b_1()
                        .border_color(rgb(style.palette.border))
                        .when(matches!(cell.kind, BlockKind::Cell(true, _)), |v| {
                            v.bg(rgb(style.palette.surface2))
                        })
                        .child(self.block(cell, style, cx))
                }))
                .into_any_element(),
            BlockKind::Cell(header, _) => self.text(
                block.text.as_ref().unwrap(),
                style.geometry.body_size,
                style.geometry.body_line,
                *header,
                None,
                style,
                cx,
            ),
            BlockKind::Image { source, alt } => match self.images.clone() {
                Some(images) => crate::image_view::render_image(
                    &images.read(cx),
                    source,
                    self.image_root.as_deref(),
                    style,
                ),
                None => div()
                    .text_size(px(g::BODY_SIZE))
                    .text_color(rgb(c::FOREGROUND_MUTED))
                    .child(if alt.is_empty() {
                        source.clone()
                    } else {
                        alt.clone()
                    })
                    .into_any_element(),
            },
            BlockKind::Rule => div()
                .w_full()
                .h(px(1.))
                .bg(rgb(style.palette.border))
                .into_any_element(),
            BlockKind::Item(_) | BlockKind::TableSection => {
                self.blocks(&block.children, true, style, cx)
            }
        }
    }
}
impl Render for MessageView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.layouts.clear();
        let settings = self.shared.get();
        let style = MessageStyle::resolve(&settings, active_palette(&self.shared, window));
        let document = self.document.clone();
        let body = self.blocks(&document.blocks, false, &style, cx);
        let group: SharedString = format!("message-{}", cx.entity_id()).into();
        let body = if self.user {
            let measure = cx.entity();
            let bubble = div()
                .self_end()
                .max_w_full()
                .min_w_0()
                .p_4()
                .rounded(px(16.))
                .rounded_tr(px(2.))
                .bg(rgb(style.palette.surface3))
                .children(self.attachments())
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .when(!self.expanded, |v| v.max_h(px(240.)).overflow_hidden())
                        .child(body),
                )
                .when(self.overflowing, |bubble| {
                    bubble.child(
                        div()
                            .id("expand-message")
                            .mt_2()
                            .text_color(rgb(style.palette.foreground_muted))
                            .cursor_pointer()
                            .child(if self.expanded {
                                "Show less"
                            } else {
                                "Show more"
                            })
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.expanded = !this.expanded;
                                    cx.notify();
                                }),
                            ),
                    )
                });
            let measure_element = gpui::canvas(
                move |_, _, cx| {
                    measure.update(cx, |this, cx| {
                        let overflowing = this
                            .layouts
                            .iter()
                            .filter(|l| l.ready.get())
                            .any(|l| l.geometry.height() > px(240.));
                        if this.overflowing != overflowing {
                            this.overflowing = overflowing;
                            cx.notify();
                        }
                    })
                },
                |_, (), _, _| {},
            )
            .h(px(0.))
            .w(px(0.));
            let column = div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .items_end()
                .group(group.clone())
                .child(bubble)
                .child(measure_element)
                .when(!self.source.trim().is_empty(), |v| {
                    v.child(
                        div()
                            .h(px(24.))
                            .mt_2()
                            .flex()
                            .items_center()
                            .gap_2()
                            .invisible()
                            .group_hover(group, |v| v.visible())
                            .when_some(self.timestamp.clone(), |row, timestamp| {
                                row.child(
                                    div()
                                        .text_size(px(13.))
                                        .text_color(rgb(c::FOREGROUND_MUTED))
                                        .child(timestamp),
                                )
                            })
                            .child(
                                div()
                                    .id(("copy-user-message", cx.entity_id()))
                                    .mr(px(-4.))
                                    .p_1()
                                    .cursor_pointer()
                                    .text_color(rgb(c::FOREGROUND_MUTED))
                                    .hover(|v| v.text_color(rgb(c::FOREGROUND)))
                                    .child(crate::chrome::icon(
                                        if self.copied.contains_key(&usize::MAX) {
                                            "check"
                                        } else {
                                            "copy"
                                        },
                                        14.,
                                    ))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.copy(usize::MAX, this.source.clone(), cx);
                                        }),
                                    ),
                            ),
                    )
                });
            div()
                .w_full()
                .flex()
                .justify_end()
                .child(column)
                .into_any_element()
        } else {
            div()
                .w_full()
                .pt_3()
                .when(!self.footer_below, |v| v.pb_3())
                .child(body)
                .into_any_element()
        };
        div()
            .id("message-view")
            .w_full()
            .min_w_0()
            .track_focus(&self.focus)
            .cursor_text()
            .on_key_down(cx.listener(Self::key_down))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                    this.focus.focus(window, cx);
                    let index = this.index_at(event.position);
                    if !event.modifiers.shift {
                        this.anchor = index;
                    }
                    this.selection = this.anchor.min(index)..this.anchor.max(index);
                    this.selecting = true;
                    cx.notify();
                    cx.stop_propagation();
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _, cx| {
                if this.selecting {
                    let index = this.index_at(event.position);
                    let selected = this.anchor.min(index)..this.anchor.max(index);
                    if this.selection != selected {
                        this.selection = selected;
                        cx.notify();
                    }
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseUpEvent, _, cx| {
                    if !this.selecting {
                        return;
                    }
                    this.selecting = false;
                    if this.selection.is_empty() {
                        let index = this.index_at(event.position);
                        for layout in &this.layouts {
                            if layout.ready.get()
                                && layout.geometry.bounds().contains(&event.position)
                                && index >= layout.offset
                                && index <= layout.offset + layout.len
                            {
                                if let Some((_, url)) = layout
                                    .links
                                    .iter()
                                    .find(|(range, _)| range.contains(&(index - layout.offset)))
                                {
                                    if url.starts_with("https://")
                                        || url.starts_with("http://")
                                        || url.starts_with("mailto:")
                                    {
                                        cx.emit(MessageEvent::OpenUrl(url.clone()));
                                    }
                                    break;
                                }
                            }
                        }
                    }
                    cx.notify();
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.selecting = false),
            )
            .child(body)
    }
}

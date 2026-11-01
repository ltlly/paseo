use crate::{message_geometry as g, theme::colors as c};
use gpui::{
    App, ClipboardItem, Context, EventEmitter, FocusHandle, Focusable, FontStyle, FontWeight,
    KeyDownEvent, MouseButton, Pixels, Point, Render, SharedString, StyledText, TextLayout,
    TextRun, Window, div, prelude::*, px, rgb,
};
use paseo_desktop::message_document::{Block, BlockKind, Document, Text};
use std::{cell::Cell, collections::BTreeMap, ops::Range, rc::Rc, sync::Arc};

pub enum MessageEvent {
    OpenUrl(String),
}
struct Layout {
    layout: TextLayout,
    offset: usize,
    len: usize,
    ready: Rc<Cell<bool>>,
    links: Vec<(Range<usize>, String)>,
}
pub struct MessageView {
    source: String,
    user: bool,
    document: Arc<Document>,
    layouts: Vec<Layout>,
    focus: FocusHandle,
    anchor: usize,
    selection: Range<usize>,
    selecting: bool,
    expanded: bool,
    overflowing: bool,
    copied: BTreeMap<usize, u64>,
    copy_revision: u64,
}
impl EventEmitter<MessageEvent> for MessageView {}
impl Focusable for MessageView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
impl MessageView {
    pub fn new(source: String, user: bool, cx: &mut Context<Self>) -> Self {
        let document = if user {
            Document::plain(&source)
        } else {
            Document::markdown(&source).unwrap_or_else(|_| Document::plain(&source))
        };
        Self {
            source,
            user,
            document: Arc::new(document),
            layouts: vec![],
            focus: cx.focus_handle(),
            anchor: 0,
            selection: 0..0,
            selecting: false,
            expanded: false,
            overflowing: false,
            copied: BTreeMap::new(),
            copy_revision: 0,
        }
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
    fn index_at(&self, position: Point<Pixels>) -> usize {
        let Some(layout) = self
            .layouts
            .iter()
            .filter(|l| l.ready.get())
            .min_by(|a, b| {
                let distance = |item: &Layout| {
                    let bounds = item.layout.bounds();
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
        layout.offset
            + layout
                .layout
                .index_for_position(position)
                .unwrap_or_else(|i| i)
                .min(layout.len)
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
        _cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let mut runs = Vec::new();
        let mut links = Vec::new();
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
                let mut font = gpui::font(if span.style.code {
                    crate::fonts::code_family()
                } else {
                    crate::fonts::ui_family()
                });
                if bold || span.style.bold {
                    font.weight = FontWeight::BOLD;
                }
                if span.style.italic {
                    font.style = FontStyle::Italic;
                }
                let selected = self.selection.contains(&(text.offset + pair[0]));
                runs.push(TextRun {
                    len: pair[1] - pair[0],
                    font,
                    color: rgb(if span.style.target.is_some() {
                        c::ACCENT_BRIGHT
                    } else {
                        c::FOREGROUND
                    })
                    .into(),
                    background_color: if selected {
                        Some(gpui::rgba(0x3390ff66).into())
                    } else if span.style.code {
                        Some(rgb(c::SURFACE2).into())
                    } else {
                        None
                    },
                    underline: None,
                    strikethrough: span.style.strike.then_some(gpui::StrikethroughStyle {
                        thickness: px(1.),
                        color: None,
                    }),
                });
            }
            if let Some(target) = &span.style.target {
                links.push((span.range.clone(), target.clone()));
            }
        }
        let element = StyledText::new(SharedString::from(text.value.to_string())).with_runs(runs);
        let layout = element.layout().clone();
        let ready = Rc::new(Cell::new(false));
        let mark = ready.clone();
        let id = self.layouts.len();
        self.layouts.push(Layout {
            layout,
            offset: text.offset,
            len: text.value.len(),
            ready,
            links,
        });
        div()
            .id(("message-text", id))
            .min_w_0()
            .text_size(px(size))
            .line_height(px(line))
            .child(element)
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
            .children(blocks.iter().map(|block| self.block(block, cx)))
            .into_any_element()
    }
    fn block(&mut self, block: &Block, cx: &mut Context<Self>) -> gpui::AnyElement {
        match &block.kind {
            BlockKind::Paragraph | BlockKind::Heading(_) | BlockKind::Html => {
                let (size, line) = if let BlockKind::Heading(level) = block.kind {
                    g::HEADINGS[level.clamp(1, 6) as usize - 1]
                } else {
                    (g::BODY_SIZE, g::BODY_LINE)
                };
                self.text(
                    block.text.as_ref().unwrap(),
                    size,
                    line,
                    matches!(block.kind, BlockKind::Heading(_)),
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
                    .bg(rgb(c::SURFACE2))
                    .child(self.text(text, g::CODE_SIZE, (g::CODE_SIZE * 1.45).round(), false, cx))
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
                            .text_color(rgb(c::FOREGROUND_MUTED))
                            .hover(|style| style.text_color(rgb(c::FOREGROUND)))
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
                .border_color(rgb(c::SURFACE2))
                .bg(rgb(c::SURFACE1))
                .p(px(g::QUOTE_PADDING))
                .when(!title.is_empty(), |v| {
                    v.child(
                        div()
                            .mb_2()
                            .font_weight(FontWeight::BOLD)
                            .child(title.clone()),
                    )
                })
                .child(self.blocks(&block.children, false, cx))
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
                                .text_size(px(g::BODY_SIZE))
                                .line_height(px(g::BODY_LINE))
                                .text_color(rgb(c::FOREGROUND_MUTED))
                                .child(marker),
                        )
                        .child(div().flex_1().min_w_0().child(self.blocks(
                            &item.children,
                            true,
                            cx,
                        )))
                }))
                .into_any_element(),
            BlockKind::Table => div()
                .w_full()
                .min_w_0()
                .rounded(px(6.))
                .border_1()
                .border_color(rgb(c::BORDER))
                .overflow_hidden()
                .child(self.blocks(&block.children, true, cx))
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
                        .border_color(rgb(c::BORDER))
                        .when(matches!(cell.kind, BlockKind::Cell(true, _)), |v| {
                            v.bg(rgb(c::SURFACE2))
                        })
                        .child(self.block(cell, cx))
                }))
                .into_any_element(),
            BlockKind::Cell(header, _) => self.text(
                block.text.as_ref().unwrap(),
                g::BODY_SIZE,
                g::BODY_LINE,
                *header,
                cx,
            ),
            BlockKind::Rule => div()
                .w_full()
                .h(px(1.))
                .bg(rgb(c::BORDER))
                .into_any_element(),
            BlockKind::Item(_) | BlockKind::TableSection => self.blocks(&block.children, true, cx),
        }
    }
}
impl Render for MessageView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.layouts.clear();
        let document = self.document.clone();
        let body = self.blocks(&document.blocks, false, cx);
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
                .bg(rgb(c::SURFACE3))
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
                            .text_color(rgb(c::FOREGROUND_MUTED))
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
                            .any(|l| l.layout.bounds().size.height > px(240.));
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
                            .invisible()
                            .group_hover(group, |v| v.visible())
                            .child(
                                div()
                                    .id("copy-user-message")
                                    .size(px(24.))
                                    .cursor_pointer()
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
            div().w_full().py_3().child(body).into_any_element()
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
                                && layout.layout.bounds().contains(&event.position)
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

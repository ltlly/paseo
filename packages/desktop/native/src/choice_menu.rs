use crate::{
    chrome::icon,
    settings_view::{SharedSettings, active_palette},
};
use gpui::{
    App, Bounds, ClipboardItem, Context, ElementInputHandler, EntityInputHandler, EventEmitter,
    FocusHandle, Focusable, MouseButton, Pixels, Point, Render, ScrollHandle, ShapedLine, TextRun,
    UTF16Selection, Window, canvas, div, fill, font, point, prelude::*, px, rgb, size,
};
use paseo_desktop::{agent_controls::OptionRow, editing::Editing};
use std::ops::Range;

pub struct Pick(pub Option<String>);
pub struct ChoiceMenu {
    rows: Vec<OptionRow>,
    selected: Option<String>,
    query: Editing,
    active: usize,
    focus: FocusHandle,
    bounds: Bounds<Pixels>,
    line: Option<ShapedLine>,
    scroll_x: Pixels,
    scroll: ScrollHandle,
    placeholder: String,
    revision: u64,
    selecting: bool,
    searchable: bool,
    /// The window's settings, so the menu follows the active theme.
    shared: SharedSettings,
}
impl EventEmitter<Pick> for ChoiceMenu {}
impl Focusable for ChoiceMenu {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
impl ChoiceMenu {
    pub fn new(
        rows: Vec<OptionRow>,
        selected: Option<String>,
        title: &str,
        searchable: bool,
        shared: SharedSettings,
        cx: &mut Context<Self>,
    ) -> Self {
        let active = rows
            .iter()
            .position(|o| Some(&o.id) == selected.as_ref())
            .unwrap_or(0);
        let scroll = ScrollHandle::new();
        scroll.scroll_to_item(active);
        Self {
            rows,
            selected,
            active,
            query: Editing::default(),
            focus: cx.focus_handle(),
            bounds: Bounds::default(),
            line: None,
            scroll_x: px(0.),
            scroll,
            searchable,
            revision: 0,
            selecting: false,
            placeholder: format!("Search {}…", title.to_lowercase()),
            shared,
        }
    }
    fn filtered(&self) -> Vec<OptionRow> {
        let query = self.query.text.trim().to_lowercase();
        self.rows
            .iter()
            .filter(|o| {
                query.is_empty()
                    || o.label.to_lowercase().contains(&query)
                    || o.id.to_lowercase().contains(&query)
            })
            .cloned()
            .collect()
    }
    fn edited(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.revision != self.query.revision {
            self.active = 0;
            self.scroll.scroll_to_item(0);
            self.revision = self.query.revision;
        }
        window.notify_text_input_changed(&self.focus);
        cx.notify();
    }
    fn key_down(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.query.marked.is_some() {
            return;
        }
        let key = &event.keystroke;
        let shortcut = if cfg!(target_os = "macos") {
            key.modifiers.platform
        } else {
            key.modifiers.control
        };
        if !self.searchable && !matches!(key.key.as_str(), "up" | "down" | "enter" | "escape") {
            return;
        }
        if shortcut && !key.modifiers.alt {
            match key.key.as_str() {
                "a" => {
                    self.query.select(0, false);
                    self.query.select(self.query.text.len(), true);
                }
                "c" | "x" => {
                    if !self.query.selection.is_empty() {
                        cx.write_to_clipboard(ClipboardItem::new_string(
                            self.query.text[self.query.selection.clone()].into(),
                        ));
                        if key.key == "x" {
                            self.query.replace(None, "", false, None);
                        }
                    }
                }
                "v" => {
                    if let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) {
                        self.query
                            .replace(None, &text.replace(['\r', '\n'], " "), false, None);
                    }
                }
                _ => return,
            }
            self.edited(window, cx);
        } else if key.modifiers.control || key.modifiers.platform || key.modifiers.alt {
            return;
        } else {
            match key.key.as_str() {
                "escape" => cx.emit(Pick(None)),
                "enter" => {
                    if let Some(row) = self.filtered().get(self.active) {
                        cx.emit(Pick(Some(row.id.clone())));
                    }
                }
                "up" | "down" => {
                    let count = self.filtered().len();
                    if count > 0 {
                        self.active = if key.key == "up" {
                            (self.active + count - 1) % count
                        } else {
                            (self.active + 1) % count
                        };
                        self.scroll.scroll_to_item(self.active);
                    }
                    cx.notify();
                }
                "backspace" | "delete" => {
                    self.query.delete(key.key == "delete");
                    self.edited(window, cx);
                }
                "left" | "right" => {
                    self.query
                        .horizontal(key.key == "right", key.modifiers.shift);
                    self.edited(window, cx);
                }
                "home" | "end" => {
                    self.query.select(
                        if key.key == "home" {
                            0
                        } else {
                            self.query.text.len()
                        },
                        key.modifiers.shift,
                    );
                    self.edited(window, cx);
                }
                _ => return,
            }
        }
        cx.stop_propagation();
        window.prevent_default();
    }
    fn caret(&self, byte: usize) -> Point<Pixels> {
        self.bounds.origin
            + point(
                self.line
                    .as_ref()
                    .map_or(px(0.), |line| line.x_for_index(byte))
                    - self.scroll_x,
                px(0.),
            )
    }
}
impl EntityInputHandler for ChoiceMenu {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.query.byte_range(range);
        *actual = Some(self.query.utf16_range(range.clone()));
        Some(self.query.text[range].into())
    }
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.query.utf16_range(self.query.selection.clone()),
            reversed: self.query.reversed,
        })
    }
    fn set_selected_text_range(
        &mut self,
        range: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.query.selection = self.query.byte_range(range);
        self.query.reversed = false;
        self.edited(window, cx);
    }
    fn text_length_utf16(&mut self, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        Some(self.query.text.encode_utf16().count())
    }
    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.query.marked.clone().map(|r| self.query.utf16_range(r))
    }
    fn unmark_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.query.marked = None;
        self.edited(window, cx);
    }
    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.query
            .replace(range, &text.replace(['\r', '\n'], " "), false, None);
        self.edited(window, cx);
    }
    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.query.replace(range, text, true, selected);
        self.edited(window, cx);
    }
    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        Some(Bounds::new(
            self.caret(self.query.byte_range(range).start),
            size(px(1.), px(20.)),
        ))
    }
    fn character_index_for_point(
        &mut self,
        position: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        Some(
            self.query.utf16_offset(
                self.line
                    .as_ref()?
                    .closest_index_for_x(position.x - self.bounds.left() + self.scroll_x),
            ),
        )
    }
}
impl Render for ChoiceMenu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = active_palette(&self.shared, window);
        let measure = cx.entity();
        let paint = cx.entity();
        let rows = self.filtered();
        div()
            .id("choice-menu")
            .track_focus(&self.focus)
            .w_full()
            .flex()
            .flex_col()
            .rounded(px(8.))
            .overflow_hidden()
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface0))
            .text_size(px(crate::theme::font_size::BASE))
            .text_color(rgb(palette.foreground))
            .on_key_down(cx.listener(Self::key_down))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.selecting = false),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.selecting = false),
            )
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(Pick(None))))
            .when(self.searchable, |root| {
                root.child(
                    div()
                        .h(px(44.))
                        .flex_shrink_0()
                        .px_3()
                        .flex()
                        .gap_2()
                        .items_center()
                        .bg(rgb(palette.surface1))
                        .border_b_1()
                        .border_color(rgb(palette.border))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                                cx.stop_propagation();
                                this.focus.focus(window, cx);
                                this.selecting = true;
                                let index = this.line.as_ref().map_or(0, |line| {
                                    line.closest_index_for_x(
                                        event.position.x - this.bounds.left() + this.scroll_x,
                                    )
                                });
                                this.query.select(index, event.modifiers.shift);
                                this.edited(window, cx);
                            }),
                        )
                        .on_mouse_move(cx.listener(
                            |this, event: &gpui::MouseMoveEvent, window, cx| {
                                if this.selecting {
                                    let index = this.line.as_ref().map_or(0, |line| {
                                        line.closest_index_for_x(
                                            event.position.x - this.bounds.left() + this.scroll_x,
                                        )
                                    });
                                    this.query.select(index, true);
                                    this.edited(window, cx);
                                }
                            },
                        ))
                        .child(icon("search", 16.).text_color(rgb(palette.foreground_muted)))
                        .child(
                            canvas(
                                move |bounds, window, cx| {
                                    measure.update(cx, |this, _| {
                                        this.bounds = bounds;
                                        let empty = this.query.text.is_empty();
                                        let text = if empty {
                                            this.placeholder.clone()
                                        } else {
                                            this.query.text.clone()
                                        };
                                        let run = TextRun {
                                            len: text.len(),
                                            font: font(crate::fonts::ui_family()),
                                            color: rgb(if empty {
                                                palette.foreground_muted
                                            } else {
                                                palette.foreground
                                            })
                                            .into(),
                                            background_color: None,
                                            underline: None,
                                            strikethrough: None,
                                        };
                                        this.line = Some(window.text_system().shape_line(
                                            text.into(),
                                            px(crate::theme::font_size::BASE),
                                            &[run],
                                            None,
                                        ));
                                        let x = this
                                            .line
                                            .as_ref()
                                            .unwrap()
                                            .x_for_index(this.query.cursor());
                                        this.scroll_x = this
                                            .scroll_x
                                            .max(x - bounds.size.width + px(1.))
                                            .min(x)
                                            .max(px(0.));
                                    })
                                },
                                move |bounds, (), window, cx| {
                                    paint.update(cx, |this, cx| {
                                        window.handle_input(
                                            &this.focus,
                                            ElementInputHandler::new(bounds, paint.clone()),
                                            cx,
                                        );
                                        window.with_content_mask(
                                            Some(gpui::ContentMask { bounds }),
                                            |window| {
                                                if let Some(line) = &this.line {
                                                    if !this.query.selection.is_empty() {
                                                        window.paint_quad(fill(
                                                            Bounds::new(
                                                                this.caret(
                                                                    this.query.selection.start,
                                                                ),
                                                                size(
                                                                    line.x_for_index(
                                                                        this.query.selection.end,
                                                                    ) - line.x_for_index(
                                                                        this.query.selection.start,
                                                                    ),
                                                                    px(20.),
                                                                ),
                                                            ),
                                                            rgb(palette.surface3),
                                                        ));
                                                    }
                                                    let _ = line.paint(
                                                        bounds.origin
                                                            - point(this.scroll_x, px(0.)),
                                                        px(20.),
                                                        gpui::TextAlign::Left,
                                                        None,
                                                        window,
                                                        cx,
                                                    );
                                                    if let Some(mark) = &this.query.marked {
                                                        window.paint_quad(fill(
                                                            Bounds::new(
                                                                this.caret(mark.start)
                                                                    + point(px(0.), px(19.)),
                                                                size(
                                                                    line.x_for_index(mark.end)
                                                                        - line.x_for_index(
                                                                            mark.start,
                                                                        ),
                                                                    px(1.),
                                                                ),
                                                            ),
                                                            rgb(palette.foreground),
                                                        ));
                                                    }
                                                    if this.focus.is_focused(window) {
                                                        window.paint_quad(fill(
                                                            Bounds::new(
                                                                this.caret(this.query.cursor()),
                                                                size(px(1.), px(20.)),
                                                            ),
                                                            rgb(palette.foreground),
                                                        ));
                                                    }
                                                }
                                            },
                                        );
                                    })
                                },
                            )
                            .flex_1()
                            .h(px(20.)),
                        ),
                )
            })
            .child(
                div()
                    .id("choice-options")
                    .track_scroll(&self.scroll)
                    .overflow_y_scroll()
                    .max_h(px(320.))
                    .flex()
                    .flex_col()
                    .children(rows.iter().enumerate().map(|(index, row)| {
                        let id = row.id.clone();
                        div()
                            .id(("choice", index))
                            .flex_shrink_0()
                            .min_h(px(36.))
                            .px_3()
                            .py_2()
                            .flex()
                            .gap_2()
                            .items_center()
                            .cursor_pointer()
                            .when(index == self.active, |v| v.bg(rgb(palette.surface1)))
                            .hover(|v| v.bg(rgb(palette.surface1)))
                            .child(div().flex_1().child(row.label.clone()))
                            .child(div().w(px(16.)).when(
                                self.selected.as_deref() == Some(&row.id),
                                |v| {
                                    v.child(
                                        icon("check", 16.)
                                            .text_color(rgb(palette.foreground_muted)),
                                    )
                                },
                            ))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |_, _, _, cx| {
                                    cx.stop_propagation();
                                    cx.emit(Pick(Some(id.clone())));
                                }),
                            )
                            .on_mouse_move(cx.listener(move |this, _, _, cx| {
                                if this.active != index {
                                    this.active = index;
                                    cx.notify();
                                }
                            }))
                    }))
                    .when(rows.is_empty(), |v| {
                        v.child(
                            div()
                                .p_3()
                                .text_color(rgb(palette.foreground_muted))
                                .child("No results found."),
                        )
                    }),
            )
    }
}

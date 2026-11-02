//! Single-line text input for forms embedded in the conversation (question permissions).
//! It follows the menu search field: the entity owns the text, selection and IME state, so the
//! parent form only reads the value and reacts to change/submit events.
use crate::{
    message_style::MessageStyle,
    settings_view::{SharedSettings, active_palette},
};
use gpui::{
    App, Bounds, ClipboardItem, Context, ElementInputHandler, EntityInputHandler, EventEmitter,
    FocusHandle, Focusable, KeyDownEvent, MouseButton, MouseMoveEvent, Pixels, Point, Render,
    UTF16Selection, Window, canvas, div, fill, font, point, prelude::*, px, rgb, size,
};
use paseo_desktop::editing::Editing;
use std::ops::Range;

#[derive(Clone, Debug)]
pub enum InlineInputEvent {
    Changed,
    Submit,
}

pub struct InlineInput {
    focus: FocusHandle,
    editing: Editing,
    placeholder: String,
    bounds: Bounds<Pixels>,
    line: Option<gpui::ShapedLine>,
    scroll_x: Pixels,
    selecting: bool,
    shared: SharedSettings,
    plain: bool,
}

impl EventEmitter<InlineInputEvent> for InlineInput {}

impl InlineInput {
    pub fn new(placeholder: String, shared: SharedSettings, cx: &mut Context<Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
            editing: Editing::default(),
            placeholder,
            bounds: Bounds::default(),
            line: None,
            scroll_x: px(0.),
            selecting: false,
            shared,
            plain: false,
        }
    }
    pub fn command_center(shared: SharedSettings, cx: &mut Context<Self>) -> Self {
        let mut input = Self::new("Search files...".into(), shared, cx);
        input.plain = true;
        input
    }
    pub fn is_composing(&self) -> bool {
        self.editing.marked.is_some()
    }
    fn style(&self, window: &Window) -> MessageStyle {
        MessageStyle::resolve(&self.shared.get(), active_palette(&self.shared, window))
    }
    pub fn text(&self) -> &str {
        &self.editing.text
    }
    pub fn set_placeholder(&mut self, placeholder: String, cx: &mut Context<Self>) {
        if self.placeholder != placeholder {
            self.placeholder = placeholder;
            cx.notify();
        }
    }
    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        if self.editing.text == text {
            return;
        }
        self.editing.replace_all(text);
        self.editing.select(self.editing.text.len(), false);
        cx.notify();
    }
    fn edited(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.notify_text_input_changed(&self.focus);
        cx.emit(InlineInputEvent::Changed);
        cx.notify();
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
    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing.marked.is_some() {
            return;
        }
        let key = &event.keystroke;
        let shortcut = if cfg!(target_os = "macos") {
            key.modifiers.platform
        } else {
            key.modifiers.control
        };
        if shortcut && !key.modifiers.alt {
            match key.key.as_str() {
                "a" => {
                    self.editing.select(0, false);
                    self.editing.select(self.editing.text.len(), true);
                }
                "c" | "x" => {
                    if !self.editing.selection.is_empty() {
                        cx.write_to_clipboard(ClipboardItem::new_string(
                            self.editing.text[self.editing.selection.clone()].into(),
                        ));
                        if key.key == "x" {
                            self.editing.replace(None, "", false, None);
                        }
                    }
                }
                "v" => {
                    if let Some(text) = cx.read_from_clipboard().and_then(|clip| clip.text()) {
                        self.editing
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
                "enter" => cx.emit(InlineInputEvent::Submit),
                "backspace"
                    if self.plain
                        && paseo_desktop::command_center_match::query_is_empty(
                            &self.editing.text,
                        ) =>
                {
                    return;
                }
                "backspace" | "delete" => {
                    self.editing.delete(key.key == "delete");
                    self.edited(window, cx);
                }
                "left" | "right" => {
                    self.editing
                        .horizontal(key.key == "right", key.modifiers.shift);
                    self.edited(window, cx);
                }
                "home" | "end" => {
                    self.editing.select(
                        if key.key == "home" {
                            0
                        } else {
                            self.editing.text.len()
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
}

impl EntityInputHandler for InlineInput {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.editing.byte_range(range);
        *actual = Some(self.editing.utf16_range(range.clone()));
        Some(self.editing.text[range].into())
    }
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.editing.utf16_range(self.editing.selection.clone()),
            reversed: self.editing.reversed,
        })
    }
    fn set_selected_text_range(
        &mut self,
        range: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.editing.selection = self.editing.byte_range(range);
        self.editing.reversed = false;
        self.edited(window, cx);
    }
    fn text_length_utf16(&mut self, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        Some(self.editing.text.encode_utf16().count())
    }
    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.editing
            .marked
            .clone()
            .map(|range| self.editing.utf16_range(range))
    }
    fn unmark_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editing.marked = None;
        self.edited(window, cx);
    }
    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.editing
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
        self.editing.replace(range, text, true, selected);
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
            self.caret(self.editing.byte_range(range).start),
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
            self.editing.utf16_offset(
                self.line
                    .as_ref()?
                    .closest_index_for_x(position.x - self.bounds.left() + self.scroll_x),
            ),
        )
    }
}

impl Focusable for InlineInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for InlineInput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let style = self.style(window);
        let line_height = if self.plain {
            crate::fonts::strut_for(window, crate::theme::font_size::BASE, &style.ui_family)
        } else {
            px(20.)
        };
        let measure = cx.entity();
        let paint = cx.entity();
        div()
            .id("inline-input")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key_down))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.selecting = false),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.selecting = false),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                    this.focus.focus(window, cx);
                    this.selecting = true;
                    let index = this.line.as_ref().map_or(0, |line| {
                        line.closest_index_for_x(
                            event.position.x - this.bounds.left() + this.scroll_x,
                        )
                    });
                    this.editing.select(index, event.modifiers.shift);
                    this.edited(window, cx);
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| {
                if this.selecting {
                    let index = this.line.as_ref().map_or(0, |line| {
                        line.closest_index_for_x(
                            event.position.x - this.bounds.left() + this.scroll_x,
                        )
                    });
                    this.editing.select(index, true);
                    this.edited(window, cx);
                }
            }))
            .w_full()
            .h(if self.plain {
                line_height + px(8.)
            } else {
                px(38.)
            })
            .when(self.plain, |input| input.py_1())
            .when(!self.plain, |input| {
                input
                    .px_3()
                    .py_2()
                    .rounded(px(crate::theme::radius::LG))
                    .border_1()
                    .border_color(rgb(if self.focus.is_focused(window) {
                        style.palette.border_accent
                    } else {
                        style.palette.border
                    }))
                    .bg(rgb(style.palette.surface2))
            })
            .font_family(style.ui_family.clone())
            .text_size(px(crate::theme::font_size::BASE))
            .child(
                canvas(
                    move |bounds, window, cx| {
                        let style = MessageStyle::resolve(
                            &measure.read(cx).shared.get(),
                            active_palette(&measure.read(cx).shared, window),
                        );
                        measure.update(cx, |this, _| {
                            this.bounds = bounds;
                            let empty = this.editing.text.is_empty();
                            let text = if empty {
                                this.placeholder.clone()
                            } else {
                                this.editing.text.clone()
                            };
                            let run = gpui::TextRun {
                                len: text.len(),
                                font: font(style.ui_family.clone()),
                                color: rgb(if empty {
                                    style.palette.foreground_muted
                                } else {
                                    style.palette.foreground
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
                                .x_for_index(this.editing.cursor());
                            this.scroll_x = this
                                .scroll_x
                                .max(x - bounds.size.width + px(1.))
                                .min(x)
                                .max(px(0.));
                        })
                    },
                    move |bounds, (), window, cx| {
                        let style = MessageStyle::resolve(
                            &paint.read(cx).shared.get(),
                            active_palette(&paint.read(cx).shared, window),
                        );
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
                                        if !this.editing.selection.is_empty() {
                                            window.paint_quad(fill(
                                                Bounds::new(
                                                    this.caret(this.editing.selection.start),
                                                    size(
                                                        line.x_for_index(
                                                            this.editing.selection.end,
                                                        ) - line.x_for_index(
                                                            this.editing.selection.start,
                                                        ),
                                                        line_height,
                                                    ),
                                                ),
                                                rgb(style.palette.surface3),
                                            ));
                                        }
                                        let _ = line.paint(
                                            bounds.origin - point(this.scroll_x, px(0.)),
                                            line_height,
                                            gpui::TextAlign::Left,
                                            None,
                                            window,
                                            cx,
                                        );
                                        if let Some(mark) = &this.editing.marked {
                                            window.paint_quad(fill(
                                                Bounds::new(
                                                    this.caret(mark.start)
                                                        + point(px(0.), line_height - px(1.)),
                                                    size(
                                                        line.x_for_index(mark.end)
                                                            - line.x_for_index(mark.start),
                                                        px(1.),
                                                    ),
                                                ),
                                                rgb(style.palette.foreground),
                                            ));
                                        }
                                        if this.focus.is_focused(window) {
                                            window.paint_quad(fill(
                                                Bounds::new(
                                                    this.caret(this.editing.cursor()),
                                                    size(px(1.), line_height),
                                                ),
                                                rgb(style.palette.foreground),
                                            ));
                                        }
                                    }
                                },
                            );
                        })
                    },
                )
                .w_full()
                .h(line_height),
            )
    }
}

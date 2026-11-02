use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, Context, ElementInputHandler, EntityInputHandler, EventEmitter,
    FocusHandle, Focusable, MouseButton, Pixels, Point, Render, ShapedLine, Subscription, TextRun,
    UTF16Selection, Window, canvas, div, fill, font, point, prelude::*, px, rgb, rgba, size,
};
use paseo_desktop::{appearance, editing::Editing};

use crate::{
    settings_view::{SharedSettings, system_dark},
    theme::font_size,
};

pub enum FieldEvent {
    /// The draft changed while editing.
    Edited,
    /// The user pressed Enter or the field lost focus.
    Committed,
}

/// A single-line settings text field with the authored input presentation:
/// surface fill, 1px border, 6px radius, 36px minimum height. Numeric fields
/// strip every non-digit keystroke like the app's number inputs.
pub struct TextField {
    editing: Editing,
    placeholder: String,
    numeric: bool,
    width: f32,
    height: f32,
    fill_width: bool,
    align_right: bool,
    shared: SharedSettings,
    focus: FocusHandle,
    bounds: Bounds<Pixels>,
    line: Option<ShapedLine>,
    scroll_x: Pixels,
    selecting: bool,
    _blur: Option<Subscription>,
}

impl TextField {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        value: &str,
        placeholder: &str,
        numeric: bool,
        width: f32,
        align_right: bool,
        shared: SharedSettings,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            editing: Editing {
                text: value.to_owned(),
                ..Editing::default()
            },
            placeholder: placeholder.to_owned(),
            numeric,
            width,
            height: 36.,
            fill_width: false,
            align_right,
            shared,
            focus: cx.focus_handle(),
            bounds: Bounds::default(),
            line: None,
            scroll_x: px(0.),
            selecting: false,
            _blur: None,
        }
    }

    pub fn form(mut self) -> Self {
        self.height = crate::profile_settings_presentation::FIELD_HEIGHT;
        self.fill_width = true;
        self.editing.select(self.editing.text.len(), false);
        self
    }
    /// Repaint the input chrome after the active theme or interface size changed.
    pub fn settings_changed(&mut self, cx: &mut Context<Self>) {
        cx.notify();
    }

    /// Commit pending drafts when the field loses focus, like the app's
    /// commit-on-blur inputs. Idempotent; call once after construction.
    pub fn arm_commit_on_blur(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self._blur.is_none() {
            self._blur = Some(cx.on_blur(&self.focus, window, |_, _, cx| {
                cx.emit(FieldEvent::Committed);
            }));
        }
    }

    pub fn text(&self) -> &str {
        &self.editing.text
    }

    /// Replace the visible draft without marking the field dirty.
    pub fn set_text(&mut self, value: &str, cx: &mut Context<Self>) {
        if self.editing.text != value {
            self.editing.replace_all(value);
            cx.notify();
        }
    }

    fn edited(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.numeric {
            let digits: String = self
                .editing
                .text
                .chars()
                .filter(|ch| ch.is_ascii_digit())
                .collect();
            if digits != self.editing.text {
                self.editing.replace_all(&digits);
            }
        }
        window.notify_text_input_changed(&self.focus);
        cx.emit(FieldEvent::Edited);
        cx.notify();
    }

    fn key_down(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
                    if let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) {
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
                "enter" => cx.emit(FieldEvent::Committed),
                "up" | "down" | "escape" | "tab" => return,
                "backspace" | "delete" => self.editing.delete(key.key == "delete"),
                "left" | "right" => self
                    .editing
                    .horizontal(key.key == "right", key.modifiers.shift),
                "home" | "end" => self.editing.select(
                    if key.key == "home" {
                        0
                    } else {
                        self.editing.text.len()
                    },
                    key.modifiers.shift,
                ),
                _ => return,
            }
            self.edited(window, cx);
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

    fn pressed(
        &mut self,
        event: &gpui::MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        self.focus.focus(window, cx);
        self.selecting = true;
        let index = self.line.as_ref().map_or(0, |line| {
            line.closest_index_for_x(event.position.x - self.bounds.left() + self.scroll_x)
        });
        self.editing.select(index, event.modifiers.shift);
        self.edited(window, cx);
    }

    fn dragged(
        &mut self,
        event: &gpui::MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selecting {
            let index = self.line.as_ref().map_or(0, |line| {
                line.closest_index_for_x(event.position.x - self.bounds.left() + self.scroll_x)
            });
            self.editing.select(index, true);
            self.edited(window, cx);
        }
    }
}

impl EventEmitter<FieldEvent> for TextField {}
impl Focusable for TextField {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EntityInputHandler for TextField {
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
            .map(|r| self.editing.utf16_range(r))
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
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let line_height = appearance::scale_ui_font(20., self.shared.get().ui_base_font_size);
        Some(Bounds::new(
            self.caret(self.editing.byte_range(range).start),
            size(px(1.), px(line_height)),
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

impl Render for TextField {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = self.shared.get();
        let palette = appearance::resolve(settings.theme, system_dark(window));
        let font_size = appearance::scale_ui_font(font_size::BASE, settings.ui_base_font_size);
        let line_height = appearance::scale_ui_font(20., settings.ui_base_font_size);
        let ui_family = crate::fonts::ui_family_for(&settings.ui_font_family);
        let measure = cx.entity();
        let paint = cx.entity();
        let input = div()
            .id("text-field")
            .relative()
            .track_focus(&self.focus)
            .min_w(px(self.width))
            .h(px(self.height))
            .when(self.fill_width, |view| view.w_full())
            .px(px(if self.fill_width {
                crate::profile_settings_presentation::FIELD_PADDING_X
            } else {
                12.
            }))
            .flex()
            .items_center()
            .rounded(px(if self.fill_width {
                crate::profile_settings_presentation::FIELD_RADIUS
            } else {
                6.
            }))
            .border_1()
            .border_color(if self.fill_width {
                if self.focus.is_focused(window) {
                    gpui::Hsla::from(rgb(palette.border_accent))
                } else {
                    rgba(0).into()
                }
            } else {
                rgb(palette.border).into()
            })
            .when(self.fill_width, |v| {
                v.hover(|v| v.border_color(rgb(palette.border_accent)))
            })
            .bg(rgb(palette.input))
            .cursor_text()
            .on_mouse_down(MouseButton::Left, cx.listener(Self::pressed))
            .on_mouse_move(cx.listener(Self::dragged))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.selecting = false),
            )
            .on_key_down(cx.listener(Self::key_down))
            .child(
                canvas(
                    move |bounds, _, cx| {
                        measure.update(cx, |this, _| this.bounds = bounds);
                    },
                    move |bounds, (), window, cx| {
                        let ui_family = ui_family.clone();
                        paint.update(cx, |this, cx| {
                            window.handle_input(
                                &this.focus,
                                ElementInputHandler::new(bounds, paint.clone()),
                                cx,
                            );
                            let empty = this.editing.text.is_empty();
                            let text = if empty {
                                this.placeholder.clone()
                            } else {
                                this.editing.text.clone()
                            };
                            let run = TextRun {
                                len: text.len(),
                                font: font(ui_family.clone()),
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
                            let line = window.text_system().shape_line(
                                text.into(),
                                px(font_size),
                                &[run],
                                None,
                            );
                            let content_width = if this.align_right {
                                line.x_for_index(this.editing.text.len())
                            } else {
                                px(0.)
                            };
                            this.line = Some(line);
                            let offset = if this.align_right && bounds.size.width > content_width {
                                bounds.size.width - content_width - px(1.)
                            } else {
                                px(0.)
                            };
                            let cursor_x = this
                                .line
                                .as_ref()
                                .unwrap()
                                .x_for_index(this.editing.cursor());
                            // An unopened form field shows the start of its
                            // value. Merely seeding a caret at the end must not
                            // scroll long arguments before the field is focused.
                            if !this.fill_width || this.focus.is_focused(window) {
                                this.scroll_x = (cursor_x + offset - bounds.size.width + px(1.))
                                    .max(px(0.))
                                    .min((cursor_x + offset).max(px(0.)));
                            }
                            window.with_content_mask(
                                Some(gpui::ContentMask { bounds }),
                                |window| {
                                    let line = this.line.as_ref().unwrap();
                                    let origin =
                                        bounds.origin - point(this.scroll_x - offset, px(0.));
                                    if !this.editing.selection.is_empty() {
                                        window.paint_quad(fill(
                                            Bounds::new(
                                                this.caret(this.editing.selection.start),
                                                size(
                                                    line.x_for_index(this.editing.selection.end)
                                                        - line.x_for_index(
                                                            this.editing.selection.start,
                                                        ),
                                                    px(line_height),
                                                ),
                                            ),
                                            rgb(palette.surface3),
                                        ));
                                    }
                                    let _ = line.paint(
                                        origin,
                                        px(line_height),
                                        gpui::TextAlign::Left,
                                        None,
                                        window,
                                        cx,
                                    );
                                    if let Some(mark) = &this.editing.marked {
                                        window.paint_quad(fill(
                                            Bounds::new(
                                                this.caret(mark.start)
                                                    + point(px(0.), px(line_height - 1.)),
                                                size(
                                                    line.x_for_index(mark.end)
                                                        - line.x_for_index(mark.start),
                                                    px(1.),
                                                ),
                                            ),
                                            rgb(palette.foreground),
                                        ));
                                    }
                                    if this.focus.is_focused(window) {
                                        window.paint_quad(fill(
                                            Bounds::new(
                                                this.caret(this.editing.cursor()),
                                                size(px(1.), px(line_height)),
                                            ),
                                            rgb(palette.foreground),
                                        ));
                                    }
                                },
                            );
                        });
                    },
                )
                .flex_1()
                .h(px(line_height)),
            )
            .when(self.fill_width && self.focus.is_focused(window), |v| {
                v.child(
                    canvas(
                        |bounds, _, _| bounds,
                        move |bounds, _, window, _| {
                            use crate::profile_settings_presentation as p;
                            window.paint_quad(
                                fill(
                                    bounds.dilate(px(p::FOCUS_RING_WIDTH + p::FOCUS_RING_OFFSET)),
                                    rgba(0),
                                )
                                .corner_radii(px(p::FIELD_RADIUS
                                    + p::FOCUS_RING_WIDTH
                                    + p::FOCUS_RING_OFFSET))
                                .border_widths(px(p::FOCUS_RING_WIDTH))
                                .border_color(rgb(palette.accent)),
                            );
                        },
                    )
                    .absolute()
                    .inset_0(),
                )
            });
        if self.editing.text.is_empty() && !self.placeholder.is_empty() {
            input
        } else {
            input
        }
    }
}

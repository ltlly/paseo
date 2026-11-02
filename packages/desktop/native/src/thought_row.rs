//! Reasoning rows. The projected reasoning item carries only text, so the row owns a markdown
//! view and expands from the same compact badge as the desktop thought slot.
use crate::{
    message_style::MessageStyle,
    message_view::MessageView,
    settings_view::{SharedSettings, active_palette},
    theme::{radius, spacing},
};
use gpui::{Context, Entity, MouseButton, Render, SharedString, Window, div, prelude::*, px, rgb};
use serde_json::Value;

pub struct ThoughtRow {
    text: String,
    loading: bool,
    expanded: bool,
    view: Entity<MessageView>,
    last_in_sequence: bool,
    shared: SharedSettings,
}

impl ThoughtRow {
    pub fn new(item: &Value, shared: SharedSettings, cx: &mut Context<Self>) -> Self {
        let text = item["text"].as_str().unwrap_or("").to_owned();
        let loading = false;
        let view = cx.new(|cx| MessageView::new(text.clone(), false, None, shared.clone(), cx));
        cx.subscribe(
            &view,
            |_, _, event: &crate::message_view::MessageEvent, cx| match event {
                crate::message_view::MessageEvent::OpenUrl(url) => cx.open_url(url),
            },
        )
        .detach();
        Self {
            text,
            loading,
            expanded: false,
            view,
            last_in_sequence: false,
            shared,
        }
    }
    pub fn set(&mut self, item: &Value, cx: &mut Context<Self>) {
        let text = item["text"].as_str().unwrap_or("").to_owned();
        if self.text != text {
            self.text = text.clone();
            self.view
                .update(cx, |view, cx| view.set_text(&text, false, cx));
            cx.notify();
        }
    }
    /// Reasoning items carry no status; the running turn owns the live label.
    pub fn set_loading(&mut self, loading: bool, cx: &mut Context<Self>) {
        if self.loading != loading {
            self.loading = loading;
            cx.notify();
        }
    }
    pub fn set_last_in_sequence(&mut self, last: bool) {
        self.last_in_sequence = last;
    }
}

impl Render for ThoughtRow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let style = MessageStyle::resolve(&self.shared.get(), active_palette(&self.shared, window));
        let group: SharedString = format!("thought-{}", cx.entity_id()).into();
        let expanded = self.expanded;
        div()
            .mx(px(-13.))
            .mb(px(if self.last_in_sequence {
                spacing::S_4
            } else {
                spacing::S_1
            }))
            .font_family(style.ui_family.clone())
            .child(
                div()
                    .id(("thought-badge", cx.entity_id()))
                    .w_full()
                    .min_w_0()
                    .rounded(px(radius::LG))
                    .border_1()
                    .border_color(rgb(if expanded {
                        style.palette.border
                    } else {
                        style.palette.surface0
                    }))
                    .px_2()
                    .py_1()
                    .cursor_pointer()
                    .group(group)
                    .when(expanded, |badge| {
                        badge
                            .bg(rgb(style.palette.surface1))
                            .rounded_bl(px(0.))
                            .rounded_br(px(0.))
                    })
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .items_center()
                            .child(
                                div()
                                    .size(px(22.))
                                    .mr_1()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(if expanded {
                                        crate::chrome::icon("chevron-down", 15.6)
                                            .ml(px(-4.))
                                            .text_color(rgb(style.palette.foreground))
                                            .into_any_element()
                                    } else {
                                        crate::chrome::icon("brain", 12.)
                                            .ml(px(-1.))
                                            .text_color(rgb(style.palette.muted_foreground))
                                            .into_any_element()
                                    }),
                            )
                            .child(
                                div()
                                    .text_color(rgb(if expanded {
                                        style.palette.foreground
                                    } else {
                                        style.palette.foreground_muted
                                    }))
                                    .child(if self.loading { "Thinking" } else { "Thought" }),
                            )
                            .child(div().flex_1()),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.expanded = !this.expanded;
                            cx.notify();
                        }),
                    ),
            )
            .when(expanded, |row| {
                row.child(
                    div()
                        .w_full()
                        .min_w_0()
                        .border_1()
                        .border_t_0()
                        .border_color(rgb(style.palette.border))
                        .rounded_bl(px(radius::LG))
                        .rounded_br(px(radius::LG))
                        .bg(rgb(style.palette.surface1))
                        .id(("thought-body", cx.entity_id()))
                        .p_3()
                        .max_h(px(400.))
                        .overflow_y_scroll()
                        .child(self.view.clone()),
                )
            })
    }
}

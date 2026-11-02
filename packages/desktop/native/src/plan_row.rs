//! Plan rows render the daemon plan detail as the desktop plan card: expandable markdown under a
//! chevron, with a title that reflects the approval outcome.
use crate::{
    message_style::MessageStyle,
    message_view::MessageView,
    settings_view::{SharedSettings, active_palette},
    theme::{font_size, radius},
};
use gpui::{Context, Entity, MouseButton, Render, Window, div, prelude::*, px, rgb};
use serde_json::Value;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Pending,
    Approved,
    Rejected,
    Canceled,
}

impl Outcome {
    fn resolve(item: &Value) -> Self {
        match item["status"].as_str() {
            Some("canceled") => return Self::Canceled,
            _ => {}
        }
        match item["metadata"]["approved"].as_bool() {
            Some(false) => Self::Rejected,
            Some(true) => Self::Approved,
            None => match item["status"].as_str() {
                Some("running" | "executing") => Self::Pending,
                _ => Self::Pending,
            },
        }
    }
    fn title(self) -> &'static str {
        match self {
            Self::Pending => "Plan",
            Self::Approved => "Approved plan",
            Self::Rejected => "Rejected plan",
            Self::Canceled => "Canceled plan",
        }
    }
}

pub struct PlanRow {
    outcome: Outcome,
    text: String,
    expanded: bool,
    view: Entity<MessageView>,
    last_in_sequence: bool,
    shared: SharedSettings,
}

impl PlanRow {
    pub fn new(item: &Value, shared: SharedSettings, cx: &mut Context<Self>) -> Self {
        let text = item["detail"]["text"].as_str().unwrap_or("").to_owned();
        let outcome = Outcome::resolve(item);
        let view = cx.new(|cx| MessageView::new(text.clone(), false, None, shared.clone(), cx));
        cx.subscribe(
            &view,
            |_, _, event: &crate::message_view::MessageEvent, cx| match event {
                crate::message_view::MessageEvent::OpenUrl(url) => cx.open_url(url),
            },
        )
        .detach();
        Self {
            outcome,
            expanded: !matches!(outcome, Outcome::Rejected | Outcome::Canceled),
            text,
            view,
            last_in_sequence: false,
            shared,
        }
    }
    pub fn set(&mut self, item: &Value, cx: &mut Context<Self>) {
        let text = item["detail"]["text"].as_str().unwrap_or("").to_owned();
        let outcome = Outcome::resolve(item);
        let mut changed = false;
        if self.text != text {
            self.text = text.clone();
            self.view
                .update(cx, |view, cx| view.set_text(&text, false, cx));
            changed = true;
        }
        if self.outcome != outcome {
            self.outcome = outcome;
            self.expanded = !matches!(outcome, Outcome::Rejected | Outcome::Canceled);
            changed = true;
        }
        if changed {
            cx.notify();
        }
    }
    pub fn set_last_in_sequence(&mut self, last: bool) {
        self.last_in_sequence = last;
    }
}

impl Render for PlanRow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let style = MessageStyle::resolve(&self.shared.get(), active_palette(&self.shared, window));
        let expanded = self.expanded;
        div()
            .my_3()
            .when(self.last_in_sequence, |card| card.mb_4())
            .w_full()
            .min_w_0()
            .p_3()
            .rounded(px(radius::LG))
            .border_1()
            .border_color(rgb(style.palette.border))
            .bg(rgb(style.palette.surface1))
            .font_family(style.ui_family.clone())
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .id(("plan-header", cx.entity_id()))
                    .min_h(px(24.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .cursor_pointer()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(if expanded {
                                crate::chrome::icon("chevron-down", 16.)
                                    .text_color(rgb(style.palette.foreground_muted))
                                    .into_any_element()
                            } else {
                                crate::chrome::icon("chevron-right", 16.)
                                    .text_color(rgb(style.palette.foreground_muted))
                                    .into_any_element()
                            }),
                    )
                    .child(
                        div()
                            .flex_shrink_1()
                            .text_size(px(font_size::BASE))
                            .line_height(px(22.))
                            .text_color(rgb(style.palette.foreground))
                            .child(self.outcome.title()),
                    )
                    .child(div().flex_1())
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.expanded = !this.expanded;
                            cx.notify();
                        }),
                    ),
            )
            .when(expanded, |card| {
                card.child(
                    div()
                        .id("plan-body")
                        .max_h(px(400.))
                        .overflow_y_scroll()
                        .child(self.view.clone()),
                )
            })
    }
}

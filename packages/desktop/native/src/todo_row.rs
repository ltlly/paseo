//! Todo rows. Each projected todo item reports the activity it represents, derived by the
//! presentation layer from the previous todo snapshot.
use crate::{
    message_style::MessageStyle,
    settings_view::{SharedSettings, active_palette},
    theme::{radius, spacing},
    timeline::{Activity, Task},
};
use gpui::{
    Context, MouseButton, Render, SharedString, StyledText, Window, div, prelude::*, px, rgb,
};
use serde_json::Value;

pub struct TodoRow {
    activity: Option<Activity>,
    tasks: Vec<Task>,
    expanded: bool,
    shared: SharedSettings,
}

impl TodoRow {
    pub fn new(
        item: &Value,
        activity: Option<Activity>,
        shared: SharedSettings,
        _cx: &mut Context<Self>,
    ) -> Self {
        Self {
            activity,
            tasks: crate::timeline::tasks(item),
            expanded: false,
            shared,
        }
    }
    pub fn set_last_in_sequence(&mut self, _last: bool) {}
    pub fn set(&mut self, item: &Value, activity: Option<Activity>, cx: &mut Context<Self>) {
        let tasks = crate::timeline::tasks(item);
        if self.activity == activity && self.tasks == tasks {
            return;
        }
        self.activity = activity;
        self.tasks = tasks;
        cx.notify();
    }
    fn label(&self) -> (String, Option<String>) {
        match &self.activity {
            Some(Activity::Created(count)) => (format!("Created {count} tasks"), None),
            Some(Activity::Added(task)) => ("Added".into(), Some(task.clone())),
            Some(Activity::Started(task)) => ("Started".into(), Some(task.clone())),
            Some(Activity::Completed(task)) => ("Completed".into(), Some(task.clone())),
            None => ("Tasks".into(), None),
        }
    }
    fn icon(&self) -> &'static str {
        match self.activity {
            Some(Activity::Created(_)) => "check-square",
            Some(Activity::Added(_)) => "plus",
            Some(Activity::Started(_)) => "circle-dot",
            Some(Activity::Completed(_)) => "check",
            None => "check-square",
        }
    }
}

impl Render for TodoRow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let style = MessageStyle::resolve(&self.shared.get(), active_palette(&self.shared, window));
        let group: SharedString = format!("todo-{}", cx.entity_id()).into();
        let expanded = self.expanded;
        let (label, secondary) = self.label();
        let icon = self.icon();
        let active = expanded;
        div()
            .mx(px(-13.))
            .mb(px(spacing::S_1))
            .font_family(style.ui_family.clone())
            .child(
                div()
                    .id(("todo-badge", cx.entity_id()))
                    .w_full()
                    .min_w_0()
                    .rounded(px(radius::LG))
                    .border_1()
                    .border_color(rgb(style.palette.border))
                    .px_2()
                    .py_1()
                    .cursor_pointer()
                    .when(expanded, |badge| {
                        badge
                            .bg(rgb(style.palette.surface1))
                            .rounded_bl(px(0.))
                            .rounded_br(px(0.))
                    })
                    .when(!expanded, |badge| {
                        badge.border_color(rgb(style.palette.surface0))
                    })
                    .group(group)
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
                                    .child(crate::chrome::icon(icon, 12.).ml(px(-1.)).text_color(
                                        rgb(if active {
                                            style.palette.foreground
                                        } else {
                                            style.palette.muted_foreground
                                        }),
                                    )),
                            )
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_color(rgb(if active {
                                        style.palette.foreground
                                    } else {
                                        style.palette.foreground_muted
                                    }))
                                    .child(label),
                            )
                            .when_some(secondary, |row, secondary| {
                                row.child(
                                    div()
                                        .ml_2()
                                        .min_w_0()
                                        .truncate()
                                        .text_color(rgb(if active {
                                            style.palette.foreground
                                        } else {
                                            style.palette.foreground_muted
                                        }))
                                        .child(secondary),
                                )
                            })
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
                        .p_2()
                        .child(div().flex().flex_col().gap_1().children(
                            if self.tasks.is_empty() {
                                vec![
                                    div()
                                        .text_color(rgb(style.palette.foreground_muted))
                                        .child("No tasks yet.")
                                        .into_any_element(),
                                ]
                            } else {
                                self.tasks
                                    .iter()
                                    .map(|task| task_row(task, &style))
                                    .collect::<Vec<_>>()
                            },
                        )),
                )
            })
    }
}

fn task_text(task: &Task, style: &MessageStyle) -> StyledText {
    let label = task.label().to_owned();
    let mut run = gpui::TextRun {
        len: label.len(),
        font: gpui::font(style.ui_family.clone()),
        color: rgb(if task.is_complete() {
            style.palette.foreground_extra_muted
        } else if task.is_running() {
            style.palette.foreground
        } else {
            style.palette.foreground_muted
        })
        .into(),
        background_color: None,
        underline: None,
        strikethrough: task.is_complete().then_some(gpui::StrikethroughStyle {
            thickness: px(1.),
            color: None,
        }),
    };
    run.len = label.len();
    StyledText::new(SharedString::from(label)).with_runs(vec![run])
}

fn task_row(task: &Task, style: &MessageStyle) -> gpui::AnyElement {
    let (icon, color) = if task.is_complete() {
        ("circle-check", style.palette.foreground_extra_muted)
    } else if task.is_running() {
        ("circle-dot", style.palette.status_dot_running)
    } else {
        ("circle", style.palette.foreground_extra_muted)
    };
    div()
        .w_full()
        .min_w_0()
        .flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .flex_shrink_0()
                .child(crate::chrome::icon(icon, 16.).text_color(rgb(color))),
        )
        .child(
            div()
                .min_w_0()
                .flex_1()
                .text_color(rgb(if task.is_complete() {
                    style.palette.foreground_extra_muted
                } else if task.is_running() {
                    style.palette.foreground
                } else {
                    style.palette.foreground_muted
                }))
                .child(task_text(task, style)),
        )
        .into_any_element()
}

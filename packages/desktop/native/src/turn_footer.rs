//! Turn footer: the running indicator or the completed turn's copy action, duration and end time.
use crate::{
    chrome::loading_spinner,
    message_style::MessageStyle,
    settings_view::{SharedSettings, active_palette},
    timeline::{self, Footer},
};
use gpui::{
    ClipboardItem, Context, MouseButton, Render, SharedString, Window, div, prelude::*, px, rgb,
};
use std::time::Duration;

pub struct TurnFooter {
    footer: Option<Footer>,
    running: bool,
    started_at: Option<String>,
    ticking: bool,
    copied: bool,
    copy_revision: u64,
    shared: SharedSettings,
}

impl TurnFooter {
    pub fn new(shared: SharedSettings, _cx: &mut Context<Self>) -> Self {
        Self {
            footer: None,
            running: false,
            started_at: None,
            ticking: false,
            copied: false,
            copy_revision: 0,
            shared,
        }
    }
    pub fn set(
        &mut self,
        footer: Option<Footer>,
        running: bool,
        started_at: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.footer == footer && self.running == running && self.started_at == started_at {
            return;
        }
        self.footer = footer;
        self.running = running;
        self.started_at = started_at;
        if running && !self.ticking {
            self.arm_elapsed_timer(cx);
        }
        cx.notify();
    }
    fn arm_elapsed_timer(&mut self, cx: &mut Context<Self>) {
        if self.ticking || !self.running {
            return;
        }
        self.ticking = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let running = this.update(cx, |this, cx| {
                    if this.running {
                        cx.notify();
                    }
                    this.running
                });
                if !matches!(running, Ok(true)) {
                    let _ = this.update(cx, |this, _| this.ticking = false);
                    break;
                }
            }
        })
        .detach();
    }
    fn copy(&mut self, cx: &mut Context<Self>) {
        let Some(footer) = &self.footer else {
            return;
        };
        if footer.copy_text.is_empty() {
            return;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(footer.copy_text.clone()));
        self.copied = true;
        self.copy_revision = self.copy_revision.wrapping_add(1);
        let revision = self.copy_revision;
        cx.notify();
        let timer = cx.background_executor().timer(Duration::from_millis(1500));
        cx.spawn(async move |this, cx| {
            timer.await;
            let _ = this.update(cx, |this, cx| {
                if this.copy_revision == revision {
                    this.copied = false;
                    cx.notify();
                }
            });
        })
        .detach();
    }
}

impl Render for TurnFooter {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let style = MessageStyle::resolve(&self.shared.get(), active_palette(&self.shared, window));
        let group: SharedString = format!("turn-footer-{}", cx.entity_id()).into();
        let mut row = div()
            .mt(px(13.))
            .pb(px(32.))
            .min_h(px(24.))
            .font_family(style.ui_family.clone())
            .flex()
            .items_center()
            .gap_2();
        if self.running {
            let elapsed = self
                .started_at
                .as_deref()
                .and_then(|start| {
                    let start = timeline::parse_time(Some(start))?;
                    u64::try_from((timeline::now() - start).num_milliseconds().max(0)).ok()
                })
                .map(|ms| timeline::duration_label(Some(ms)))
                .unwrap_or_default();
            return row
                .child(
                    div()
                        .ml(px(-2.))
                        .child(loading_spinner(14., style.palette.foreground)),
                )
                .when(!elapsed.is_empty(), |row| {
                    row.child(
                        div()
                            .text_size(px(13.))
                            .text_color(rgb(style.palette.foreground_muted))
                            .child(elapsed),
                    )
                });
        }
        let Some(footer) = self.footer.clone() else {
            return row;
        };
        let duration = if footer.duration_ms.is_some() {
            format!(
                "Worked for {}",
                timeline::duration_label(footer.duration_ms)
            )
        } else {
            String::new()
        };
        let timestamp = footer
            .completed_at
            .as_deref()
            .map(|time| timeline::timestamp_label(time, timeline::now()))
            .unwrap_or_default();
        let primary = if duration.is_empty() {
            timestamp.clone()
        } else {
            duration.clone()
        };
        let can_swap = !duration.is_empty() && !timestamp.is_empty() && duration != timestamp;
        let sizer = if primary.len() >= timestamp.len() {
            primary.clone()
        } else {
            timestamp.clone()
        };
        row = row.child(
            div()
                .id(("copy-turn", cx.entity_id()))
                .mt_2()
                .mr(px(-4.))
                .p_2()
                .pt_0()
                .cursor_pointer()
                .text_color(rgb(style.palette.foreground_muted))
                .hover(|v| v.text_color(rgb(style.palette.foreground)))
                .child(crate::chrome::icon(
                    if self.copied { "check" } else { "copy" },
                    14.,
                ))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.copy(cx);
                    }),
                ),
        );
        row.child(
            div()
                .relative()
                .text_size(px(13.))
                .text_color(rgb(style.palette.foreground_muted))
                .when(can_swap, |label| label.group(group.clone()))
                .child(div().invisible().child(sizer))
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .when(can_swap, |text| {
                            text.group_hover(group.clone(), |v| v.invisible())
                        })
                        .child(primary),
                )
                .when(can_swap, |label| {
                    label.child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .invisible()
                            .group_hover(group, |v| v.visible())
                            .child(timestamp),
                    )
                }),
        )
    }
}

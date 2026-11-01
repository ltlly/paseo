use anyhow::{Result, ensure};
use gpui::{Context, Entity, MouseButton, Render, Window, div, prelude::*, px, rgb};
use paseo_desktop::{
    client::Connection,
    history::{History, Page},
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};

pub struct Conversation {
    agent_id: String,
    connection: Arc<Connection>,
    history: History,
    pending: Option<String>,
    dirty: bool,
    error: Option<String>,
    refresh_scheduled: bool,
    whole_turns: bool,
    composer: Entity<crate::composer_view::Composer>,
    messages: BTreeMap<u64, Entity<crate::message_view::MessageView>>,
}

impl Conversation {
    pub fn new(
        agent_id: String,
        connection: Arc<Connection>,
        whole_turns: bool,
        composer: Entity<crate::composer_view::Composer>,
    ) -> Self {
        let mut view = Self {
            agent_id,
            connection,
            history: History::default(),
            pending: None,
            dirty: false,
            error: None,
            refresh_scheduled: false,
            whole_turns,
            composer,
            messages: BTreeMap::new(),
        };
        if let Err(error) = view.fetch("tail") {
            view.error = Some(error.to_string());
        }
        view
    }
    fn fetch(&mut self, direction: &str) -> Result<()> {
        if self.pending.is_some() {
            return Ok(());
        }
        let mut fields = json!({"agentId":self.agent_id,"direction":direction,"projection":"projected","limit":40});
        if self.whole_turns && direction != "after" {
            fields["wholeTurns"] = true.into();
        }
        let cursor = match direction {
            "before" => self.history.start.as_ref(),
            "after" => self.history.end.as_ref(),
            _ => None,
        };
        if let Some(cursor) = cursor {
            fields["cursor"] = json!({"epoch":cursor.epoch,"seq":cursor.seq});
        }
        self.pending = Some(
            self.connection
                .request("fetch_agent_timeline_request", fields)?,
        );
        self.dirty = false;
        Ok(())
    }
    pub fn receive(&mut self, kind: &str, payload: &Value, cx: &mut Context<Self>) -> Result<()> {
        if kind == "rpc_error" && payload["requestId"].as_str() == self.pending.as_deref() {
            self.pending = None;
            self.error = Some(payload["error"].to_string());
            cx.notify();
            return Ok(());
        }
        if payload["agentId"].as_str() != Some(&self.agent_id) {
            return Ok(());
        }
        match kind {
            "fetch_agent_timeline_response" => {
                if payload["requestId"].as_str() != self.pending.as_deref() {
                    return Ok(());
                }
                self.pending = None;
                let page: Page = serde_json::from_value(payload.clone())?;
                ensure!(
                    page.agent_id == self.agent_id,
                    "timeline belongs to another agent"
                );
                let previous = self.history.end.clone();
                let reset_views = page.reset || self.history.epoch.as_deref() != Some(&page.epoch);
                self.history.apply(&page)?;
                if reset_views {
                    self.messages.clear();
                }
                self.sync_messages(cx);
                if page.has_newer {
                    ensure!(
                        page.end_cursor.is_some() && page.end_cursor != previous,
                        "timeline catch-up cursor did not advance"
                    );
                }
                if page.has_newer {
                    self.fetch("after")?;
                } else if self.dirty {
                    self.schedule_refresh(cx);
                }
            }
            "agent_stream" | "agent.timeline.replacement" => {
                self.dirty = true;
                self.schedule_refresh(cx);
            }
            _ => return Ok(()),
        }
        self.error = None;
        cx.notify();
        Ok(())
    }

    fn sync_messages(&mut self, cx: &mut Context<Self>) {
        self.messages.retain(|id, _| {
            self.history.entries.get(id).is_some_and(|entry| {
                matches!(
                    entry.item["type"].as_str(),
                    Some("user_message" | "assistant_message")
                )
            })
        });
        for (id, entry) in &self.history.entries {
            let user = entry.item["type"] == "user_message";
            if !user && entry.item["type"] != "assistant_message" {
                continue;
            }
            let source = entry.item["text"].as_str().unwrap_or("");
            if let Some(view) = self.messages.get(id) {
                view.update(cx, |view, cx| view.set_text(source, user, cx));
            } else {
                let view =
                    cx.new(|cx| crate::message_view::MessageView::new(source.to_owned(), user, cx));
                cx.subscribe(
                    &view,
                    |_, _, event: &crate::message_view::MessageEvent, cx| match event {
                        crate::message_view::MessageEvent::OpenUrl(url) => cx.open_url(url),
                    },
                )
                .detach();
                self.messages.insert(*id, view);
            }
        }
    }

    fn schedule_refresh(&mut self, cx: &mut Context<Self>) {
        if self.refresh_scheduled {
            return;
        }
        self.refresh_scheduled = true;
        let timer = cx
            .background_executor()
            .timer(std::time::Duration::from_millis(50));
        cx.spawn(async move |this, cx| {
            timer.await;
            let _ = this.update(cx, |this, cx| {
                this.refresh_scheduled = false;
                if this.dirty && this.pending.is_none() {
                    if let Err(error) = this.fetch("after") {
                        this.error = Some(error.to_string());
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }
}

impl Render for Conversation {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use crate::theme::{colors as c, font_size};
        let entries = self.history.entries.values().collect::<Vec<_>>();
        div()
            .id("agent-conversation")
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .id("conversation-history")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .p_4()
                    // The shared desktop viewport reserves a history-start slot even when
                    // history is empty. Omitting it puts the empty state 16px too high.
                    .child(
                        div()
                            .h(px(32.))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .when(self.history.has_older, |slot| {
                                slot.child(
                                    div()
                                        .id("older-messages")
                                        .cursor_pointer()
                                        .child("Load earlier messages")
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _, _, cx| {
                                                if let Err(error) = this.fetch("before") {
                                                    this.error = Some(error.to_string());
                                                }
                                                cx.notify();
                                            }),
                                        ),
                                )
                            }),
                    )
                    .when_some(self.error.clone(), |view, error| {
                        view.child(div().text_color(rgb(c::STATUS_DANGER)).child(error))
                    })
                    .when(self.pending.is_some(), |view| {
                        view.child(
                            div()
                                .py_2()
                                .text_color(rgb(c::FOREGROUND_MUTED))
                                .child("Loading messages…"),
                        )
                    })
                    .when(
                        self.history.entries.is_empty() && self.pending.is_none(),
                        |view| {
                            view.child(
                                div()
                                    .w_full()
                                    .flex_1()
                                    .py_12()
                                    .line_height(px(16.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_color(rgb(c::FOREGROUND_MUTED))
                                    .child("Start chatting with this agent..."),
                            )
                        },
                    )
                    .children(entries.iter().enumerate().map(|(index, entry)| {
                        let kind = entry.item["type"].as_str().unwrap_or("message");
                        let next = entries
                            .get(index + 1)
                            .copied()
                            .and_then(|e| e.item["type"].as_str());
                        let gap = match (kind, next) {
                            (_, None) => 0.,
                            ("user_message", Some("user_message")) => 4.,
                            ("user_message", Some("assistant_message")) => 0.,
                            ("assistant_message", Some("tool_call" | "reasoning" | "todo"))
                            | ("tool_call" | "reasoning" | "todo", Some("assistant_message")) => 4.,
                            (
                                "tool_call" | "reasoning" | "todo",
                                Some("tool_call" | "reasoning" | "todo"),
                            ) => 0.,
                            _ => 16.,
                        };
                        let row = div()
                            .id(("timeline-row", entry.seq_start))
                            .flex_shrink_0()
                            .w_full()
                            .flex()
                            .justify_center()
                            .mb(px(gap));
                        let content = div()
                            .w_full()
                            .max_w(px(crate::theme::CONTENT_MAX_WIDTH))
                            .px_2();
                        if let Some(message) = self.messages.get(&entry.seq_start) {
                            return row.child(content.child(message.clone())).into_any_element();
                        }
                        let text = entry.item["text"]
                            .as_str()
                            .or_else(|| entry.item["message"].as_str())
                            .map(str::to_owned)
                            .unwrap_or_else(|| {
                                serde_json::to_string_pretty(&entry.item).unwrap_or_default()
                            });
                        row.child(
                            content
                                .child(
                                    div()
                                        .py_2()
                                        .text_color(rgb(c::FOREGROUND_MUTED))
                                        .child(kind.replace('_', " ")),
                                )
                                .child(div().text_size(px(font_size::CONTENT)).child(text)),
                        )
                        .into_any_element()
                    })),
            )
            .child(self.composer.clone())
    }
}

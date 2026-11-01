use anyhow::{Result, ensure};
use gpui::{Context, MouseButton, Render, Window, div, prelude::*, rgb};
use paseo_desktop::{
    client::Connection,
    history::{History, Page},
};
use serde_json::{Value, json};
use std::sync::Arc;

pub struct Conversation {
    agent_id: String,
    connection: Arc<Connection>,
    history: History,
    pending: Option<String>,
    dirty: bool,
    error: Option<String>,
    refresh_scheduled: bool,
    whole_turns: bool,
}

impl Conversation {
    pub fn new(agent_id: String, connection: Arc<Connection>, whole_turns: bool) -> Self {
        let mut view = Self {
            agent_id,
            connection,
            history: History::default(),
            pending: None,
            dirty: false,
            error: None,
            refresh_scheduled: false,
            whole_turns,
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
                self.history.apply(&page)?;
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
        div()
            .id("agent-conversation")
            .size_full()
            .overflow_y_scroll()
            .p_4()
            .child(
                div()
                    .id("refresh-messages")
                    .py_2()
                    .cursor_pointer()
                    .child("Refresh messages")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            if let Err(error) = this.fetch("tail") {
                                this.error = Some(error.to_string());
                            }
                            cx.notify();
                        }),
                    ),
            )
            .when_some(self.error.clone(), |view, error| {
                view.child(div().text_color(rgb(0xf07178)).child(error))
            })
            .when(self.history.has_older, |view| {
                view.child(
                    div()
                        .id("older-messages")
                        .py_2()
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
            })
            .when(self.pending.is_some(), |view| {
                view.child(
                    div()
                        .py_2()
                        .text_color(rgb(0x808989))
                        .child("Loading messages…"),
                )
            })
            .children(self.history.entries.values().map(|entry| {
                let kind = entry.item["type"].as_str().unwrap_or("message").to_owned();
                let text = entry.item["text"]
                    .as_str()
                    .or_else(|| entry.item["message"].as_str())
                    .map(str::to_owned)
                    .unwrap_or_else(|| {
                        serde_json::to_string_pretty(&entry.item).unwrap_or_default()
                    });
                div()
                    .id(("timeline-row", entry.seq_start))
                    .mb_4()
                    .w_full()
                    .child(
                        div()
                            .text_xs()
                            .mb_1()
                            .text_color(rgb(0x808989))
                            .child(kind.replace('_', " ")),
                    )
                    .child(div().w_full().child(text))
            }))
    }
}

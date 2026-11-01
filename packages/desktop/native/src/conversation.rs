use anyhow::{Result, ensure};
use gpui::{Context, Entity, MouseButton, Render, Window, div, prelude::*, px, rgb};
use paseo_desktop::{
    client::Connection,
    history::{Cursor, History, LiveBuffer, LiveChange, Page, SyncNeed},
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc, time::Duration};

#[derive(Clone, Copy, PartialEq)]
enum Query {
    Tail,
    Before,
    After,
}
impl Query {
    fn name(self) -> &'static str {
        match self {
            Self::Tail => "tail",
            Self::Before => "before",
            Self::After => "after",
        }
    }
}
struct Pending {
    id: String,
    query: Query,
    from: Option<Cursor>,
    generation: u64,
}

pub struct Conversation {
    agent_id: String,
    connection: Arc<Connection>,
    history: History,
    buffer: LiveBuffer,
    pending: Option<Pending>,
    dirty: bool,
    want_tail: bool,
    error: Option<String>,
    refresh_scheduled: bool,
    schedule_generation: u64,
    retry_ms: u64,
    retry_query: Query,
    whole_turns: bool,
    composer: Entity<crate::composer_view::Composer>,
    messages: BTreeMap<u64, Entity<crate::message_view::MessageView>>,
}
impl Conversation {
    pub fn owns_request(&self, payload: &Value) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|pending| payload["requestId"].as_str() == Some(pending.id.as_str()))
    }
    pub fn new(
        agent_id: String,
        connection: Arc<Connection>,
        whole_turns: bool,
        composer: Entity<crate::composer_view::Composer>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self {
            agent_id,
            connection,
            history: History::default(),
            buffer: LiveBuffer::default(),
            pending: None,
            dirty: false,
            want_tail: false,
            error: None,
            refresh_scheduled: false,
            schedule_generation: 0,
            retry_ms: 1000,
            retry_query: Query::After,
            whole_turns,
            composer,
            messages: BTreeMap::new(),
        };
        if let Err(error) = view.fetch(Query::Tail, cx) {
            view.failed(error.to_string(), Some(Query::Tail), cx);
        }
        view
    }
    fn fetch(&mut self, query: Query, cx: &mut Context<Self>) -> Result<()> {
        if self.pending.is_some() {
            return Ok(());
        }
        let query = if self.want_tail || (query == Query::After && self.history.cursor().is_none())
        {
            Query::Tail
        } else {
            query
        };
        let from = match query {
            Query::Before => self.history.start.clone(),
            Query::After => self.history.cursor(),
            Query::Tail => None,
        };
        let mut fields = json!({"agentId":self.agent_id,"direction":query.name(),"projection":"projected","limit":40});
        // The daemon also uses wholeTurns if an after-query resets to a replacement tail.
        if self.whole_turns {
            fields["wholeTurns"] = true.into();
        }
        if let Some(cursor) = &from {
            fields["cursor"] = json!({"epoch":cursor.epoch,"seq":cursor.seq});
        }
        let id = self
            .connection
            .request("fetch_agent_timeline_request", fields)?;
        self.pending = Some(Pending {
            id: id.clone(),
            query,
            from,
            generation: self.buffer.generation(),
        });
        self.dirty = false;
        self.want_tail = false;
        let timer = cx.background_executor().timer(Duration::from_secs(30));
        cx.spawn(async move |this, cx| {
            timer.await;
            let _ = this.update(cx, |this, cx| {
                if this
                    .pending
                    .as_ref()
                    .is_some_and(|pending| pending.id == id)
                {
                    let pending = this.pending.take().unwrap();
                    this.failed(
                        "Timed out while updating messages. Retry to update the conversation."
                            .into(),
                        Some(pending.query),
                        cx,
                    );
                }
            });
        })
        .detach();
        Ok(())
    }
    pub fn receive(&mut self, kind: &str, payload: &Value, cx: &mut Context<Self>) -> Result<()> {
        if kind == "rpc_error" && self.owns_request(payload) {
            let pending = self.pending.take();
            self.failed(
                payload["error"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| payload["error"].to_string()),
                pending.map(|p| p.query),
                cx,
            );
            return Ok(());
        }
        if payload["agentId"].as_str() != Some(&self.agent_id) {
            return Ok(());
        }
        match kind {
            "fetch_agent_timeline_response" => {
                if !self.owns_request(payload) {
                    return Ok(());
                }
                let pending = self.pending.take().unwrap();
                let result = (|| -> Result<SyncNeed> {
                    let page: Page = serde_json::from_value(payload.clone())?;
                    ensure!(
                        page.agent_id == self.agent_id,
                        "Timeline belongs to another agent"
                    );
                    let reset = page.reset || self.history.epoch.as_deref() != Some(&page.epoch);
                    self.history.apply(&page)?;
                    if reset {
                        self.messages.clear();
                    }
                    let need = self
                        .buffer
                        .finish_snapshot(&mut self.history, pending.generation)?;
                    if need == SyncNeed::After
                        && pending.query == Query::After
                        && pending.from.as_ref().is_some_and(|from| {
                            from.epoch == page.epoch
                                && self.history.cursor().is_some_and(|end| end.seq <= from.seq)
                        })
                    {
                        anyhow::bail!(
                            "The host did not advance the conversation history. Retry to update messages."
                        );
                    }
                    Ok(need)
                })();
                self.sync_messages(cx);
                match result {
                    Err(error) => self.failed(error.to_string(), Some(pending.query), cx),
                    Ok(need) => {
                        self.error = None;
                        self.retry_ms = 1000;
                        self.dirty = need != SyncNeed::Current || self.want_tail;
                        self.want_tail |= need == SyncNeed::Tail;
                        if self.dirty {
                            self.schedule_refresh(cx);
                        }
                        cx.notify();
                    }
                }
            }
            "agent_stream" => {
                if payload["event"]["type"] != "timeline" {
                    return Ok(());
                }
                if self.pending.is_some() || self.dirty || self.want_tail {
                    self.buffer.push(payload);
                    if self.pending.is_none() {
                        self.dirty = true;
                        self.schedule_refresh(cx);
                    }
                    return Ok(());
                }
                match self.history.apply_live(payload) {
                    Ok(LiveChange::Ignored) => {}
                    Ok(LiveChange::Applied { anchor, inserted }) => {
                        if self.sync_message(anchor, cx) || inserted {
                            cx.notify();
                        }
                    }
                    Ok(LiveChange::Reconcile) => {
                        self.buffer.push(payload);
                        self.dirty = true;
                        self.want_tail |= payload["epoch"]
                            .as_str()
                            .is_some_and(|epoch| self.history.epoch.as_deref() != Some(epoch));
                        self.schedule_refresh(cx);
                    }
                    Err(error) => {
                        self.buffer.push(payload);
                        self.failed(error.to_string(), None, cx);
                    }
                }
            }
            "agent.timeline.replacement" => {
                self.want_tail = true;
                self.dirty = true;
                if self.pending.is_none() {
                    self.schedule_refresh(cx);
                }
            }
            _ => {}
        }
        Ok(())
    }
    fn sync_message(&mut self, id: u64, cx: &mut Context<Self>) -> bool {
        let Some(entry) = self.history.entries.get(&id) else {
            return false;
        };
        let user = entry.item["type"] == "user_message";
        if !user && entry.item["type"] != "assistant_message" {
            return true;
        }
        let source = entry.item["text"].as_str().unwrap_or("");
        if let Some(view) = self.messages.get(&id) {
            view.update(cx, |view, cx| view.set_text(source, user, cx));
            false
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
            self.messages.insert(id, view);
            true
        }
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
        for id in self.history.entries.keys().copied().collect::<Vec<_>>() {
            self.sync_message(id, cx);
        }
    }
    fn failed(&mut self, error: String, query: Option<Query>, cx: &mut Context<Self>) {
        self.error = Some(error);
        self.retry_query = query.unwrap_or(Query::After);
        self.dirty = true;
        self.want_tail |= query == Some(Query::Tail);
        self.schedule_generation = self.schedule_generation.saturating_add(1);
        self.refresh_scheduled = false;
        self.schedule_refresh(cx);
        self.retry_ms = (self.retry_ms * 2).min(30_000);
        cx.notify();
    }
    fn retry(&mut self, cx: &mut Context<Self>) {
        if self.pending.is_some() {
            return;
        }
        self.schedule_generation = self.schedule_generation.saturating_add(1);
        self.refresh_scheduled = false;
        self.retry_ms = 1000;
        if let Err(error) = self.fetch(self.retry_query, cx) {
            self.failed(error.to_string(), None, cx);
        }
        cx.notify();
    }
    fn schedule_refresh(&mut self, cx: &mut Context<Self>) {
        if self.refresh_scheduled {
            return;
        }
        self.refresh_scheduled = true;
        let generation = self.schedule_generation;
        let delay = if self.error.is_some() {
            self.retry_ms
        } else {
            50
        };
        let timer = cx.background_executor().timer(Duration::from_millis(delay));
        cx.spawn(async move |this, cx| {
            timer.await;
            let _ = this.update(cx, |this, cx| {
                if this.schedule_generation != generation {
                    return;
                }
                this.refresh_scheduled = false;
                if this.dirty && this.pending.is_none() {
                    let query = if this.error.is_some() {
                        this.retry_query
                    } else {
                        Query::After
                    };
                    if let Err(error) = this.fetch(query, cx) {
                        this.failed(error.to_string(), None, cx);
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
                            .when(self.pending.is_some(), |slot| {
                                slot.child("Loading messages…")
                            })
                            .when(self.pending.is_none() && self.history.has_older, |slot| {
                                slot.child(
                                    div()
                                        .id("older-messages")
                                        .cursor_pointer()
                                        .child("Load earlier messages")
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _, _, cx| {
                                                if let Err(error) = this.fetch(Query::Before, cx) {
                                                    this.failed(
                                                        error.to_string(),
                                                        Some(Query::Before),
                                                        cx,
                                                    );
                                                }
                                                cx.notify();
                                            }),
                                        ),
                                )
                            }),
                    )
                    .when_some(self.error.clone(), |view, error| {
                        view.child(
                            div()
                                .flex()
                                .flex_col()
                                .items_start()
                                .gap_2()
                                .child(div().text_color(rgb(c::STATUS_DANGER)).child(error))
                                .child(
                                    div()
                                        .id("retry-timeline")
                                        .px_2()
                                        .py_1()
                                        .rounded_md()
                                        .bg(rgb(c::SURFACE2))
                                        .child(if self.pending.is_some() {
                                            "Retrying…"
                                        } else {
                                            "Retry"
                                        })
                                        .when(self.pending.is_some(), |button| button.opacity(0.5))
                                        .when(self.pending.is_none(), |button| {
                                            button.cursor_pointer().on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(|this, _, _, cx| this.retry(cx)),
                                            )
                                        }),
                                ),
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

use crate::{
    image_view::ImageCache, message_view::MessageView, permission_board::PermissionBoard,
    plan_row::PlanRow, thought_row::ThoughtRow, timeline, todo_row::TodoRow, tool_row::ToolRow,
    turn_footer::TurnFooter,
};
use anyhow::{Result, ensure};
use gpui::{
    BoxShadow, Context, Entity, MouseButton, Render, Window, div, prelude::*, px, rgb, rgba,
};
use paseo_desktop::{
    client::Connection,
    history::{Cursor, History, LiveBuffer, LiveChange, Page, SyncNeed},
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, collections::BTreeSet, sync::Arc, time::Duration};

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
    shared: crate::settings_view::SharedSettings,
    history: History,
    buffer: LiveBuffer,
    pending: Option<Pending>,
    connected: bool,
    dirty: bool,
    want_tail: bool,
    error: Option<String>,
    refresh_scheduled: bool,
    schedule_generation: u64,
    retry_ms: u64,
    retry_query: Query,
    whole_turns: bool,
    foreground_sync: bool,
    composer: Entity<crate::composer_view::Composer>,
    rows: BTreeMap<u64, RowView>,
    display: Vec<timeline::DisplayRow>,
    expanded_turns: BTreeSet<u64>,
    active_turn: Option<String>,
    footers: BTreeMap<String, Entity<TurnFooter>>,
    permissions: Entity<PermissionBoard>,
    images: Entity<ImageCache>,
    workspace_root: Option<String>,
    pending_sent: Option<(String, Entity<MessageView>)>,
    agent_running: bool,
}

enum RowView {
    Message(Entity<MessageView>),
    Tool(Entity<ToolRow>),
    Thought(Entity<ThoughtRow>),
    Todo(Entity<TodoRow>),
    Plan(Entity<PlanRow>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowKind {
    Message,
    Tool,
    Thought,
    Todo,
    Plan,
}

impl RowView {
    fn kind(&self) -> RowKind {
        match self {
            Self::Message(_) => RowKind::Message,
            Self::Tool(_) => RowKind::Tool,
            Self::Thought(_) => RowKind::Thought,
            Self::Todo(_) => RowKind::Todo,
            Self::Plan(_) => RowKind::Plan,
        }
    }
}

/// A retained entry can be re-projected into another item type under the same seq key. Only an
/// existing message row can be updated in place; every other variant is replaced.
fn reuses_message_row(kind: Option<RowKind>) -> bool {
    matches!(kind, Some(RowKind::Message))
}

fn shows_pending_bubble(pending: bool, echo: bool) -> bool {
    pending && !echo
}

fn shows_empty_state(history_empty: bool, pending_request: bool, pending_bubble: bool) -> bool {
    history_empty && !pending_request && !pending_bubble
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
        shared: crate::settings_view::SharedSettings,
        whole_turns: bool,
        composer: Entity<crate::composer_view::Composer>,
        cx: &mut Context<Self>,
    ) -> Self {
        let view_agent = agent_id.clone();
        let view_connection = connection.clone();
        let image_connection = connection.clone();
        let mut view = Self {
            agent_id,
            connection,
            shared: shared.clone(),
            history: History::default(),
            buffer: LiveBuffer::default(),
            pending: None,
            connected: true,
            dirty: false,
            want_tail: false,
            error: None,
            refresh_scheduled: false,
            schedule_generation: 0,
            retry_ms: 1000,
            retry_query: Query::After,
            whole_turns,
            foreground_sync: true,
            composer,
            rows: BTreeMap::new(),
            display: Vec::new(),
            expanded_turns: BTreeSet::new(),
            active_turn: None,
            footers: BTreeMap::new(),
            permissions: cx.new(|cx| {
                PermissionBoard::new(view_agent.clone(), view_connection, shared.clone(), cx)
            }),
            images: cx.new(|cx| ImageCache::new(image_connection, cx)),
            workspace_root: None,
            pending_sent: None,
            agent_running: false,
        };
        cx.observe(&view.composer, |this, _, cx| {
            if this.refresh_pending(cx) {
                cx.notify();
            }
        })
        .detach();
        cx.observe(&view.images.clone(), |_, _, cx| cx.notify())
            .detach();
        if let Err(error) = view.fetch(Query::Tail, cx) {
            view.failed(error.to_string(), Some(Query::Tail), cx);
        }
        view
    }
    pub fn disconnected(&mut self, cx: &mut Context<Self>) {
        self.connected = false;
        self.pending = None;
        self.schedule_generation = self.schedule_generation.saturating_add(1);
        self.refresh_scheduled = false;
        self.dirty = true;
        self.foreground_sync = true;
        self.error = Some("Host disconnected. Messages will catch up after reconnect.".into());
        self.clear_images(cx);
        cx.notify();
    }

    pub fn resume(&mut self, cx: &mut Context<Self>) {
        self.connected = true;
        self.pending = None;
        self.error = None;
        self.foreground_sync = true;
        self.schedule_generation = self.schedule_generation.saturating_add(1);
        self.refresh_scheduled = false;
        self.retry_ms = 1000;
        if let Err(error) = self.fetch(Query::After, cx) {
            self.failed(error.to_string(), None, cx);
        }
        cx.notify();
    }
    /// Appearance, content width or message sizes changed. Message views read
    /// the shared settings during render; notify them so cached layouts redraw.
    pub fn settings_changed(&mut self, cx: &mut Context<Self>) {
        for view in self.rows.values() {
            if let RowView::Message(view) = view {
                view.update(cx, |_, cx| cx.notify());
            }
        }
        if let Some((_, view)) = &self.pending_sent {
            view.update(cx, |_, cx| cx.notify());
        }
        cx.notify();
    }
    fn fetch(&mut self, query: Query, cx: &mut Context<Self>) -> Result<()> {
        ensure!(self.connected, "Host disconnected");
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
        let permission_message = self
            .permissions
            .update(cx, |board, cx| board.receive(kind, payload, cx));
        if permission_message {
            cx.notify();
            return Ok(());
        }
        if self
            .images
            .update(cx, |images, cx| images.receive_session(kind, payload, cx))
        {
            cx.notify();
            return Ok(());
        }
        if kind == "send_agent_message_response" {
            if self.refresh_pending(cx) {
                cx.notify();
            }
            return Ok(());
        }
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
                        self.rows.clear();
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
                self.refresh(cx);
                match result {
                    Err(error) => self.failed(error.to_string(), Some(pending.query), cx),
                    Ok(need) => {
                        self.error = None;
                        self.retry_ms = 1000;
                        self.dirty = need != SyncNeed::Current || self.want_tail;
                        if !self.dirty {
                            self.foreground_sync = false;
                        }
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
                        if !inserted && self.sync_live_row(anchor, cx) {
                            cx.notify();
                            return Ok(());
                        }
                        self.refresh(cx);
                        cx.notify();
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
    pub fn set_permissions(&mut self, agent_id: &str, pending: Vec<Value>, cx: &mut Context<Self>) {
        self.permissions
            .update(cx, |board, cx| board.set_pending(agent_id, pending, cx));
    }
    pub fn set_running(&mut self, running: bool, cx: &mut Context<Self>) {
        if self.agent_running != running {
            self.agent_running = running;
            self.refresh(cx);
            cx.notify();
        }
    }
    pub fn set_workspace_root(&mut self, root: Option<String>, cx: &mut Context<Self>) {
        if self.workspace_root == root {
            return;
        }
        self.workspace_root = root;
        self.images
            .update(cx, |images, cx| images.retry_unresolved(cx));
        self.refresh(cx);
        cx.notify();
    }
    /// Hand a binary file-transfer frame to the image cache. The UI calls this for
    /// `Event::Binary`; unrelated frames return false and stay with their owner.
    #[allow(dead_code)] // The UI dispatch is merged with the parent-owned Event::Binary change.
    pub fn receive_binary(&mut self, bytes: &[u8], cx: &mut Context<Self>) -> bool {
        let handled = self
            .images
            .update(cx, |images, cx| images.receive_binary(bytes, cx));
        if handled {
            cx.notify();
        }
        handled
    }
    pub fn clear_images(&mut self, cx: &mut Context<Self>) {
        self.images.update(cx, |images, cx| images.clear(cx));
    }
    fn attach_images(&mut self, view: &Entity<MessageView>, cx: &mut Context<Self>) {
        view.update(cx, |view, _| {
            view.set_images(self.images.clone(), self.workspace_root.clone());
        });
        let sources = view.read(cx).image_sources();
        if sources.is_empty() {
            return;
        }
        let root = self.workspace_root.clone();
        self.images.update(cx, |images, cx| {
            for source in sources {
                images.ensure(root.as_deref(), &source, cx);
            }
        });
    }
    pub fn set_active_turn(&mut self, turn: Option<String>, cx: &mut Context<Self>) {
        if self.active_turn == turn {
            return;
        }
        self.active_turn = turn;
        self.refresh(cx);
        cx.notify();
    }
    fn toggle_turn(&mut self, host: u64, cx: &mut Context<Self>) {
        if !self.expanded_turns.remove(&host) {
            self.expanded_turns.insert(host);
        }
        self.refresh(cx);
        cx.notify();
    }
    fn refresh_pending(&mut self, cx: &mut Context<Self>) -> bool {
        let pending = self.composer.read(cx).pending_sent();
        match pending {
            None => {
                let had = self.pending_sent.take().is_some();
                had
            }
            Some((id, text, attachments)) => {
                if let Some((current, view)) = &mut self.pending_sent {
                    if current == &id {
                        view.update(cx, |view, cx| {
                            view.set_text(&text, true, cx);
                            view.set_attachments(attachments, cx);
                        });
                        return false;
                    }
                }
                let view = cx.new(|cx| MessageView::new(text, true, None, self.shared.clone(), cx));
                view.update(cx, |view, cx| view.set_attachments(attachments, cx));
                cx.subscribe(
                    &view,
                    |_, _, event: &crate::message_view::MessageEvent, cx| match event {
                        crate::message_view::MessageEvent::OpenUrl(url) => cx.open_url(url),
                    },
                )
                .detach();
                self.attach_images(&view, cx);
                self.pending_sent = Some((id, view));
                true
            }
        }
    }
    fn has_pending_echo(&self) -> bool {
        let Some((id, _)) = &self.pending_sent else {
            return false;
        };
        timeline::has_pending_echo(&self.history.entries, id)
    }
    fn sync_row(&mut self, id: u64, cx: &mut Context<Self>) -> bool {
        let Some(entry) = self.history.entries.get(&id) else {
            return false;
        };
        let kind = entry.item["type"].as_str().unwrap_or("");
        if kind == "todo" {
            return false;
        }
        match kind {
            "user_message" | "assistant_message" => {
                let user = kind == "user_message";
                let source = entry.item["text"].as_str().unwrap_or("");
                let timestamp = entry.timestamp.clone();
                // A retained entry can be re-projected into a different item type under the
                // same seq key; a stale non-message row is replaced, never assumed.
                let existing = self
                    .rows
                    .get(&id)
                    .filter(|row| reuses_message_row(Some(row.kind())))
                    .and_then(|row| match row {
                        RowView::Message(view) => Some(view.clone()),
                        _ => None,
                    });
                match existing {
                    Some(view) => {
                        view.update(cx, |view, cx| view.set_text(source, user, cx));
                        self.attach_images(&view, cx);
                        self.rows.insert(id, RowView::Message(view));
                        return false;
                    }
                    None => {
                        let view = cx.new(|cx| {
                            MessageView::new(
                                source.to_owned(),
                                user,
                                timestamp,
                                self.shared.clone(),
                                cx,
                            )
                        });
                        cx.subscribe(
                            &view,
                            |_, _, event: &crate::message_view::MessageEvent, cx| match event {
                                crate::message_view::MessageEvent::OpenUrl(url) => cx.open_url(url),
                            },
                        )
                        .detach();
                        self.attach_images(&view, cx);
                        self.rows.insert(id, RowView::Message(view.clone()));
                        true
                    }
                }
            }
            "reasoning" => {
                if let Some(RowView::Thought(view)) = self.rows.get(&id) {
                    view.update(cx, |view, cx| view.set(&entry.item, cx));
                    return false;
                }
                let view = cx.new(|cx| ThoughtRow::new(&entry.item, self.shared.clone(), cx));
                self.rows.insert(id, RowView::Thought(view));
                true
            }
            "tool_call" => {
                if entry.item["detail"]["type"] == "plan" {
                    if let Some(RowView::Plan(view)) = self.rows.get(&id) {
                        view.update(cx, |view, cx| view.set(&entry.item, cx));
                        return false;
                    }
                    let view = cx.new(|cx| PlanRow::new(&entry.item, self.shared.clone(), cx));
                    self.rows.insert(id, RowView::Plan(view));
                    return true;
                }
                if let Some(RowView::Tool(view)) = self.rows.get(&id) {
                    view.update(cx, |view, cx| view.set_item(&entry.item, cx));
                    return false;
                }
                let view = cx.new(|cx| ToolRow::new(&entry.item, None, self.shared.clone(), cx));
                self.rows.insert(id, RowView::Tool(view));
                true
            }
            _ => false,
        }
    }
    fn sync_todos(&mut self, rows: &[timeline::Row], cx: &mut Context<Self>) {
        let keys = rows
            .iter()
            .filter(|row| row.kind == timeline::Kind::Todo)
            .map(|row| row.key)
            .collect::<BTreeSet<_>>();
        self.rows
            .retain(|key, view| !matches!(view, RowView::Todo(_)) || keys.contains(key));
        for row in rows.iter().filter(|row| row.kind == timeline::Kind::Todo) {
            if let Some(RowView::Todo(view)) = self.rows.get(&row.key) {
                view.update(cx, |view, cx| view.set(&row.item, row.activity.clone(), cx));
            } else {
                let activity = row.activity.clone();
                let item = row.item.clone();
                let view = cx.new(|cx| TodoRow::new(&item, activity, self.shared.clone(), cx));
                self.rows.insert(row.key, RowView::Todo(view));
            }
        }
    }
    /// Update the single row a streaming event touched. Structural inserts and todo snapshots
    /// still rebuild, but token updates must stay O(row) instead of cloning the timeline.
    fn sync_live_row(&mut self, anchor: u64, cx: &mut Context<Self>) -> bool {
        let Some(entry) = self.history.entries.get(&anchor) else {
            return false;
        };
        let is_todo = entry.item["type"] == "todo";
        let item = entry.item.clone();
        let timestamp = entry.timestamp.clone();
        if is_todo {
            return false;
        }
        if self.sync_row(anchor, cx) {
            return false;
        }
        let Some(index) = self
            .display
            .iter()
            .position(|entry| entry.item().is_some_and(|row| row.key == anchor))
        else {
            return true;
        };
        let footer = self.display[index]
            .item()
            .and_then(|row| row.footer.as_ref().map(|footer| footer.turn.clone()));
        if let timeline::DisplayRow::Item { row, .. } = &mut self.display[index] {
            row.item = item;
            row.timestamp = timestamp.clone();
        }
        if let Some(turn) = footer {
            self.update_footer(&turn, anchor, timestamp.as_deref(), cx);
        }
        true
    }
    fn update_footer(
        &mut self,
        turn: &str,
        anchor: u64,
        completed_at: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        let Some(view) = self.footers.get(turn).cloned() else {
            return;
        };
        let Some(cached) = self
            .display
            .iter()
            .filter_map(|entry| entry.item())
            .find_map(|row| row.footer.as_ref().filter(|footer| footer.turn == turn))
            .cloned()
        else {
            return;
        };
        let started_at = cached.started_at.clone();
        let footer = timeline::Footer {
            copy_text: timeline::copy_for_turn(
                &self.history.entries,
                cached
                    .turn
                    .strip_prefix("seq-")
                    .is_none()
                    .then_some(cached.turn.as_str()),
                anchor,
            ),
            duration_ms: timeline::duration_between(
                cached.started_at.as_deref(),
                completed_at.or(cached.completed_at.as_deref()),
            ),
            completed_at: completed_at
                .map(str::to_owned)
                .or(cached.completed_at.clone()),
            ..cached
        };
        let running = self.agent_running
            && self
                .display
                .iter()
                .rev()
                .filter_map(|entry| entry.item())
                .filter_map(|row| row.footer.as_ref().map(|footer| footer.turn.as_str()))
                .next()
                == Some(turn);
        view.update(cx, |view, cx| {
            view.set(Some(footer), running, started_at, cx)
        });
    }
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let rows = timeline::rows(&self.history.entries);
        let display = timeline::display_rows(
            &self.history.entries,
            &self.expanded_turns,
            self.active_turn.as_deref(),
        );
        let keys = rows.iter().map(|row| row.key).collect::<BTreeSet<_>>();
        self.rows.retain(|key, _| keys.contains(key));
        for row in &rows {
            self.sync_row(row.key, cx);
        }
        let last_key = rows.last().map(|row| row.key);
        for (index, row) in rows.iter().enumerate() {
            if let Some(RowView::Thought(view)) = self.rows.get(&row.key) {
                let loading = self.agent_running
                    && row.kind == timeline::Kind::Reasoning
                    && last_key == Some(row.key);
                view.update(cx, |view, cx| view.set_loading(loading, cx));
            }
            let last_in_sequence = matches!(
                row.kind,
                timeline::Kind::Tool | timeline::Kind::Reasoning | timeline::Kind::Todo
            ) && rows.get(index + 1).is_none_or(|next| {
                !matches!(
                    next.kind,
                    timeline::Kind::Tool | timeline::Kind::Reasoning | timeline::Kind::Todo
                )
            });
            match self.rows.get(&row.key) {
                Some(RowView::Tool(view)) => {
                    view.update(cx, |view, _| view.set_last_in_sequence(last_in_sequence))
                }
                Some(RowView::Thought(view)) => {
                    view.update(cx, |view, _| view.set_last_in_sequence(last_in_sequence))
                }
                Some(RowView::Todo(view)) => {
                    view.update(cx, |view, _| view.set_last_in_sequence(last_in_sequence))
                }
                Some(RowView::Plan(view)) => {
                    view.update(cx, |view, _| view.set_last_in_sequence(last_in_sequence))
                }
                Some(RowView::Message(view)) => {
                    view.update(cx, |view, _| view.set_footer_below(row.footer.is_some()))
                }
                None => {}
            }
        }
        self.sync_todos(&rows, cx);
        self.display = display;
        self.refresh_footers(cx);
        self.refresh_pending(cx);
    }
    fn refresh_footers(&mut self, cx: &mut Context<Self>) {
        let turns = self
            .display
            .iter()
            .filter_map(|entry| entry.item())
            .filter_map(|row| row.footer.as_ref().map(|footer| footer.turn.clone()))
            .collect::<BTreeSet<_>>();
        self.footers.retain(|turn, _| turns.contains(turn));
        let last_turn = self
            .display
            .iter()
            .filter_map(|entry| entry.item())
            .filter_map(|row| row.footer.as_ref().map(|footer| footer.turn.clone()))
            .next_back();
        for row in self.display.iter().filter_map(|entry| entry.item()) {
            let Some(footer) = &row.footer else {
                continue;
            };
            let running = self.agent_running && last_turn.as_deref() == Some(footer.turn.as_str());
            let view = match self.footers.get(&footer.turn) {
                Some(view) => view.clone(),
                None => {
                    let shared = self.shared.clone();
                    let view = cx.new(|cx| TurnFooter::new(shared, cx));
                    self.footers.insert(footer.turn.clone(), view.clone());
                    view
                }
            };
            view.update(cx, |view, cx| {
                view.set(Some(footer.clone()), running, footer.started_at.clone(), cx)
            });
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
        if self.refresh_scheduled || !self.connected {
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
                if this.schedule_generation != generation || !this.connected {
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use crate::{chrome::loading_spinner, sync_presentation as p};
        let palette = crate::settings_view::active_palette(&self.shared, window);
        let settings = self.shared.get();
        let content_width = settings
            .content_max_width
            .unwrap_or(paseo_desktop::settings::DEFAULT_CONTENT_MAX_WIDTH)
            as f32;
        let initial_loading = self.history.epoch.is_none() && self.error.is_none();
        // Gap repair remains silent; the original status toast belongs to foreground recovery.
        let updating = !initial_loading && self.error.is_none() && self.foreground_sync;
        let show_error = self.error.is_some() && self.foreground_sync;
        div()
            .id("agent-conversation")
            .relative()
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
                                slot.when(
                                    self.pending
                                        .as_ref()
                                        .is_some_and(|p| p.query == Query::Before),
                                    |slot| {
                                        slot.child(loading_spinner(
                                            p::SPINNER_SMALL,
                                            palette.foreground_muted,
                                        ))
                                    },
                                )
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
                    .when(
                        shows_empty_state(
                            self.history.entries.is_empty(),
                            self.pending.is_some(),
                            self.pending_sent.is_some(),
                        ),
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
                                    .text_color(rgb(palette.foreground_muted))
                                    .child("Start chatting with this agent..."),
                            )
                        },
                    )
                    .children(self.display.iter().enumerate().map(|(index, entry)| {
                        let host_gap = self
                            .display
                            .get(index.wrapping_sub(1))
                            .and_then(|entry| entry.item())
                            .map(|row| row.gap)
                            .unwrap_or(0.);
                        match entry {
                            timeline::DisplayRow::Summary {
                                host,
                                summary,
                                expanded,
                                ..
                            } => {
                                let host = *host;
                                let gap = if host_gap > 0. { host_gap } else { 4. };
                                div()
                                    .id(("turn-summary", host))
                                    .mx(px(-13.))
                                    .mb(px(gap))
                                    .w_full()
                                    .min_w_0()
                                    .flex()
                                    .items_center()
                                    .rounded(px(crate::theme::radius::LG))
                                    .px_2()
                                    .py_1()
                                    .cursor_pointer()
                                    .hover(|style| style.bg(rgb(palette.surface1)))
                                    .child(
                                        div()
                                            .size(px(22.))
                                            .mr_1()
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .child(
                                                crate::chrome::icon(
                                                    if *expanded {
                                                        "chevron-down"
                                                    } else {
                                                        "summary"
                                                    },
                                                    12.,
                                                )
                                                .ml(px(-1.))
                                                .text_color(rgb(if *expanded {
                                                    palette.foreground
                                                } else {
                                                    palette.muted_foreground
                                                })),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .text_color(rgb(if *expanded {
                                                palette.foreground
                                            } else {
                                                palette.foreground_muted
                                            }))
                                            .child(timeline::summary_label(summary)),
                                    )
                                    .child(div().flex_1())
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.toggle_turn(host, cx);
                                        }),
                                    )
                                    .into_any_element()
                            }
                            timeline::DisplayRow::Item { row, nested } => {
                                let followed_by_summary = matches!(
                                    self.display.get(index + 1),
                                    Some(timeline::DisplayRow::Summary { .. })
                                );
                                let gap = if followed_by_summary { 0. } else { row.gap };
                                let content = div()
                                    .w_full()
                                    .max_w(px(content_width))
                                    .px_2()
                                    .when(*nested, |content| {
                                        content
                                            .ml(px(1.))
                                            .pl(px(20.))
                                            .border_l_1()
                                            .border_color(rgb(palette.border))
                                    })
                                    .child(match self.rows.get(&row.key) {
                                        Some(RowView::Message(view)) => {
                                            view.clone().into_any_element()
                                        }
                                        Some(RowView::Tool(view)) => {
                                            view.clone().into_any_element()
                                        }
                                        Some(RowView::Thought(view)) => {
                                            view.clone().into_any_element()
                                        }
                                        Some(RowView::Todo(view)) => {
                                            view.clone().into_any_element()
                                        }
                                        Some(RowView::Plan(view)) => {
                                            view.clone().into_any_element()
                                        }
                                        None => marker(row, &palette).into_any_element(),
                                    });
                                let footer = row
                                    .footer
                                    .as_ref()
                                    .and_then(|footer| self.footers.get(&footer.turn).cloned());
                                div()
                                    .id(("timeline-row", row.key))
                                    .flex_shrink_0()
                                    .w_full()
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div()
                                            .w_full()
                                            .flex()
                                            .justify_center()
                                            .when(footer.is_none(), |row| row.mb(px(gap)))
                                            .child(content),
                                    )
                                    .when_some(footer, |container, footer| {
                                        container.child(
                                            div()
                                                .w_full()
                                                .flex()
                                                .justify_center()
                                                .mb(px(gap))
                                                .child(
                                                    div()
                                                        .w_full()
                                                        .max_w(px(content_width))
                                                        .px_2()
                                                        .child(footer),
                                                ),
                                        )
                                    })
                                    .into_any_element()
                            }
                        }
                    }))
                    .when(
                        shows_pending_bubble(self.pending_sent.is_some(), self.has_pending_echo()),
                        |view| {
                            view.child(div().w_full().flex().justify_center().child(
                                div().w_full().max_w(px(content_width)).px_2().children(
                                    self.pending_sent.as_ref().map(|(_, view)| view.clone()),
                                ),
                            ))
                        },
                    ),
            )
            .when(show_error, |root| {
                root.child(
                    div()
                        .w_full()
                        .flex_shrink_0()
                        .flex()
                        .justify_center()
                        .px(px(p::RAIL_PADDING_X))
                        .pt(px(p::RAIL_PADDING_TOP))
                        .child(
                            div()
                                .w_full()
                                .max_w(px(p::MAX_WIDTH))
                                .flex()
                                .items_center()
                                .justify_center()
                                .gap(px(p::CALLOUT_GAP))
                                .bg(rgb(p::CALLOUT_FILL))
                                .border(px(p::CALLOUT_BORDER))
                                .border_color(rgb(p::CALLOUT_BORDER_COLOR))
                                .rounded(px(p::CALLOUT_RADIUS))
                                .px(px(p::CALLOUT_PADDING_X))
                                .py(px(p::CALLOUT_PADDING_Y))
                                .child(
                                    div()
                                        .text_size(px(p::CALLOUT_TEXT_SIZE))
                                        .text_color(rgb(p::CALLOUT_TEXT_COLOR))
                                        .child(p::ERROR),
                                )
                                .child(
                                    div()
                                        .id("retry-timeline")
                                        .flex_shrink_0()
                                        .min_h(px(p::BUTTON_HEIGHT))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .px(px(p::BUTTON_PADDING_X))
                                        .border(px(p::BUTTON_BORDER))
                                        .border_color(rgb(p::BUTTON_BORDER_COLOR))
                                        .bg(rgb(p::BUTTON_FILL))
                                        .text_size(px(p::BUTTON_TEXT_SIZE))
                                        .text_color(rgb(p::BUTTON_TEXT_COLOR))
                                        .rounded(px(p::BUTTON_RADIUS))
                                        .child(if self.pending.is_some() {
                                            p::RETRYING
                                        } else {
                                            p::RETRY
                                        })
                                        .when(self.pending.is_some(), |button| {
                                            let blend = |color| {
                                                rgb(crate::chrome::control_color(
                                                    color,
                                                    p::CALLOUT_FILL,
                                                    p::BUTTON_DISABLED_OPACITY,
                                                ))
                                            };
                                            button
                                                .bg(blend(p::BUTTON_FILL))
                                                .border_color(blend(p::BUTTON_BORDER_COLOR))
                                                .text_color(blend(p::BUTTON_TEXT_COLOR))
                                        })
                                        .when(self.pending.is_none(), |button| {
                                            button
                                                .cursor_pointer()
                                                .active(|style| {
                                                    style.opacity(p::BUTTON_PRESSED_OPACITY)
                                                })
                                                .on_click(
                                                    cx.listener(|this, _, _, cx| this.retry(cx)),
                                                )
                                        }),
                                ),
                        ),
                )
            })
            .child(
                div()
                    .w_full()
                    .flex_shrink_0()
                    .flex()
                    .justify_center()
                    .px_2()
                    .child(
                        div()
                            .w_full()
                            .max_w(px(content_width))
                            .child(self.permissions.clone()),
                    ),
            )
            .child(self.composer.clone())
            .when(initial_loading, |root| {
                root.child(
                    div()
                        .absolute()
                        .inset_0()
                        .occlude()
                        .bg(rgb(palette.surface0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(loading_spinner(p::SPINNER_LARGE, palette.foreground_muted)),
                )
            })
            .when(updating, |root| {
                root.child(
                    div()
                        .absolute()
                        .top(px(p::TOAST_TOP))
                        .left(px(p::TOAST_INSET_X))
                        .right(px(p::TOAST_INSET_X))
                        .flex()
                        .justify_center()
                        .child(
                            div()
                                .w(gpui::relative(p::TOAST_WIDTH_RATIO))
                                .max_w(px(p::TOAST_MAX_WIDTH))
                                .flex()
                                .justify_center()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(p::TOAST_GAP))
                                        .bg(rgb(p::TOAST_FILL))
                                        .border(px(p::TOAST_BORDER))
                                        .border_color(rgb(p::TOAST_BORDER_COLOR))
                                        .text_size(px(p::TOAST_TEXT_SIZE))
                                        .line_height(crate::fonts::ui_strut(
                                            window,
                                            p::TOAST_TEXT_SIZE,
                                        ))
                                        .text_color(rgb(p::TOAST_TEXT_COLOR))
                                        .rounded(px(p::TOAST_RADIUS))
                                        .px(px(p::TOAST_PADDING_X))
                                        .py(px(p::TOAST_PADDING_Y))
                                        .shadow(vec![
                                            BoxShadow::new(
                                                px(p::TOAST_SHADOW_X),
                                                px(p::TOAST_SHADOW_Y),
                                                rgba(p::TOAST_SHADOW_RGBA).into(),
                                            )
                                            .blur_radius(px(p::TOAST_SHADOW_BLUR)),
                                        ])
                                        .child(loading_spinner(
                                            p::SPINNER_TOAST,
                                            p::TOAST_TEXT_COLOR,
                                        ))
                                        .child(p::UPDATING),
                                ),
                        ),
                )
            })
    }
}

fn marker(row: &timeline::Row, palette: &paseo_desktop::appearance::Palette) -> gpui::AnyElement {
    let (icon, color, text) = match row.kind {
        timeline::Kind::Notification => {
            let (level, message) = timeline::notification(&row.item);
            let color = match level {
                timeline::NotificationLevel::Error => palette.destructive,
                timeline::NotificationLevel::Warning => palette.status_warning,
                timeline::NotificationLevel::Info => palette.foreground_muted,
            };
            ("triangle-alert", color, message)
        }
        timeline::Kind::Compaction => (
            "scissors",
            palette.foreground_muted,
            timeline::compaction_label(&row.item),
        ),
        _ => return div().into_any_element(),
    };
    div()
        .w_full()
        .flex()
        .items_center()
        .justify_center()
        .gap_2()
        .py_1()
        .child(crate::chrome::icon(icon, 13.).text_color(rgb(color)))
        .child(
            div()
                .min_w_0()
                .text_center()
                .text_size(px(13.))
                .text_color(rgb(color))
                .child(text),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_rows_are_rebuilt_when_the_projection_changes_type() {
        assert!(reuses_message_row(Some(RowKind::Message)));
        for kind in [
            RowKind::Tool,
            RowKind::Thought,
            RowKind::Todo,
            RowKind::Plan,
        ] {
            assert!(!reuses_message_row(Some(kind)));
        }
        assert!(!reuses_message_row(None));
    }

    #[test]
    fn pending_bubble_shows_for_the_first_message_and_hides_on_echo() {
        assert!(shows_pending_bubble(true, false));
        assert!(!shows_pending_bubble(true, true));
        assert!(!shows_pending_bubble(false, false));
        // Empty history is not a gate: the first optimistic message still renders.
        assert!(!shows_empty_state(true, false, true));
        assert!(shows_empty_state(true, false, false));
        assert!(!shows_empty_state(false, false, false));
        assert!(!shows_empty_state(true, true, false));
    }
}

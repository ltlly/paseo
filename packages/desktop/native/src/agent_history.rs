use crate::theme::{colors as c, font_size};
use gpui::{Context, EventEmitter, MouseButton, Render, Window, div, prelude::*, px, rgb};
use paseo_desktop::{
    client::Connection,
    model::{Agent, HistoryPage},
};
use serde_json::{Value, json};
use std::sync::Arc;

/// Fetch deadline shared with the workspace form: past it, acceptance is
/// unknown and the panel never retries on its own.
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(150);
const PAGE_LIMIT: i64 = 200;

pub struct OpenAgent(pub Agent);
pub struct Dismiss;

enum Load {
    /// The first page never arrived; the panel shows retry instead of an empty
    /// claim that history is empty.
    Empty,
    Pending {
        id: String,
    },
    Failed(String),
}

pub struct HistoryPanel {
    connection: Arc<Connection>,
    entries: Vec<Agent>,
    cursor: Option<String>,
    has_more: bool,
    state: Load,
    /// Request ids already settled; late rpc_error replies are swallowed.
    settled: Vec<String>,
}

impl EventEmitter<OpenAgent> for HistoryPanel {}
impl EventEmitter<Dismiss> for HistoryPanel {}

impl HistoryPanel {
    pub fn new(connection: Arc<Connection>, cx: &mut Context<Self>) -> Self {
        let mut panel = Self {
            connection,
            entries: Vec::new(),
            cursor: None,
            has_more: false,
            state: Load::Empty,
            settled: Vec::new(),
        };
        panel.load(None, cx);
        panel
    }

    fn load(&mut self, cursor: Option<String>, cx: &mut Context<Self>) {
        let mut page = json!({"limit": PAGE_LIMIT});
        if let Some(cursor) = &cursor {
            page["cursor"] = cursor.clone().into();
        }
        match self.connection.request(
            "fetch_agent_history_request",
            json!({"sort":[{"key":"updated_at","direction":"desc"}],"page":page}),
        ) {
            Ok(id) => {
                self.state = Load::Pending { id: id.clone() };
                let timer = cx.background_executor().timer(REQUEST_TIMEOUT);
                cx.spawn(async move |this, cx| {
                    timer.await;
                    let _ = this.update(cx, |this, cx| {
                        if let Load::Pending { id: pending } = &this.state
                            && pending == &id
                        {
                            this.remember_settled(&id);
                            this.state = Load::Failed(
                                "History timed out. Check the host state before retrying.".into(),
                            );
                            cx.notify();
                        }
                    });
                })
                .detach();
            }
            Err(error) => self.state = Load::Failed(error.to_string()),
        }
        cx.notify();
    }

    pub fn owns(&self, payload: &Value) -> bool {
        matches!(&self.state, Load::Pending { id } if payload["requestId"].as_str() == Some(id))
    }

    pub fn dismisses_error(&self, payload: &Value) -> bool {
        payload["requestId"]
            .as_str()
            .is_some_and(|id| self.settled.iter().any(|seen| seen == id))
    }

    pub fn receive(&mut self, kind: &str, payload: &Value, cx: &mut Context<Self>) {
        if !self.owns(payload) {
            return;
        }
        if let Load::Pending { id } = &self.state {
            let id = id.clone();
            self.remember_settled(&id);
        }
        if kind == "rpc_error" {
            self.state = Load::Failed(payload["error"].as_str().unwrap_or("History failed").into());
            cx.notify();
            return;
        }
        match serde_json::from_value::<HistoryPage>(payload.clone()) {
            Ok(page) => {
                for entry in page.entries {
                    if !self.entries.iter().any(|seen| seen.id == entry.agent.id) {
                        self.entries.push(entry.agent);
                    }
                }
                self.cursor = page.page_info.next_cursor;
                self.has_more = page.page_info.has_more;
                self.state = Load::Empty;
            }
            Err(error) => self.state = Load::Failed(error.to_string()),
        }
        cx.notify();
    }

    pub fn disconnected(&mut self, cx: &mut Context<Self>) {
        if let Load::Pending { id } = &self.state {
            let id = id.clone();
            self.remember_settled(&id);
            self.state = Load::Failed(
                "Host disconnected. History may be incomplete; retry when reconnected.".into(),
            );
            cx.notify();
        }
    }

    fn remember_settled(&mut self, id: &str) {
        if !self.settled.iter().any(|seen| seen == id) {
            self.settled.push(id.into());
            let excess = self.settled.len().saturating_sub(32);
            self.settled.drain(..excess);
        }
    }

    fn button(
        &self,
        id: &'static str,
        label: &str,
        cx: &mut Context<Self>,
        handler: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> gpui::AnyElement {
        let label = label.to_owned();
        div()
            .id(id)
            .h(px(32.))
            .px_3()
            .rounded(px(6.))
            .flex()
            .items_center()
            .justify_center()
            .bg(rgb(c::SURFACE2))
            .cursor_pointer()
            .child(label)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    handler(this, cx);
                }),
            )
            .into_any_element()
    }

    fn status_label(agent: &Agent) -> String {
        if agent.archived_at.is_some() {
            return "Archived".into();
        }
        match agent.sidebar_status() {
            paseo_desktop::model::AgentStatusBucket::NeedsInput => "Needs input".into(),
            paseo_desktop::model::AgentStatusBucket::Failed => "Error".into(),
            paseo_desktop::model::AgentStatusBucket::Running => "Running".into(),
            paseo_desktop::model::AgentStatusBucket::Attention => "Attention".into(),
            paseo_desktop::model::AgentStatusBucket::Done => "Idle".into(),
        }
    }
}

impl Render for HistoryPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let loading = matches!(self.state, Load::Pending { .. });
        let mut body = div().flex().flex_col().gap_2().w_full();
        if let Load::Failed(error) = &self.state {
            body = body
                .child(div().text_color(rgb(c::STATUS_DANGER)).child(error.clone()))
                .child(self.button("retry", "Retry", cx, |this, cx| {
                    let cursor = this.cursor.clone();
                    this.load(
                        if this.entries.is_empty() {
                            None
                        } else {
                            cursor
                        },
                        cx,
                    );
                }));
        } else if self.entries.is_empty() {
            body = body.child(
                div()
                    .text_color(rgb(c::FOREGROUND_MUTED))
                    .child(if loading {
                        "Loading history…"
                    } else {
                        "No agent history."
                    }),
            );
        }
        if !self.entries.is_empty() {
            body = body.child(
                div()
                    .id("history-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .children(self.entries.iter().map(|agent| {
                        let id = agent.id.clone();
                        let title = agent.display_title().to_owned();
                        let status = Self::status_label(agent);
                        let workspace = agent.workspace_id.clone();
                        div()
                            .id(gpui::SharedString::from(format!("history-{id}")))
                            .h(px(36.))
                            .px_2()
                            .rounded(px(6.))
                            .flex()
                            .items_center()
                            .gap_2()
                            .hover(|row| row.bg(rgb(c::SURFACE2)))
                            .cursor_pointer()
                            .child(crate::chrome::provider_icon_data(
                                &agent.provider,
                                14.,
                                None,
                            ))
                            .child(div().min_w_0().flex_1().truncate().child(title))
                            .child(div().text_color(rgb(c::FOREGROUND_MUTED)).child(status))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    if let Some(agent) =
                                        this.entries.iter().find(|a| a.id == id).cloned()
                                    {
                                        cx.emit(OpenAgent(agent));
                                    }
                                }),
                            )
                            .when(workspace.is_none(), |row| {
                                row.text_color(rgb(c::FOREGROUND_MUTED))
                            })
                    })),
            );
        }
        if self.has_more && !loading && !matches!(self.state, Load::Failed(_)) {
            body = body.child(self.button("load-more", "Load more", cx, |this, cx| {
                let cursor = this.cursor.clone();
                this.load(cursor, cx);
            }));
        }
        body = body.child(self.button("close", "Close", cx, |_, cx| cx.emit(Dismiss)));
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgb(c::SURFACE_WORKSPACE))
            .child(
                div()
                    .w(px(560.))
                    .h(px(640.))
                    .max_w_full()
                    .max_h_full()
                    .p_4()
                    .text_size(px(font_size::BASE))
                    .text_color(rgb(c::FOREGROUND))
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(div().text_size(px(font_size::LG)).child("History"))
                    .child(body),
            )
    }
}

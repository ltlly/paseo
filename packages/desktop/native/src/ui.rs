use std::sync::Arc;

use anyhow::{Context as _, Result};
use gpui::{Context, Entity, MouseButton, Render, Window, div, prelude::*, px, rgb};
use serde_json::{Value, json};

use crate::terminal_view::TerminalView;
use paseo_desktop::{
    client::{Connection, Event, Target},
    model::{AgentExecution, AgentPage, Directory, ServerInfo, WorkspacePage},
};

pub struct Paseo {
    connection: Option<Arc<Connection>>,
    info: Option<ServerInfo>,
    directory: Directory,
    selected_workspace: Option<String>,
    selected_terminal: Option<String>,
    terminal: Option<Entity<TerminalView>>,
    error: Option<String>,
    selected_agent: Option<String>,
    conversation: Option<Entity<crate::conversation::Conversation>>,
}

impl Paseo {
    pub fn new(target: Result<Target>, cx: &mut Context<Self>) -> Self {
        let mut app = Self {
            connection: None,
            info: None,
            directory: Directory::default(),
            selected_workspace: None,
            selected_terminal: None,
            terminal: None,
            error: None,
            selected_agent: None,
            conversation: None,
        };
        match target.and_then(Connection::start) {
            Ok(connection) => {
                let events = connection.events.clone();
                app.connection = Some(Arc::new(connection));
                cx.spawn(async move |this, cx| {
                    while let Ok(event) = events.recv().await {
                        if this.update(cx, |this, cx| this.receive(event, cx)).is_err() {
                            break;
                        }
                    }
                })
                .detach();
            }
            Err(error) => app.error = Some(format!("{error:#}")),
        }
        app
    }

    fn receive(&mut self, event: Event, cx: &mut Context<Self>) {
        let result = match event {
            Event::Ready(info) => {
                self.info = Some(info);
                self.load_directory()
            }
            Event::Session { kind, payload } => {
                let timeline_owned = matches!(
                    kind.as_str(),
                    "agent_stream" | "fetch_agent_timeline_response" | "agent.timeline.replacement"
                );
                let result = self.apply_session(&kind, payload, cx);
                if timeline_owned && result.is_ok() {
                    return;
                }
                result
            }
            Event::Terminal(frame) => {
                if let Some(terminal) = &self.terminal {
                    terminal.update(cx, |terminal, cx| terminal.receive(frame, cx));
                }
                return;
            }
            Event::Disconnected(error) => {
                self.terminal = None;
                self.error = Some(error);
                Ok(())
            }
        };
        if let Err(error) = result {
            self.error = Some(format!("{error:#}"));
        }
        cx.notify();
    }

    fn load_directory(&self) -> Result<()> {
        let connection = self.connection.as_ref().context("host is disconnected")?;
        connection.request("project.list.request", json!({}))?;
        connection.request("fetch_workspaces_request", json!({"subscribe":{}}))?;
        connection.request("list_terminals_request", json!({}))?;
        connection.request(
            "fetch_agents_request",
            json!({"scope":"active","subscribe":{}}),
        )?;
        Ok(())
    }

    fn apply_session(&mut self, kind: &str, payload: Value, cx: &mut Context<Self>) -> Result<()> {
        if let Some(conversation) = &self.conversation {
            conversation.update(cx, |view, cx| view.receive(kind, &payload, cx))?;
        }
        match kind {
            "fetch_agents_response" => {
                let page: AgentPage = serde_json::from_value(payload.clone())?;
                if page.page_info.has_more {
                    let cursor = page
                        .page_info
                        .next_cursor
                        .context("agent continuation cursor missing")?;
                    self.connection
                        .as_ref()
                        .context("host disconnected")?
                        .request(
                            "fetch_agents_request",
                            json!({"scope":"active","page":{"cursor":cursor}}),
                        )?;
                }
            }
            "rpc_error" => anyhow::bail!("{}", payload["error"]),
            "subscribe_terminal_response" => {
                if let Some(terminal) = &self.terminal {
                    terminal.update(cx, |terminal, cx| terminal.bind(&payload, cx));
                }
            }
            "fetch_workspaces_response" => {
                let page: WorkspacePage = serde_json::from_value(payload.clone())?;
                if page.page_info.has_more {
                    let cursor = page
                        .page_info
                        .next_cursor
                        .context("workspace continuation cursor missing")?;
                    self.connection
                        .as_ref()
                        .context("host is disconnected")?
                        .request(
                            "fetch_workspaces_request",
                            json!({"page":{"cursor":cursor}}),
                        )?;
                }
            }
            "terminals_changed" => {
                if let Some(workspace) = payload["workspaceId"].as_str() {
                    self.directory
                        .terminals
                        .retain(|_, terminal| terminal.workspace_id.as_deref() != Some(workspace));
                }
                let changed: paseo_desktop::model::TerminalList =
                    serde_json::from_value(payload.clone())?;
                self.directory.terminals.extend(
                    changed
                        .terminals
                        .into_iter()
                        .map(|terminal| (terminal.id.clone(), terminal)),
                );
            }
            _ => {}
        }
        self.directory.apply(kind, payload)
    }

    fn select_workspace(&mut self, id: String, cx: &mut Context<Self>) {
        self.clear_conversation();
        self.terminal = None;
        self.selected_terminal = None;
        self.selected_workspace = Some(id);
        if let Some(connection) = &self.connection {
            if let Err(error) = connection.request("list_terminals_request", json!({})) {
                self.error = Some(error.to_string());
            }
        }
        cx.notify();
    }

    fn clear_conversation(&mut self) {
        self.selected_agent = None;
        self.conversation = None;
        if let Some(connection) = &self.connection {
            let _ = connection.request(
                "agent.timeline.set_subscription.request",
                json!({"agentIds":[]}),
            );
        }
    }

    fn select_agent(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(agent) = self.directory.agents.get(&id).cloned() else {
            return;
        };
        self.select_workspace(agent.workspace_id.clone().unwrap_or_default(), cx);
        match agent.execution {
            Some(AgentExecution::Terminal { terminal_id }) => {
                self.select_terminal(terminal_id, window, cx)
            }
            Some(AgentExecution::Unknown) => {
                self.error = Some("Update this client to open this agent execution type.".into());
            }
            None => {
                if let Some(connection) = &self.connection {
                    match connection.request(
                        "agent.timeline.set_subscription.request",
                        json!({"agentIds":[id]}),
                    ) {
                        Ok(_) => {
                            let connection = connection.clone();
                            let agent_id = id.clone();
                            let whole_turns = self
                                .info
                                .as_ref()
                                .and_then(|info| info.features.get("wholeTurnTimeline"))
                                == Some(&Value::Bool(true));
                            self.conversation = Some(cx.new(|_| {
                                crate::conversation::Conversation::new(
                                    agent_id,
                                    connection,
                                    whole_turns,
                                )
                            }));
                        }
                        Err(error) => self.error = Some(error.to_string()),
                    }
                }
            }
        }
        self.selected_agent = Some(id);
        cx.notify();
    }

    fn select_terminal(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.clear_conversation();
        let Some(connection) = &self.connection else {
            return;
        };
        let capable = self
            .info
            .as_ref()
            .and_then(|info| info.features.get("terminal-restore-modes"))
            == Some(&Value::Bool(true));
        if !capable {
            self.error = Some("Update this host to use terminal restore modes.".into());
            cx.notify();
            return;
        }
        self.selected_terminal = Some(id.clone());
        let connection = connection.clone();
        self.terminal = Some(cx.new(|cx| TerminalView::new(id, connection, window, cx)));
        cx.notify();
    }
}

impl Render for Paseo {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut projects = self
            .directory
            .projects
            .values()
            .cloned()
            .collect::<Vec<_>>();
        projects.sort_by(|a, b| a.project_display_name.cmp(&b.project_display_name));
        let sidebar = div()
            .id("workspace-sidebar")
            .w(px(280.))
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .bg(rgb(0x121414))
            .child(
                div()
                    .h(px(48.))
                    .px_4()
                    .flex()
                    .items_center()
                    .text_color(rgb(0xd8dee9))
                    .child("Paseo"),
            )
            .child(
                div()
                    .id("project-list")
                    .flex_1()
                    .overflow_y_scroll()
                    .px_2()
                    .children(projects.into_iter().map(|project| {
                        let mut workspaces = self
                            .directory
                            .workspaces
                            .values()
                            .filter(|w| w.project_id == project.project_id)
                            .cloned()
                            .collect::<Vec<_>>();
                        workspaces.sort_by(|a, b| a.name.cmp(&b.name));
                        div()
                            .mb_3()
                            .child(
                                div()
                                    .px_2()
                                    .py_2()
                                    .text_sm()
                                    .text_color(rgb(0x969e9e))
                                    .child(project.project_display_name),
                            )
                            .children(workspaces.into_iter().map(|workspace| {
                                let agents = self
                                    .directory
                                    .agents
                                    .values()
                                    .filter(|a| a.workspace_id.as_deref() == Some(&workspace.id))
                                    .cloned()
                                    .collect::<Vec<_>>();
                                let id = workspace.id.clone();
                                let selected = self.selected_workspace.as_ref() == Some(&id);
                                div()
                                    .id(gpui::SharedString::from(id.clone()))
                                    .rounded_md()
                                    .px_3()
                                    .py_2()
                                    .cursor_pointer()
                                    .when(selected, |row| row.bg(rgb(0x292d2d)))
                                    .hover(|row| row.bg(rgb(0x222626)))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, _, _, cx| {
                                            this.select_workspace(id.clone(), cx)
                                        }),
                                    )
                                    .child(div().text_sm().child(workspace.name))
                                    .child(div().flex().flex_wrap().gap_1().children(
                                        agents.into_iter().map(|agent| {
                                            let id = agent.id.clone();
                                            let title = agent
                                                .title
                                                .clone()
                                                .unwrap_or_else(|| agent.provider.clone());
                                            div()
                                                .id(gpui::SharedString::from(format!("agent-{id}")))
                                                .rounded_md()
                                                .px_2()
                                                .py_1()
                                                .text_xs()
                                                .cursor_pointer()
                                                .bg(rgb(if agent.status == "running" {
                                                    0x304334
                                                } else {
                                                    0x222626
                                                }))
                                                .child(format!(
                                                    "{} · {} · {}",
                                                    agent.provider, agent.status, title
                                                ))
                                                .on_mouse_down(
                                                    MouseButton::Left,
                                                    cx.listener(move |this, _, window, cx| {
                                                        cx.stop_propagation();
                                                        this.select_agent(id.clone(), window, cx);
                                                    }),
                                                )
                                        }),
                                    ))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(rgb(0x808989))
                                            .child(workspace.status),
                                    )
                            }))
                    })),
            )
            .child(
                div()
                    .px_4()
                    .py_2()
                    .text_xs()
                    .text_color(rgb(0x808989))
                    .child(
                        self.info
                            .as_ref()
                            .and_then(|info| info.hostname.clone())
                            .unwrap_or_else(|| "Connecting…".into()),
                    ),
            );
        let title = self
            .selected_terminal
            .as_ref()
            .and_then(|id| self.directory.terminals.get(id))
            .map(|t| t.title.clone().unwrap_or_else(|| t.name.clone()))
            .or_else(|| {
                self.selected_workspace
                    .as_ref()
                    .and_then(|id| self.directory.workspaces.get(id))
                    .map(|w| w.name.clone())
            })
            .unwrap_or_else(|| "Workspaces".into());
        let title = self
            .selected_agent
            .as_ref()
            .and_then(|id| self.directory.agents.get(id))
            .and_then(|agent| agent.title.clone())
            .unwrap_or(title);
        let tabs = self
            .directory
            .terminals
            .values()
            .filter(|terminal| {
                self.selected_workspace.is_some()
                    && terminal.workspace_id.as_ref() == self.selected_workspace.as_ref()
            })
            .cloned()
            .collect::<Vec<_>>();
        let content = div()
            .flex_1()
            .h_full()
            .min_w_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(48.))
                    .flex_shrink_0()
                    .px_4()
                    .flex()
                    .items_center()
                    .border_b_1()
                    .border_color(rgb(0x2b3030))
                    .child(title),
            )
            .when_some(self.error.clone(), |content, error| {
                content.child(
                    div()
                        .px_4()
                        .py_2()
                        .text_sm()
                        .text_color(rgb(0xf07178))
                        .child(error),
                )
            })
            .child(
                div()
                    .h(px(36.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .border_b_1()
                    .border_color(rgb(0x2b3030))
                    .children(tabs.into_iter().map(|terminal| {
                        let id = terminal.id.clone();
                        div()
                            .id(gpui::SharedString::from(id.clone()))
                            .px_3()
                            .py_1()
                            .rounded_sm()
                            .text_sm()
                            .cursor_pointer()
                            .when(self.selected_terminal.as_ref() == Some(&id), |tab| {
                                tab.bg(rgb(0x292d2d))
                            })
                            .hover(|tab| tab.bg(rgb(0x222626)))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, window, cx| {
                                    this.select_terminal(id.clone(), window, cx)
                                }),
                            )
                            .child(terminal.name)
                    })),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .when_some(self.terminal.clone(), |pane, terminal| pane.child(terminal))
                    .when_some(self.conversation.clone(), |pane, conversation| {
                        pane.child(conversation)
                    })
                    .when(
                        self.terminal.is_none() && self.conversation.is_none(),
                        |pane| {
                            pane.child(div().p_6().text_color(rgb(0x808989)).child(
                                if self.selected_workspace.is_some() {
                                    "Select a terminal tab"
                                } else {
                                    "Select a workspace"
                                },
                            ))
                        },
                    ),
            );
        div()
            .size_full()
            .flex()
            .font_family(".SystemUIFont")
            .bg(rgb(0x171919))
            .text_color(rgb(0xd8dee9))
            .text_size(px(14.))
            .child(sidebar)
            .child(content)
    }
}

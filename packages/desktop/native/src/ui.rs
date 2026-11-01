use std::{collections::BTreeMap, sync::Arc};

use anyhow::{Context as _, Result};
use gpui::{Context, Entity, Focusable, MouseButton, Render, Window, div, prelude::*, px, rgb};
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
    composers: BTreeMap<String, Entity<crate::composer_view::Composer>>,
    sidebar_open: bool,
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
            composers: BTreeMap::new(),
            sidebar_open: true,
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
                for composer in self.composers.values() {
                    composer.update(cx, |view, cx| view.disconnected(cx));
                }
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
        if matches!(
            kind,
            "send_agent_message_response" | "cancel_agent_response" | "rpc_error"
        ) {
            let mut handled = false;
            for composer in self.composers.values() {
                handled |= composer.update(cx, |view, cx| view.receive(kind, &payload, cx));
            }
            self.composers.retain(|id, view| {
                self.selected_agent.as_ref() == Some(id) || !view.read(cx).is_disposable()
            });
            if handled {
                return Ok(());
            }
        }
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
        self.directory.apply(kind, payload)?;
        for (id, composer) in &self.composers {
            if let Some(agent) = self.directory.agents.get(id) {
                composer.update(cx, |view, cx| {
                    view.set_running(agent.status == "running", cx)
                });
            }
        }
        Ok(())
    }

    fn select_workspace(&mut self, id: String, cx: &mut Context<Self>) {
        self.clear_conversation(cx);
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

    fn clear_conversation(&mut self, cx: &mut Context<Self>) {
        if let Some(composer) = self
            .selected_agent
            .as_ref()
            .and_then(|id| self.composers.get(id))
        {
            composer.update(cx, |view, _| view.release_layout());
        }
        self.selected_agent = None;
        self.conversation = None;
        self.composers
            .retain(|_, view| !view.read(cx).is_disposable());
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
                            let composer = self
                                .composers
                                .entry(id.clone())
                                .or_insert_with(|| {
                                    cx.new(|cx| {
                                        crate::composer_view::Composer::new(
                                            id.clone(),
                                            connection.clone(),
                                            agent.status == "running",
                                            cx,
                                        )
                                    })
                                })
                                .clone();
                            composer.read(cx).focus_handle(cx).focus(window, cx);
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
                                    composer,
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
        self.clear_conversation(cx);
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use crate::{
            chrome::{Tooltip, icon, provider_icon},
            theme::{colors as c, font_size, radius},
        };
        let mut projects = self
            .directory
            .projects
            .values()
            .cloned()
            .collect::<Vec<_>>();
        projects.sort_by(|a, b| a.project_display_name.cmp(&b.project_display_name));
        let sidebar = div()
            .id("workspace-sidebar")
            .w(px(crate::window_geometry::DEFAULT_SIDEBAR_WIDTH))
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(rgb(c::BORDER))
            .bg(rgb(c::SURFACE_SIDEBAR))
            .when(cfg!(target_os = "macos"), |sidebar| {
                sidebar.child(
                    div()
                        .h(px(36.))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .px_3()
                        .child(icon("panel-left", 16.)),
                )
            })
            .child(
                div()
                    .px_2()
                    .pt(px(if cfg!(target_os = "macos") { 0. } else { 8. }))
                    .pb(px(6.))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .border_b_1()
                    .border_color(rgb(c::BORDER))
                    .children(
                        [
                            ("plus", "New workspace"),
                            ("history", "History"),
                            ("search", "Search"),
                            ("calendar-clock", "Schedules"),
                        ]
                        .map(|(glyph, label)| {
                            div()
                                .h(px(28.))
                                .px_2()
                                .flex()
                                .items_center()
                                .gap_2()
                                .text_color(rgb(c::FOREGROUND_MUTED))
                                .child(icon(glyph, 14.))
                                .child(label)
                        }),
                    ),
            )
            .child(
                div()
                    .id("project-list")
                    .flex_1()
                    .overflow_y_scroll()
                    .px_2()
                    .child(
                        div()
                            .h(px(38.))
                            .px_2()
                            .flex()
                            .items_center()
                            .justify_between()
                            .text_size(px(font_size::SM))
                            .text_color(rgb(c::FOREGROUND_MUTED))
                            .child("Workspaces")
                            .child(icon("sliders-horizontal", 14.)),
                    )
                    .children(projects.into_iter().map(|project| {
                        let mut workspaces = self
                            .directory
                            .workspaces
                            .values()
                            .filter(|w| w.project_id == project.project_id)
                            .cloned()
                            .collect::<Vec<_>>();
                        workspaces.sort_by(|a, b| a.name.cmp(&b.name));
                        let initial = project
                            .project_display_name
                            .chars()
                            .next()
                            .unwrap_or(' ')
                            .to_uppercase()
                            .to_string();
                        div()
                            .mb_3()
                            .child(
                                div()
                                    .h(px(40.))
                                    .mb_1()
                                    .px_2()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .text_color(rgb(c::FOREGROUND_MUTED))
                                    .child(
                                        div()
                                            .size(px(16.))
                                            .rounded(px(radius::BASE))
                                            .bg(rgb(crate::chrome::project_color(&format!(
                                                "host:{}:{}",
                                                self.info
                                                    .as_ref()
                                                    .map(|info| info.server_id.as_str())
                                                    .unwrap_or_default(),
                                                project.project_root_path
                                            ))))
                                            .text_size(px(9.))
                                            .text_color(rgb(c::FOREGROUND))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .child(initial),
                                    )
                                    .child(project.project_display_name),
                            )
                            .children(workspaces.into_iter().map(|workspace| {
                                let mut agents = self
                                    .directory
                                    .agents
                                    .values()
                                    .filter(|a| a.workspace_id.as_deref() == Some(&workspace.id))
                                    .cloned()
                                    .collect::<Vec<_>>();
                                agents.sort_by(|a, b| {
                                    a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id))
                                });
                                let focused = self
                                    .selected_agent
                                    .as_ref()
                                    .and_then(|id| self.directory.agents.get(id))
                                    .filter(|a| a.workspace_id.as_deref() == Some(&workspace.id));
                                let label = focused
                                    .and_then(|a| a.title.clone())
                                    .unwrap_or_else(|| workspace.name.clone());
                                let id = workspace.id.clone();
                                let selected = self.selected_workspace.as_ref() == Some(&id);
                                div()
                                    .id(gpui::SharedString::from(id.clone()))
                                    .rounded(px(radius::LG))
                                    .py_2()
                                    .pl_2()
                                    .pr_3()
                                    .mb(px(2.))
                                    .cursor_pointer()
                                    .when(selected, |row| row.bg(rgb(c::SURFACE_SIDEBAR_SELECTED)))
                                    .hover(move |row| {
                                        row.bg(rgb(if selected {
                                            c::SURFACE_SIDEBAR_SELECTED
                                        } else {
                                            c::SURFACE_SIDEBAR_HOVER
                                        }))
                                    })
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, _, _, cx| {
                                            this.select_workspace(id.clone(), cx)
                                        }),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .gap_2()
                                            .items_start()
                                            .child(
                                                div()
                                                    .w(px(16.))
                                                    .h(px(20.))
                                                    .flex()
                                                    .items_center()
                                                    .justify_center()
                                                    .child(
                                                        div()
                                                            .size(px(6.))
                                                            .rounded_full()
                                                            .bg(gpui::rgba(0x7175744d)),
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .min_w_0()
                                                    .flex_1()
                                                    .line_height(px(20.))
                                                    .text_color(gpui::rgba(0xfafafac2))
                                                    .child(label),
                                            ),
                                    )
                                    .when(!agents.is_empty(), |row| {
                                        row.child(
                                            div()
                                                .ml(px(24.))
                                                .mt_2()
                                                .flex()
                                                .flex_wrap()
                                                .gap_1()
                                                .children(agents.into_iter().map(|agent| {
                                                    let id = agent.id.clone();
                                                    let title = agent
                                                        .title
                                                        .clone()
                                                        .unwrap_or_else(|| agent.provider.clone());
                                                    div()
                                                        .id(gpui::SharedString::from(format!(
                                                            "agent-{id}"
                                                        )))
                                                        .size(px(30.))
                                                        .rounded_full()
                                                        .flex()
                                                        .items_center()
                                                        .justify_center()
                                                        .when(
                                                            self.selected_agent.as_ref()
                                                                == Some(&id),
                                                            |button| {
                                                                button.bg(gpui::rgba(0xffffff14))
                                                            },
                                                        )
                                                        .hover(|button| {
                                                            button.bg(gpui::rgba(0xffffff14))
                                                        })
                                                        .cursor_pointer()
                                                        .tooltip(move |_, cx| {
                                                            cx.new(|_| Tooltip(title.clone()))
                                                                .into()
                                                        })
                                                        .child(crate::chrome::agent_icon(
                                                            &agent.provider,
                                                            agent.status == "running",
                                                        ))
                                                        .on_mouse_down(
                                                            MouseButton::Left,
                                                            cx.listener(
                                                                move |this, _, window, cx| {
                                                                    cx.stop_propagation();
                                                                    this.select_agent(
                                                                        id.clone(),
                                                                        window,
                                                                        cx,
                                                                    )
                                                                },
                                                            ),
                                                        )
                                                })),
                                        )
                                    })
                            }))
                    })),
            )
            .child(
                div()
                    .h(px(41.))
                    .flex_shrink_0()
                    .border_t_1()
                    .border_color(rgb(c::BORDER))
                    .px_2()
                    .py(px(6.))
                    .flex()
                    .items_center()
                    .children(["folder-plus", "gauge", "server"].map(|glyph| {
                        div()
                            .size(px(28.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icon(glyph, 16.))
                    }))
                    .child(div().flex_1())
                    .children(["circle-help", "settings"].map(|glyph| {
                        div()
                            .size(px(28.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icon(glyph, 16.))
                    })),
            );
        let workspace = self
            .selected_workspace
            .as_ref()
            .and_then(|id| self.directory.workspaces.get(id));
        let title = workspace
            .map(|w| w.name.clone())
            .unwrap_or_else(|| "Workspaces".into());
        let subtitle = workspace
            .map(|w| w.project_display_name.clone())
            .unwrap_or_default();
        let tabs = self
            .directory
            .terminals
            .values()
            .filter(|t| {
                self.selected_workspace.is_some()
                    && t.workspace_id.as_ref() == self.selected_workspace.as_ref()
            })
            .cloned()
            .collect::<Vec<_>>();
        let mut agents = self
            .directory
            .agents
            .values()
            .filter(|a| {
                self.selected_workspace.is_some()
                    && a.workspace_id.as_ref() == self.selected_workspace.as_ref()
                    && a.execution.is_none()
            })
            .cloned()
            .collect::<Vec<_>>();
        agents.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        let content = div()
            .flex_1()
            .h_full()
            .min_w_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(36.))
                    .flex_shrink_0()
                    .pl(px(4.))
                    .pr(px(if cfg!(target_os = "macos") { 12. } else { 0. }))
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_b_1()
                    .border_color(rgb(c::BORDER))
                    .when(!cfg!(target_os = "macos"), |header| {
                        header.child(
                            div()
                                .id("toggle-sidebar")
                                .size(px(26.))
                                .rounded(px(radius::MD))
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .hover(|button| button.bg(gpui::rgba(0xffffff14)))
                                .child(icon("panel-left", 16.))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.sidebar_open = !this.sidebar_open;
                                        cx.notify();
                                    }),
                                ),
                        )
                    })
                    .child(div().font_weight(gpui::FontWeight::LIGHT).child(title))
                    .child(div().text_color(rgb(c::FOREGROUND_MUTED)).child(subtitle))
                    .child(icon("ellipsis", 16.))
                    .child(crate::window_chrome::drag_region())
                    .child(
                        div()
                            .size(px(26.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icon("panel-right", 16.)),
                    )
                    .when(
                        !cfg!(target_os = "macos") && !window.is_fullscreen(),
                        |header| header.child(crate::window_chrome::controls(window)),
                    ),
            )
            .when_some(self.error.clone(), |view, error| {
                view.child(
                    div()
                        .px_4()
                        .py_2()
                        .text_color(rgb(c::STATUS_DANGER))
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
                    .px(px(6.))
                    .border_b_1()
                    .border_color(rgb(c::BORDER))
                    .children(agents.into_iter().map(|agent| {
                        let id = agent.id.clone();
                        div()
                            .id(gpui::SharedString::from(format!("tab-agent-{id}")))
                            .w(px(160.))
                            .h(px(28.))
                            .px_2()
                            .rounded(px(radius::MD))
                            .flex()
                            .items_center()
                            .gap_1()
                            .cursor_pointer()
                            .text_color(rgb(if self.selected_agent.as_ref() == Some(&id) {
                                c::FOREGROUND
                            } else {
                                c::FOREGROUND_MUTED
                            }))
                            .when(self.selected_agent.as_ref() == Some(&id), |tab| {
                                tab.bg(rgb(c::SURFACE2))
                            })
                            .hover(|tab| tab.bg(rgb(c::SURFACE2)))
                            .child(provider_icon(&agent.provider, 14.))
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .truncate()
                                    .child(agent.title.unwrap_or(agent.provider)),
                            )
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, window, cx| {
                                    this.select_agent(id.clone(), window, cx)
                                }),
                            )
                    }))
                    .children(tabs.into_iter().map(|terminal| {
                        let id = terminal.id.clone();
                        div()
                            .id(gpui::SharedString::from(id.clone()))
                            .h(px(28.))
                            .px_2()
                            .rounded(px(radius::MD))
                            .flex()
                            .items_center()
                            .gap_1()
                            .cursor_pointer()
                            .when(self.selected_terminal.as_ref() == Some(&id), |tab| {
                                tab.bg(rgb(c::SURFACE2))
                            })
                            .hover(|tab| tab.bg(rgb(c::SURFACE2)))
                            .child(icon("square-terminal", 14.))
                            .child(terminal.name)
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, window, cx| {
                                    this.select_terminal(id.clone(), window, cx)
                                }),
                            )
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
                            pane.child(
                                div()
                                    .size_full()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_color(rgb(c::FOREGROUND_MUTED))
                                    .child(if self.selected_workspace.is_some() {
                                        "Select a tab"
                                    } else {
                                        "Select a workspace"
                                    }),
                            )
                        },
                    ),
            );
        div()
            .size_full()
            .relative()
            .flex()
            .font_family(crate::fonts::ui_family())
            .bg(rgb(c::SURFACE0))
            .text_color(rgb(c::FOREGROUND))
            .text_size(px(font_size::BASE))
            .line_height(px(20.))
            .when(self.sidebar_open, |root| root.child(sidebar))
            .child(content)
            .children(crate::window_chrome::resize_handles(window))
    }
}

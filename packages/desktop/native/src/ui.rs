use std::{collections::BTreeMap, sync::Arc};

use anyhow::{Context as _, Result};
use gpui::{Context, Entity, Focusable, MouseButton, Render, Window, div, prelude::*, px, rgb};
use serde_json::{Value, json};

use crate::terminal_view::TerminalView;
use paseo_desktop::terminal_retention::TerminalCache;
use paseo_desktop::{
    client::{Connection, Event, Target},
    model::{AgentExecution, AgentPage, Directory, FocusedSession, ServerInfo, WorkspacePage},
};

pub struct Paseo {
    connection: Option<Arc<Connection>>,
    info: Option<ServerInfo>,
    provider_icons_request: Option<String>,
    directory_refresh: Option<paseo_desktop::model::DirectoryRefresh>,
    provider_images: BTreeMap<String, Arc<gpui::RenderImage>>,
    workspace_form: Option<Entity<crate::workspace_form::WorkspaceForm>>,
    history: Option<Entity<crate::agent_history::HistoryPanel>>,
    lifecycle_subscriptions: Vec<gpui::Subscription>,
    remembered: paseo_desktop::workspace_lifecycle::RememberedWorkspaces,
    remembered_path: std::path::PathBuf,
    startup_workspace: Option<String>,
    directory: Directory,
    selected_workspace: Option<String>,
    selected_terminal: Option<String>,
    terminal: Option<Entity<TerminalView>>,
    terminals: TerminalCache<Entity<TerminalView>>,
    error: Option<String>,
    selected_agent: Option<String>,
    conversation: Option<Entity<crate::conversation::Conversation>>,
    composers: BTreeMap<String, Entity<crate::composer_view::Composer>>,
    sidebar_open: bool,
    window_visible: bool,
    _visibility: gpui::Subscription,
    _activation: gpui::Subscription,
}

impl Drop for Paseo {
    fn drop(&mut self) {
        if let Some(connection) = &self.connection {
            connection.close();
        }
    }
}

impl Paseo {
    pub fn new(target: Result<Target>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let visibility = cx.observe_window_visibility(window, |this, visibility, _, cx| {
            let visible = visibility.is_visible();
            if visible
                && !this.window_visible
                && this
                    .connection
                    .as_ref()
                    .is_some_and(|connection| connection.connected())
                && let Some(conversation) = &this.conversation
            {
                conversation.update(cx, |view, cx| view.resume(cx));
            }
            if let Some(terminal) = &this.terminal {
                terminal.update(cx, |view, cx| view.visible(visible, cx));
            }
            this.window_visible = visible;
        });
        let activation = cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active()
                && let Some(terminal) = &this.terminal
            {
                if terminal.read(cx).focus_handle(cx).is_focused(window) {
                    terminal.update(cx, |view, cx| view.request_claim(cx));
                }
            }
        });
        let remembered_path = std::env::var_os("PASEO_NATIVE_STATE_DIR")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("XDG_CONFIG_HOME")
                    .map(|p| std::path::PathBuf::from(p).join("paseo"))
            })
            .unwrap_or_else(|| {
                std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                    .map(|p| std::path::PathBuf::from(p).join(".config/paseo"))
                    .unwrap_or_else(std::env::temp_dir)
            })
            .join("native-last-workspace.json");
        let remembered =
            paseo_desktop::workspace_lifecycle::RememberedWorkspaces::load(&remembered_path);
        let remembered_error = remembered
            .as_ref()
            .err()
            .map(|error| format!("Cannot restore workspace selection: {error}"));
        let mut app = Self {
            connection: None,
            info: None,
            provider_icons_request: None,
            directory_refresh: None,
            provider_images: BTreeMap::new(),
            workspace_form: None,
            history: None,
            lifecycle_subscriptions: Vec::new(),
            remembered: remembered.unwrap_or_default(),
            remembered_path,
            startup_workspace: None,
            directory: Directory::default(),
            selected_workspace: None,
            selected_terminal: None,
            terminal: None,
            // Matches the existing workspace pane retention cap.
            terminals: TerminalCache::new(3),
            error: remembered_error,
            selected_agent: None,
            conversation: None,
            composers: BTreeMap::new(),
            sidebar_open: true,
            window_visible: window.is_visible(),
            _visibility: visibility,
            _activation: activation,
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
                // Initial restore only. Reconnect restores the active view without navigating again.
                if self
                    .info
                    .as_ref()
                    .is_none_or(|previous| previous.server_id != info.server_id)
                {
                    self.startup_workspace =
                        self.remembered.get(&info.server_id).map(str::to_owned);
                }
                self.directory.set_host(&info.server_id);
                self.provider_images.clear();
                self.info = Some(info);
                self.provider_icons_request = self.connection.as_ref().and_then(|connection| {
                    connection
                        .request("get_providers_snapshot_request", json!({}))
                        .map_err(|error| {
                            self.error = Some(format!("Cannot load provider icons: {error}"));
                        })
                        .ok()
                });
                if let Some(id) = self.provider_icons_request.clone() {
                    let timer = cx
                        .background_executor()
                        .timer(std::time::Duration::from_secs(30));
                    cx.spawn(async move |this, cx| {
                        timer.await;
                        let _ = this.update(cx, |this, cx| {
                            if this.provider_icons_request.as_ref() == Some(&id) {
                                this.provider_icons_request = None;
                                this.error = Some(
                                    "Timed out loading provider icons. Reconnect to retry.".into(),
                                );
                                cx.notify();
                            }
                        });
                    })
                    .detach();
                }
                self.restore_observations(cx)
            }
            Event::Session { kind, payload } => {
                let timeline_owned = matches!(
                    kind.as_str(),
                    "agent_stream" | "fetch_agent_timeline_response" | "agent.timeline.replacement"
                );
                let result = self.apply_session(&kind, payload, cx);
                if timeline_owned && result.is_ok() && kind != "agent_stream" {
                    return;
                }
                result
            }
            Event::Terminal(frame) => {
                if let Some(terminal) = self
                    .terminals
                    .values()
                    .find(|terminal| terminal.read(cx).slot() == Some(frame.slot))
                    .cloned()
                {
                    terminal.update(cx, |terminal, cx| terminal.receive(frame, cx));
                }
                return;
            }
            Event::Disconnected(error) => {
                if let Some(form) = &self.workspace_form {
                    form.update(cx, |form, cx| form.disconnected(cx));
                }
                if let Some(history) = &self.history {
                    history.update(cx, |panel, cx| panel.disconnected(cx));
                }
                self.directory.clear_provider_icons();
                self.provider_images.clear();
                self.provider_icons_request = None;
                for terminal in self.terminals.values() {
                    terminal.update(cx, |view, cx| view.disconnected(cx));
                }
                if let Some(conversation) = &self.conversation {
                    conversation.update(cx, |view, cx| view.disconnected(cx));
                }
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

    fn restore_observations(&mut self, cx: &mut Context<Self>) -> Result<()> {
        self.error = None;
        self.directory_refresh = Some(paseo_desktop::model::DirectoryRefresh::default());
        self.load_directory()?;
        let connection = self.connection.as_ref().context("host disconnected")?;
        if let Some(workspace) = self
            .selected_workspace
            .as_ref()
            .and_then(|id| self.directory.workspaces.get(id))
        {
            let cwd = workspace
                .workspace_directory
                .as_deref()
                .unwrap_or(&workspace.project_root_path);
            connection.subscribe(
                "terminals",
                "subscribe_terminals_request",
                json!({"cwd":cwd,"workspaceId":workspace.id}),
            )?;
        }
        for terminal in self.terminals.values() {
            terminal.update(cx, |view, cx| view.reconnect(cx));
        }
        for composer in self.composers.values() {
            composer.update(cx, |view, cx| view.reconnected(cx));
        }
        if let Some(agent) = &self.selected_agent {
            connection.subscribe(
                "timeline",
                "agent.timeline.set_subscription.request",
                json!({"agentIds":[agent]}),
            )?;
            if let Some(conversation) = &self.conversation {
                conversation.update(cx, |view, cx| view.resume(cx));
            }
        }
        Ok(())
    }

    fn load_directory(&self) -> Result<()> {
        anyhow::ensure!(
            self.info.as_ref().is_some_and(
                |info| info.features.get("ownedSubscriptions") == Some(&Value::Bool(true))
            ),
            "Update the host to use native desktop subscriptions."
        );
        let connection = self.connection.as_ref().context("host is disconnected")?;
        connection.request("project.list.request", json!({}))?;
        connection.subscribe(
            "workspaces",
            "fetch_workspaces_request",
            json!({"subscribe":{}}),
        )?;
        connection.subscribe(
            "agents",
            "fetch_agents_request",
            json!({"scope":"active","subscribe":{}}),
        )?;
        Ok(())
    }

    fn apply_session(
        &mut self,
        kind: &str,
        mut payload: Value,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        if kind == "fetch_agents_response" {
            let _: AgentPage = serde_json::from_value(payload.clone())?;
        } else if kind == "fetch_workspaces_response" {
            let _: WorkspacePage = serde_json::from_value(payload.clone())?;
        }
        if let Some(refresh) = &mut self.directory_refresh {
            refresh.apply(kind, &mut payload, &mut self.directory);
            if refresh.complete() {
                self.directory_refresh = None;
            }
        }
        if let Some(form) = &self.workspace_form
            && form.read(cx).owns(&payload)
        {
            form.update(cx, |form, cx| form.receive(kind, &payload, cx));
            return Ok(());
        }
        if kind == "rpc_error"
            && let Some(form) = &self.workspace_form
            && form.read(cx).state.dismisses_error(&payload)
        {
            return Ok(());
        }
        if let Some(history) = &self.history {
            if history.read(cx).owns(&payload) {
                history.update(cx, |panel, cx| panel.receive(kind, &payload, cx));
                return Ok(());
            }
            if kind == "rpc_error" && history.read(cx).dismisses_error(&payload) {
                return Ok(());
            }
        }
        if self
            .provider_icons_request
            .as_deref()
            .is_some_and(|id| payload["requestId"].as_str() == Some(id))
        {
            self.provider_icons_request = None;
            if kind == "rpc_error" {
                anyhow::bail!("Cannot load provider icons: {}", payload["error"]);
            }
            if kind == "get_providers_snapshot_response" {
                self.directory.replace_provider_icons(&payload)?;
                self.provider_images =
                    crate::chrome::render_provider_icons(&self.directory, &payload, cx)?;
                return Ok(());
            }
        }
        if kind == "get_providers_snapshot_response" {
            return Ok(());
        }
        if kind == "providers_snapshot_update" {
            self.directory.replace_provider_icons(&payload)?;
            self.provider_images =
                crate::chrome::render_provider_icons(&self.directory, &payload, cx)?;
            return Ok(());
        }

        if kind == "terminal.input.write_raw.response" || kind == "rpc_error" {
            if let Some(terminal) = self
                .terminals
                .values()
                .find(|terminal| terminal.read(cx).owns_raw_request(&payload))
                .cloned()
            {
                terminal.update(cx, |view, cx| view.receive_raw(&payload, cx));
                return Ok(());
            }
        }
        if kind == "rpc_error"
            && payload["requestType"].as_str() == Some("terminal.input.write_raw.request")
        {
            // Late input failures must not appear in another workspace after expiry/eviction.
            return Ok(());
        }
        if kind == "rpc_error" {
            if let Some(terminal) = self
                .terminals
                .values()
                .find(|terminal| terminal.read(cx).owns_request(&payload))
                .cloned()
            {
                terminal.update(cx, |view, cx| view.bind(&payload, cx));
                return Ok(());
            }
        }
        if matches!(
            kind,
            "send_agent_message_response"
                | "file.upload.response"
                | "cancel_agent_response"
                | "fetch_agent_response"
                | "list_provider_models_response"
                | "list_provider_modes_response"
                | "set_agent_model_response"
                | "set_agent_thinking_response"
                | "set_agent_mode_response"
                | "rpc_error"
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
        if kind == "agent_status" {
            for composer in self.composers.values() {
                composer.update(cx, |view, cx| view.status(&payload, cx));
            }
        }
        if let Some(conversation) = &self.conversation {
            let owns_error = kind == "rpc_error" && conversation.read(cx).owns_request(&payload);
            conversation.update(cx, |view, cx| view.receive(kind, &payload, cx))?;
            if owns_error {
                return Ok(());
            }
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
                if let Some(terminal) = self
                    .terminals
                    .values()
                    .find(|terminal| terminal.read(cx).owns_request(&payload))
                    .cloned()
                {
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
        let terminal_inventory = kind == "list_terminals_response"
            || (kind == "terminals_changed"
                && payload["workspaceId"].as_str() == self.selected_workspace.as_deref());
        let directory_ready =
            kind == "fetch_workspaces_response" && payload["pageInfo"]["hasMore"] == false;
        self.directory.apply(kind, payload)?;
        let available = self.startup_workspace.as_ref().is_some_and(|id| {
            self.directory
                .workspaces
                .get(id)
                .is_some_and(|workspace| workspace.available())
        });
        match paseo_desktop::workspace_lifecycle::restore_selection(
            self.startup_workspace.as_deref(),
            directory_ready,
            available,
        ) {
            paseo_desktop::workspace_lifecycle::SelectionRestore::Select(id) => {
                self.startup_workspace = None;
                self.select_workspace(id, cx);
            }
            paseo_desktop::workspace_lifecycle::SelectionRestore::Unavailable(_) => {
                self.startup_workspace = None;
                self.error = Some(
                    "The remembered workspace is unavailable. Use Recover workspace to inspect it."
                        .into(),
                );
            }
            _ => {}
        }
        if terminal_inventory {
            self.terminals
                .retain(|id| self.directory.terminals.contains_key(id));
            if self
                .selected_terminal
                .as_ref()
                .is_some_and(|id| !self.directory.terminals.contains_key(id))
            {
                self.terminal = None;
                self.selected_terminal = None;
            }
        }
        for (id, composer) in &self.composers {
            if let Some(agent) = self.directory.agents.get(id) {
                composer.update(cx, |view, cx| view.set_running(agent.is_working(), cx));
            }
        }
        Ok(())
    }

    fn remember_workspace(&mut self) -> Result<()> {
        if let (Some(host), Some(id)) = (&self.info, &self.selected_workspace) {
            self.remembered.select(&host.server_id, id);
            self.remembered.save(&self.remembered_path)?;
        }
        Ok(())
    }
    fn open_workspace_form(
        &mut self,
        mode: crate::workspace_form::FormMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(connection) = self.connection.clone() else {
            self.error = Some("Host is disconnected.".into());
            cx.notify();
            return;
        };
        let feature = match mode {
            crate::workspace_form::FormMode::NewWorkspace => Some("workspaceMultiplicity"),
            crate::workspace_form::FormMode::Recover { .. } => Some("workspaceRecovery"),
            _ => None,
        };
        if feature.is_some_and(|feature| {
            self.info
                .as_ref()
                .and_then(|info| info.features.get(feature))
                != Some(&Value::Bool(true))
        }) {
            self.error = Some("Update this host to use this workspace action.".into());
            cx.notify();
            return;
        }
        if self.workspace_form.as_ref().is_some_and(|form| {
            matches!(
                form.read(cx).state.phase,
                paseo_desktop::workspace_lifecycle::Phase::Pending { .. }
            )
        }) {
            return;
        }
        let projects = self.directory.projects.values().cloned().collect();
        let path = self
            .selected_workspace
            .as_ref()
            .and_then(|id| self.directory.workspaces.get(id))
            .map(|w| w.project_root_path.clone())
            .unwrap_or_default();
        let form = cx.new(|cx| {
            crate::workspace_form::WorkspaceForm::new(mode, connection, projects, &path, cx)
        });
        self.lifecycle_subscriptions.clear();
        self.lifecycle_subscriptions.push(
            cx.subscribe_in(
                &form,
                window,
                |this, _, event: &crate::workspace_form::Completed, _, cx| {
                    use paseo_desktop::workspace_lifecycle::Operation;
                    match &event.operation {
                        Operation::CreateWorkspace => {
                            match serde_json::from_value::<paseo_desktop::model::Workspace>(
                                event.payload["workspace"].clone(),
                            ) {
                                Ok(workspace) => {
                                    let id = workspace.id.clone();
                                    this.directory.workspaces.insert(id.clone(), workspace);
                                    this.select_workspace(id, cx);
                                }
                                Err(_) => {
                                    this.error = Some(
                                        "The host created a workspace this client could not read."
                                            .into(),
                                    );
                                }
                            }
                        }
                        Operation::Restore(id) => {
                            this.startup_workspace = Some(id.clone());
                            if let Some(connection) = &this.connection {
                                if let Err(error) =
                                    connection.request("fetch_workspaces_request", json!({}))
                                {
                                    this.error = Some(error.to_string());
                                }
                            }
                        }
                        Operation::Archive(id) => {
                            this.directory.workspaces.remove(id);
                            if let Some(host) = &this.info {
                                this.remembered.forget(&host.server_id, id);
                                if let Err(error) = this.remembered.save(&this.remembered_path) {
                                    this.error = Some(error.to_string());
                                }
                            }
                            if this.selected_workspace.as_ref() == Some(id) {
                                this.clear_conversation(cx);
                                this.hide_terminal(cx);
                                this.terminals.clear();
                                this.selected_workspace = None;
                            }
                        }
                        Operation::AddProject | Operation::CreateDirectory => {
                            if let Ok(project) =
                                serde_json::from_value::<paseo_desktop::model::Project>(
                                    event.payload["project"].clone(),
                                )
                            {
                                this.directory
                                    .projects
                                    .insert(project.project_id.clone(), project);
                            }
                        }
                        Operation::Inspect(_) => {}
                    }
                    // The app navigates away as soon as a create or restore is
                    // confirmed; keeping the form open left a dead Complete page.
                    if matches!(
                        event.operation,
                        Operation::CreateWorkspace | Operation::Restore(_)
                    ) {
                        this.workspace_form = None;
                        this.lifecycle_subscriptions.clear();
                    }
                    cx.notify();
                },
            ),
        );
        self.lifecycle_subscriptions.push(cx.subscribe_in(
            &form,
            window,
            |this, _, _: &crate::workspace_form::Dismiss, _, cx| {
                this.workspace_form = None;
                this.lifecycle_subscriptions.clear();
                cx.notify();
            },
        ));
        form.update(cx, |form, cx| {
            form.focus(window, cx);
            form.begin(cx);
        });
        self.workspace_form = Some(form);
        cx.notify();
    }

    fn open_history(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.history.is_some() {
            return;
        }
        let Some(connection) = self.connection.clone() else {
            self.error = Some("Host is disconnected.".into());
            cx.notify();
            return;
        };
        self.workspace_form = None;
        self.lifecycle_subscriptions.clear();
        let panel = cx.new(|cx| crate::agent_history::HistoryPanel::new(connection, cx));
        self.lifecycle_subscriptions.push(cx.subscribe_in(
            &panel,
            window,
            |this, _, event: &crate::agent_history::OpenAgent, window, cx| {
                this.open_agent_from_history(event.0.clone(), window, cx);
            },
        ));
        self.lifecycle_subscriptions.push(cx.subscribe_in(
            &panel,
            window,
            |this, _, _: &crate::agent_history::Dismiss, _, cx| {
                this.history = None;
                this.lifecycle_subscriptions.clear();
                cx.notify();
            },
        ));
        self.history = Some(panel);
        cx.notify();
    }

    /// History navigation follows the agent's workspaceId: an available
    /// workspace opens the agent (or its workspace), anything else goes
    /// through workspace recovery instead of inventing a workspace.
    fn open_agent_from_history(
        &mut self,
        agent: paseo_desktop::model::Agent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.history = None;
        self.lifecycle_subscriptions.clear();
        let workspace_id = agent.workspace_id.clone();
        let archived = agent.archived_at.is_some();
        let id = agent.id.clone();
        match workspace_id {
            Some(workspace_id)
                if self
                    .directory
                    .workspaces
                    .get(&workspace_id)
                    .is_some_and(|workspace| workspace.available()) =>
            {
                if !archived && self.directory.agent_target(&id).is_some() {
                    self.directory.agents.insert(id.clone(), agent);
                    self.select_agent(id, window, cx);
                } else {
                    self.select_workspace(workspace_id, cx);
                }
            }
            Some(workspace_id) => {
                self.open_workspace_form(
                    crate::workspace_form::FormMode::Recover { workspace_id },
                    window,
                    cx,
                );
            }
            None => {
                self.error = Some("This history entry has no workspace.".into());
                cx.notify();
            }
        }
    }

    fn hide_terminal(&mut self, cx: &mut Context<Self>) {
        if let Some(terminal) = self.terminal.take() {
            terminal.update(cx, |view, cx| view.visible(false, cx));
        }
        self.selected_terminal = None;
    }
    fn select_workspace(&mut self, id: String, cx: &mut Context<Self>) {
        if self.selected_workspace.as_ref() == Some(&id) {
            return;
        }
        for terminal in self.terminals.values() {
            if let Err(error) = terminal.update(cx, |view, _| view.prepare_detach()) {
                self.error = Some(error.to_string());
                cx.notify();
                return;
            }
        }
        self.error = None;
        self.clear_conversation(cx);
        self.hide_terminal(cx);
        self.terminals.clear();
        self.selected_workspace = Some(id);
        if let Some(connection) = &self.connection {
            let workspace = self
                .selected_workspace
                .as_ref()
                .and_then(|id| self.directory.workspaces.get(id));
            let result = workspace
                .context("Workspace is no longer available")
                .and_then(|workspace| {
                    let cwd = workspace
                        .workspace_directory
                        .as_deref()
                        .unwrap_or(&workspace.project_root_path);
                    connection.subscribe(
                        "terminals",
                        "subscribe_terminals_request",
                        json!({"cwd":cwd,"workspaceId":workspace.id}),
                    )
                });
            if let Err(error) = result {
                self.error = Some(error.to_string());
            }
        }
        if let Err(error) = self.remember_workspace() {
            self.error = Some(error.to_string());
        }
        cx.notify();
    }

    fn clear_conversation(&mut self, cx: &mut Context<Self>) {
        let had_conversation = self.conversation.is_some();
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
        if had_conversation && let Some(connection) = &self.connection {
            connection.unsubscribe("timeline");
        }
    }

    fn select_agent(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(agent) = self.directory.agents.get(&id).cloned() else {
            return;
        };
        let Some(workspace) = agent.workspace_id.clone() else {
            self.error = Some("This agent has no workspace.".into());
            cx.notify();
            return;
        };
        if self.directory.agent_target(&id).is_none() {
            self.error = Some("This agent execution is not available in this client.".into());
            cx.notify();
            return;
        }
        self.select_workspace(workspace.clone(), cx);
        if self.selected_workspace.as_ref() != Some(&workspace) {
            return;
        }
        match agent.execution {
            Some(AgentExecution::Terminal { terminal_id, .. }) => {
                self.select_terminal(terminal_id.clone(), window, cx);

                return;
            }
            Some(AgentExecution::Unknown) => {
                self.error = Some("Update this client to open this agent execution type.".into());
            }
            None => {
                self.clear_conversation(cx);
                self.hide_terminal(cx);
                if let Some(connection) = &self.connection {
                    match connection.subscribe(
                        "timeline",
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
                                            agent.is_working(),
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
                            self.conversation = Some(cx.new(|cx| {
                                crate::conversation::Conversation::new(
                                    agent_id,
                                    connection,
                                    whole_turns,
                                    composer,
                                    cx,
                                )
                            }));
                        }
                        Err(error) => self.error = Some(error.to_string()),
                    }
                }
            }
        }
        if self.conversation.is_some() {
            self.directory
                .remember_focus(&workspace, FocusedSession::Agent(id.clone()));
            self.selected_agent = Some(id);
        }
        cx.notify();
    }

    fn select_terminal(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.directory.terminal_workspace(&id) != self.selected_workspace.as_deref()
            || self.selected_workspace.is_none()
        {
            self.error = Some("This terminal is no longer available in this workspace.".into());
            cx.notify();
            return;
        }
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
        let connection = connection.clone();
        let protected = self
            .terminals
            .iter()
            .filter(|(_, view)| !view.read(cx).can_release())
            .map(|(id, _)| id.clone())
            .collect();
        let view = self.terminals.open(&id, &protected, || {
            cx.new(|cx| TerminalView::new(id.clone(), connection, window, cx))
        });
        match view {
            Ok(view)=>{
                self.clear_conversation(cx);self.hide_terminal(cx);
                if let Some(workspace) = &self.selected_workspace { self.directory.remember_focus(workspace, FocusedSession::Terminal(id.clone())); }
                self.selected_agent = self.directory.terminal_agent(&id).map(|agent|agent.id.clone());
                self.selected_terminal=Some(id);self.error=None;
                view.update(cx,|view,cx|view.activate(window,cx));self.terminal=Some(view);
            },
            Err(_)=>self.error=Some("Wait for the other terminals to finish opening or sending input, then select this tab again.".into()),
        }
        cx.notify();
    }
}

impl Render for Paseo {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use crate::{
            chrome::{Tooltip, icon},
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
                                .when(glyph=="plus",|row|row.cursor_pointer().on_mouse_down(MouseButton::Left,cx.listener(|this,_,window,cx|this.open_workspace_form(crate::workspace_form::FormMode::NewWorkspace,window,cx))))
                                .when(glyph=="history",|row|row.cursor_pointer().on_mouse_down(MouseButton::Left,cx.listener(|this,_,window,cx|this.open_history(window,cx))))
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
                            .filter(|w| w.project_id == project.project_id && w.available())
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
                                let agents = self.directory.sidebar_agents(&workspace.id).into_iter().cloned().collect::<Vec<_>>();
                                let label = self.directory.workspace_title(&workspace.id, &workspace.name);
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
                                                    let title = agent.display_title().to_owned();
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
                                                            self.selected_workspace.as_ref() == Some(&workspace.id) && self.directory.agent_target(&id).is_some_and(|target| match target { FocusedSession::Agent(id) => self.selected_agent.as_ref()==Some(&id) && self.conversation.is_some(), FocusedSession::Terminal(id) => self.selected_terminal.as_ref()==Some(&id) }),
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
                                                            agent.sidebar_icon(),
                                                            agent.sidebar_status(),
                                                            self.provider_images.get(agent.sidebar_icon()),
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
                            .when(glyph=="folder-plus",|row|row.cursor_pointer().on_mouse_down(MouseButton::Left,cx.listener(|this,_,window,cx|this.open_workspace_form(crate::workspace_form::FormMode::AddProject,window,cx))))
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
            .map(|w| self.directory.workspace_title(&w.id, &w.name))
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
                    && a.archived_at.is_none()
                    && !a.internal
            })
            .cloned()
            .collect::<Vec<_>>();
        agents.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        let lifecycle_actions = div()
            .flex()
            .gap_2()
            .child(
                div()
                    .id("new-directory")
                    .h(px(28.))
                    .px_2()
                    .cursor_pointer()
                    .child("New directory")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| {
                            this.open_workspace_form(
                                crate::workspace_form::FormMode::CreateDirectory,
                                window,
                                cx,
                            )
                        }),
                    ),
            )
            .child(
                div()
                    .id("recover-workspace")
                    .h(px(28.))
                    .px_2()
                    .cursor_pointer()
                    .child("Recover workspace")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| {
                            this.open_workspace_form(
                                crate::workspace_form::FormMode::Recover {
                                    workspace_id: String::new(),
                                },
                                window,
                                cx,
                            )
                        }),
                    ),
            )
            .when(self.selected_workspace.is_some(), |row| {
                row.child(
                    div()
                        .id("archive-workspace")
                        .h(px(28.))
                        .px_2()
                        .cursor_pointer()
                        .child("Archive workspace")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, window, cx| {
                                if let Some(id) = this.selected_workspace.clone() {
                                    let name = this
                                        .directory
                                        .workspaces
                                        .get(&id)
                                        .map(|w| w.name.clone())
                                        .unwrap_or_else(|| id.clone());
                                    this.open_workspace_form(
                                        crate::workspace_form::FormMode::Archive { id, name },
                                        window,
                                        cx,
                                    );
                                }
                            }),
                        ),
                )
            });
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
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .font_weight(gpui::FontWeight::LIGHT)
                                    .child(title),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(rgb(c::FOREGROUND_MUTED))
                                    .child(subtitle),
                            ),
                    )
                    .child(lifecycle_actions)
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
                            .child(crate::chrome::provider_icon_data(
                                &agent.provider,
                                14.,
                                self.provider_images.get(&agent.provider),
                            ))
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .truncate()
                                    .child(agent.display_title().to_owned()),
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
                            .child(self.directory.terminal_title(&terminal))
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
            .when_some(self.workspace_form.clone(), |root, form| {
                root.child(
                    // Occlude the whole surface: hit-testing stops here so the
                    // covered sidebar, tabs, and titlebar never see a click.
                    div().absolute().inset_0().occlude().child(form),
                )
            })
            .when_some(self.history.clone(), |root, history| {
                root.child(div().absolute().inset_0().occlude().child(history))
            })
            .children(crate::window_chrome::resize_handles(window))
    }
}

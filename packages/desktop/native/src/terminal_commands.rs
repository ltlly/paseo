use super::{NewBrowser, NewTab, NewTerminal, Pane, Paseo};
use crate::launcher_presentation as p;
use crate::{
    action_menu::{ActionMenu, Chosen, Item, anchored_menu},
    chrome,
    text_field::{FieldEvent, TextField},
};
use gpui::{
    Bounds, ClipboardItem, Context, Entity, FocusHandle, Focusable, MouseButton, Pixels, Point,
    SharedString, Task, Window, div, prelude::*, px, rgb, rgba, size,
};
use paseo_desktop::{
    model::CloseAgentTabPolicy,
    terminal_lifecycle::{self as model, Destination, Focus, Operation, Outcome, Profile},
    workspace_tab_layout::{
        BulkCloseGroups, ContextMenuEntry, ContextMenuTarget,
        build_bulk_close_confirmation_message, build_context_menu_entries,
    },
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub(super) const PREFIX: &str = "launcher:";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum RenameTabKind {
    Agent,
    Terminal,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum RenameResponseOutcome {
    Completed,
    Failed(String),
    Ignored,
}

fn resolve_rename_response(kind: &str, payload: &Value) -> RenameResponseOutcome {
    match kind {
        "terminal.rename.response" => {
            if payload["success"].as_bool() == Some(false) {
                RenameResponseOutcome::Failed(
                    payload["error"]
                        .as_str()
                        .unwrap_or("Failed to rename terminal")
                        .to_owned(),
                )
            } else {
                RenameResponseOutcome::Completed
            }
        }
        "update_agent_response" => {
            if payload["accepted"].as_bool() == Some(false) || payload["error"].as_str().is_some() {
                RenameResponseOutcome::Failed(
                    payload["error"]
                        .as_str()
                        .unwrap_or("Unable to save")
                        .to_owned(),
                )
            } else {
                RenameResponseOutcome::Completed
            }
        }
        "rpc_error" => RenameResponseOutcome::Failed(
            payload["error"]
                .as_str()
                .unwrap_or("Unable to save")
                .to_owned(),
        ),
        _ => RenameResponseOutcome::Ignored,
    }
}

fn resolve_tab_rename_target(
    directory: &paseo_desktop::model::Directory,
    id: &str,
) -> Option<(RenameTabKind, String, String)> {
    if let Some(agent) = directory.terminal_agent(id) {
        return Some((
            RenameTabKind::Agent,
            agent.id.clone(),
            agent.title.clone().unwrap_or_default(),
        ));
    }
    if let Some(terminal) = directory.terminals.get(id) {
        return Some((
            RenameTabKind::Terminal,
            terminal.id.clone(),
            terminal
                .title
                .clone()
                .unwrap_or_else(|| terminal.name.clone()),
        ));
    }
    if let Some(agent) = directory.agents.get(id) {
        return Some((
            RenameTabKind::Agent,
            agent.id.clone(),
            agent.title.clone().unwrap_or_default(),
        ));
    }
    None
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CloseItemsResolution {
    archived_agents: Vec<(String, String)>,
    failed_agents: Vec<String>,
    killed_terminals: Vec<String>,
    failed_terminals: Vec<String>,
}

impl CloseItemsResolution {
    fn failure_message(&self) -> Option<String> {
        match (self.failed_agents.len(), self.failed_terminals.len()) {
            (0, 0) => None,
            (agents, 0) => Some(format!("Failed to archive {agents} agent(s).")),
            (0, terminals) => Some(format!("Failed to close {terminals} terminal(s).")),
            (agents, terminals) => Some(format!(
                "Failed to archive {agents} agent(s) and close {terminals} terminal(s)."
            )),
        }
    }
}

fn resolve_close_items_response(
    payload: &Value,
    requested_agents: &[String],
    requested_terminals: &[String],
) -> CloseItemsResolution {
    let mut archived_agents = Vec::new();
    for entry in payload["agents"].as_array().into_iter().flatten() {
        if let Some(id) = entry["agentId"].as_str() {
            let archived_at = entry["archivedAt"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| "archived".into());
            archived_agents.push((id.to_owned(), archived_at));
        }
    }
    let failed_agents = requested_agents
        .iter()
        .filter(|id| !archived_agents.iter().any(|(archived, _)| archived == *id))
        .cloned()
        .collect();
    let mut killed_terminals = Vec::new();
    for entry in payload["terminals"].as_array().into_iter().flatten() {
        if entry["success"].as_bool() == Some(true)
            && let Some(id) = entry["terminalId"].as_str()
        {
            killed_terminals.push(id.to_owned());
        }
    }
    let failed_terminals = requested_terminals
        .iter()
        .filter(|id| !killed_terminals.iter().any(|killed| killed == *id))
        .cloned()
        .collect();
    CloseItemsResolution {
        archived_agents,
        failed_agents,
        killed_terminals,
        failed_terminals,
    }
}

fn retain_pane_tabs(requested: &[String], pane_tabs: &[String]) -> Vec<String> {
    requested
        .iter()
        .filter(|id| pane_tabs.iter().any(|tab| tab == *id))
        .cloned()
        .collect()
}

fn resolve_built_in_launcher_ids(
    host: paseo_desktop::panel_host::Host,
    surface: paseo_desktop::panel_host::LaunchSurface,
    existing: &[&str],
    git: bool,
    pr: bool,
) -> Vec<&'static str> {
    let mut ids: Vec<_> = [
        ("agent", "draft", true),
        ("terminal", "terminal", true),
        ("changes", "changes_tree", git),
        ("diff", "working_diff", git),
        ("files", "files", true),
        ("browser", "browser", true),
        ("pullRequest", "pull_request", pr),
    ]
    .into_iter()
    .filter(|(_, kind, visible)| {
        *visible && paseo_desktop::panel_host::can_launch_on_surface(kind, host, existing, surface)
    })
    .map(|(id, _, _)| id)
    .collect();
    let order = match host {
        paseo_desktop::panel_host::Host::Explorer => p::SUPPORTING_ORDER,
        paseo_desktop::panel_host::Host::Main => p::PRIMARY_ORDER,
    };
    ids.sort_by_key(|id| {
        order
            .iter()
            .position(|candidate| candidate == id)
            .unwrap_or(usize::MAX)
    });
    ids
}

struct TabContextMenu {
    workspace: String,
    pane: String,
    tab_id: String,
    position: Point<Pixels>,
    view: Entity<ActionMenu>,
    _subscription: gpui::Subscription,
    previous: Option<FocusHandle>,
}

struct TabRenameModal {
    workspace: String,
    tab_id: String,
    target_id: String,
    kind: RenameTabKind,
    initial_title: String,
    field: Entity<TextField>,
    pending_request_id: Option<String>,
    submitted_title: Option<String>,
    error: Option<String>,
    _subscription: gpui::Subscription,
    previous: Option<FocusHandle>,
}

enum PendingAgentTabOp {
    Archive {
        agent_id: String,
    },
    Reload {
        agent_id: String,
    },
    CloseItems {
        workspace: String,
        agent_ids: Vec<String>,
        terminal_ids: Vec<String>,
    },
}

struct Menu {
    view: Entity<ActionMenu>,
    _subscription: gpui::Subscription,
    destination: Destination,
    previous: Option<FocusHandle>,
}
struct SplitMenu {
    view: Entity<ActionMenu>,
    _subscription: gpui::Subscription,
    workspace: String,
    pane: String,
    previous: Option<FocusHandle>,
}
#[derive(Default)]
pub(super) struct Commands {
    pub state: model::State,
    pub profiles_subscribed: bool,
    menu: Option<Menu>,
    split_menu: Option<SplitMenu>,
    tab_context_menu: Option<TabContextMenu>,
    tab_rename: Option<TabRenameModal>,
    pending_ops: BTreeMap<String, PendingAgentTabOp>,
    prompt: Option<Task<()>>,
    pub anchors: BTreeMap<String, Bounds<Pixels>>,
    pub split_anchors: BTreeMap<String, Bounds<Pixels>>,
    active: BTreeMap<(String, String, Option<String>), String>,
}
impl Commands {
    pub fn retain(
        &mut self,
        workspace: Option<&str>,
        layout: &paseo_desktop::split_layout::SplitLayout,
    ) {
        let panes = layout.all_panes();
        self.anchors
            .retain(|id, _| panes.iter().any(|pane| &pane.id == id));
        self.split_anchors
            .retain(|id, _| panes.iter().any(|pane| &pane.id == id));
        if self.menu.as_ref().is_some_and(|menu| {
            workspace != Some(menu.destination.workspace.as_str())
                || !layout.is_pane_rendered(&menu.destination.pane)
        }) {
            self.menu = None;
        }
        if self.split_menu.as_ref().is_some_and(|menu| {
            workspace != Some(menu.workspace.as_str()) || !layout.is_main_pane_rendered(&menu.pane)
        }) {
            self.split_menu = None;
        }
        if self.tab_context_menu.as_ref().is_some_and(|menu| {
            workspace != Some(menu.workspace.as_str())
                || !layout.is_pane_rendered(&menu.pane)
                || !layout
                    .pane(&menu.pane)
                    .is_some_and(|pane| pane.tabs.contains(&menu.tab_id))
        }) {
            self.tab_context_menu = None;
        }
        if self.tab_rename.as_ref().is_some_and(|modal| {
            workspace != Some(modal.workspace.as_str())
                || !panes.iter().any(|pane| pane.tabs.contains(&modal.tab_id))
        }) {
            self.tab_rename = None;
        }
        self.active.retain(|(owner, pane, tab), _| {
            workspace == Some(owner.as_str())
                && layout.pane(pane).is_some_and(|pane| {
                    tab.as_ref()
                        .map_or(pane.focused_tab.is_none(), |tab| pane.tabs.contains(tab))
                })
        });
    }
    pub(super) fn disconnected(&mut self) {
        self.state.disconnected();
        self.profiles_subscribed = false;
        self.pending_ops.clear();
        if let Some(modal) = &mut self.tab_rename
            && modal.pending_request_id.take().is_some()
        {
            modal.submitted_title = None;
            modal.error = Some("The host disconnected.".into());
        }
    }
    pub fn modal(&self) -> bool {
        self.prompt.is_some()
            || self.menu.is_some()
            || self.split_menu.is_some()
            || self.tab_context_menu.is_some()
            || self.tab_rename.is_some()
    }
}
#[derive(Clone)]
enum Selection {
    Terminal(Option<Profile>),
    Browser,
    Tools(crate::workspace_tools::DockTab),
    Unavailable,
}
struct Row {
    id: String,
    label: String,
    icon: &'static str,
    selection: Selection,
    enabled: bool,
    profile: Option<Option<String>>,
    shortcut: Option<String>,
}

impl Paseo {
    pub(super) fn launch_text(&self, key: &'static str) -> String {
        paseo_desktop::locales::translate(
            paseo_desktop::locales::active_locale(self.shared_settings.get().language),
            paseo_desktop::locales::Tx::key(key),
        )
    }
    fn terminal_focus_ticket(&self) -> Option<Focus> {
        let workspace = self.selected_workspace.as_ref()?;
        let pane = self.splits.get(workspace)?.active_pane()?;
        Some(Focus {
            workspace: workspace.clone(),
            pane: pane.id.clone(),
            tab: pane.focused_tab.clone(),
        })
    }
    fn terminal_destination(&self, pane: &str, replace: Option<String>) -> Option<Destination> {
        let workspace = self
            .directory
            .workspaces
            .get(self.selected_workspace.as_ref()?)?;
        Some(Destination {
            workspace: workspace.id.clone(),
            cwd: workspace
                .workspace_directory
                .as_ref()
                .unwrap_or(&workspace.project_root_path)
                .clone(),
            pane: pane.into(),
            replace,
            focus: self.terminal_focus_ticket(),
        })
    }
    fn track_terminal_request(&mut self, id: String, operation: Operation, cx: &mut Context<Self>) {
        self.terminal_commands.state.begin(id.clone(), operation);
        let generation = self.host_generation;
        let timer = cx
            .background_executor()
            .timer(std::time::Duration::from_secs(150));
        cx.spawn(async move |this, cx| {
            timer.await;
            let _ = this.update(cx, |this, cx| {
                if this.host_generation == generation {
                    this.terminal_commands.state.expire(
                        &id,
                        "The host did not answer. Check its terminals before trying again.",
                    );
                    cx.notify();
                }
            });
        })
        .detach();
    }
    pub(super) fn load_terminal_profiles(&mut self, force: bool, cx: &mut Context<Self>) {
        if !self.terminal_commands.state.room() || (self.terminal_commands.state.demanded && !force)
        {
            return;
        }
        let Some(connection) = &self.connection else {
            return;
        };
        if !self.terminal_commands.profiles_subscribed {
            match connection.subscribe(
                "terminal-profiles-events",
                "session.events.set_subscription.request",
                json!({"events":["status.daemon_config_changed"]}),
            ) {
                Ok(_) => self.terminal_commands.profiles_subscribed = true,
                Err(error) => {
                    self.terminal_commands.state.config_error = Some(error.to_string());
                    return;
                }
            }
        }
        match connection.request("get_daemon_config_request", json!({})) {
            Ok(id) => {
                let generation = self.terminal_commands.state.next_config();
                self.track_terminal_request(id, Operation::Config(generation), cx);
            }
            Err(error) => self.terminal_commands.state.config_error = Some(error.to_string()),
        }
    }
    fn create_terminal(
        &mut self,
        destination: Destination,
        profile: Option<&Profile>,
        cx: &mut Context<Self>,
    ) {
        if self.terminal_commands.state.creating() {
            return;
        }
        let result = if !self.terminal_commands.state.room() {
            Err(anyhow::anyhow!(
                "Too many terminal operations are awaiting a host response."
            ))
        } else {
            self.connection
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("The host is disconnected."))
                .and_then(|connection| {
                    connection.request("create_terminal_request", destination.fields(profile))
                })
        };
        match result {
            Ok(id) => self.track_terminal_request(id, Operation::Create(destination), cx),
            Err(error) => self
                .terminal_commands
                .state
                .fail(&destination.workspace, error.to_string()),
        }
        cx.notify();
    }
    pub(super) fn receive_terminal_command(
        &mut self,
        kind: &str,
        payload: &Value,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<bool> {
        if let Some(request_id) = payload["requestId"].as_str() {
            if self
                .terminal_commands
                .tab_rename
                .as_ref()
                .is_some_and(|modal| modal.pending_request_id.as_deref() == Some(request_id))
            {
                let rename_outcome = resolve_rename_response(kind, payload);
                let mut completed: Option<(RenameTabKind, String, String, Option<FocusHandle>)> =
                    None;
                let mut rename_error: Option<String> = None;
                match rename_outcome {
                    RenameResponseOutcome::Failed(error) => {
                        rename_error = Some(error);
                    }
                    RenameResponseOutcome::Completed => {
                        if let Some(modal) = self.terminal_commands.tab_rename.take() {
                            let next = modal
                                .submitted_title
                                .unwrap_or_else(|| modal.field.read(cx).text().trim().to_owned());
                            completed = Some((modal.kind, modal.target_id, next, modal.previous));
                        }
                    }
                    RenameResponseOutcome::Ignored => {}
                }
                if let Some(error) = rename_error {
                    let field = self.terminal_commands.tab_rename.as_mut().map(|modal| {
                        modal.pending_request_id = None;
                        modal.submitted_title = None;
                        modal.error = Some(error);
                        modal.field.clone()
                    });
                    if let Some(handle) = self.window_handle
                        && let Some(field) = field
                    {
                        let generation = self.host_generation;
                        cx.defer(move |cx| {
                            if let Some(typed) = handle.downcast::<Paseo>() {
                                let _ = typed.update(cx, |this, window, cx| {
                                    if this.host_generation == generation {
                                        field.read(cx).focus_handle(cx).focus(window, cx);
                                        cx.notify();
                                    }
                                });
                            }
                        });
                    }
                    cx.notify();
                    return Ok(true);
                }
                if let Some((tab_kind, target_id, next_title, previous)) = completed {
                    match tab_kind {
                        RenameTabKind::Terminal => {
                            if let Some(terminal) = self.directory.terminals.get_mut(&target_id) {
                                terminal.title = Some(next_title.clone());
                                terminal.name = next_title;
                            }
                            self.command_center_snapshot_dirty = true;
                        }
                        RenameTabKind::Agent => {
                            let _ = self.directory.apply(kind, payload.clone());
                            if let Some(agent) = self.directory.agents.get_mut(&target_id) {
                                agent.title = Some(next_title);
                            }
                            self.command_center_snapshot_dirty = true;
                        }
                    }
                    if let Some(handle) = self.window_handle {
                        let generation = self.host_generation;
                        cx.defer(move |cx| {
                            if let Some(typed) = handle.downcast::<Paseo>() {
                                let _ = typed.update(cx, |this, window, cx| {
                                    if this.host_generation == generation {
                                        if let Some(previous) = previous {
                                            previous.focus(window, cx);
                                        }
                                        cx.notify();
                                    }
                                });
                            }
                        });
                    }
                    cx.notify();
                    return Ok(true);
                }
            }
            if let Some(op) = self.terminal_commands.pending_ops.remove(request_id) {
                match (op, kind) {
                    (
                        PendingAgentTabOp::Archive { agent_id },
                        "agent_archived" | "archive_agent_response",
                    ) => {
                        if let Some(agent) = self.directory.agents.get_mut(&agent_id) {
                            agent.archived_at = payload["archivedAt"]
                                .as_str()
                                .map(str::to_owned)
                                .or_else(|| Some("archived".into()));
                        }
                        self.command_center_snapshot_dirty = true;
                        cx.notify();
                        return Ok(true);
                    }
                    (PendingAgentTabOp::Reload { agent_id }, "status")
                        if payload["status"].as_str() == Some("agent_refreshed") =>
                    {
                        if let Some(conversation) = self.conversations.get(&agent_id) {
                            conversation.update(cx, |view, cx| view.reload_after_agent_refresh(cx));
                        }
                        if let Some(composer) = self.composers.get(&agent_id) {
                            composer.update(cx, |view, cx| view.reload_agent_config(cx));
                        }
                        cx.notify();
                        return Ok(true);
                    }
                    (
                        PendingAgentTabOp::CloseItems {
                            workspace,
                            agent_ids,
                            terminal_ids,
                        },
                        "close_items_response",
                    ) => {
                        let resolution =
                            resolve_close_items_response(payload, &agent_ids, &terminal_ids);
                        for (id, archived_at) in &resolution.archived_agents {
                            if let Some(agent) = self.directory.agents.get_mut(id) {
                                agent.archived_at = Some(archived_at.clone());
                            }
                        }
                        for id in &resolution.killed_terminals {
                            self.directory.terminals.remove(id);
                            self.terminals.retain(|tid| tid != id);
                        }
                        if let Some(error) = resolution.failure_message() {
                            self.error = Some(error);
                        }
                        self.command_center_snapshot_dirty = true;
                        let closed_agents = resolution
                            .archived_agents
                            .into_iter()
                            .map(|(id, _)| id)
                            .collect::<Vec<_>>();
                        let closed_terminals = resolution.killed_terminals;
                        if (!closed_agents.is_empty() || !closed_terminals.is_empty())
                            && let Some(handle) = self.window_handle
                        {
                            let generation = self.host_generation;
                            cx.defer(move |cx| {
                                if let Some(typed) = handle.downcast::<Paseo>() {
                                    let _ = typed.update(cx, move |this, window, cx| {
                                        if this.host_generation != generation {
                                            return;
                                        }
                                        let mut shown = false;
                                        for id in &closed_agents {
                                            shown |= this
                                                .selected_splits()
                                                .is_some_and(|layout| layout.tab_is_focused(id));
                                            if let Some(view) = this.composers.get(id) {
                                                view.update(cx, |view, _| view.release_layout());
                                            }
                                            this.conversations.remove(id);
                                            this.composers.retain(|key, view| {
                                                key != id || !view.read(cx).is_disposable()
                                            });
                                            for layout in this.splits.values_mut() {
                                                layout.prune_tabs(|tab| tab != id);
                                            }
                                            if this.selected_agent.as_ref() == Some(id) {
                                                this.selected_agent = None;
                                            }
                                        }
                                        if !closed_agents.is_empty() {
                                            this.sync_timeline_subscription();
                                        }
                                        for id in &closed_terminals {
                                            shown |= this
                                                .selected_splits()
                                                .is_some_and(|layout| layout.tab_is_focused(id));
                                            if this.selected_terminal.as_ref() == Some(id) {
                                                this.selected_terminal = None;
                                                this.terminal = None;
                                            }
                                            for layout in this.splits.values_mut() {
                                                layout.prune_tabs(|tab| tab != id);
                                            }
                                        }
                                        this.persist_workspace_splits(&workspace);
                                        if this.selected_workspace.as_deref()
                                            == Some(workspace.as_str())
                                        {
                                            if shown {
                                                this.selected_terminal = None;
                                                this.sync_active_tab(window, cx);
                                            }
                                            this.apply_terminal_visibility(cx);
                                        }
                                        cx.notify();
                                    });
                                }
                            });
                        }
                        cx.notify();
                        return Ok(true);
                    }
                    (op, "rpc_error") => {
                        let message = payload["error"]
                            .as_str()
                            .unwrap_or("Host operation failed")
                            .to_owned();
                        let ws = match op {
                            PendingAgentTabOp::CloseItems { workspace, .. } => Some(workspace),
                            PendingAgentTabOp::Archive { agent_id }
                            | PendingAgentTabOp::Reload { agent_id } => self
                                .directory
                                .agents
                                .get(&agent_id)
                                .and_then(|agent| agent.workspace_id.clone())
                                .or_else(|| self.selected_workspace.clone()),
                        };
                        if let Some(workspace) = ws {
                            self.terminal_commands
                                .state
                                .fail(&workspace, message.clone());
                        }
                        self.error = Some(message);
                        cx.notify();
                        return Ok(true);
                    }
                    (op, _) => {
                        self.terminal_commands
                            .pending_ops
                            .insert(request_id.to_owned(), op);
                    }
                }
            }
        }
        let outcome = if kind == "status"
            && payload["status"] == "daemon_config_changed"
            && self.terminal_commands.state.demanded
        {
            self.terminal_commands
                .state
                .config_changed(&payload["config"]);
            Outcome::Profiles
        } else {
            self.terminal_commands.state.receive(kind, payload)?
        };
        if matches!(outcome, Outcome::Ignored) {
            return Ok(false);
        }
        let generation = self.host_generation;
        if let Some(handle) = self.window_handle {
            cx.defer(move |cx| {
                if let Some(typed) = handle.downcast::<Paseo>() {
                    let _ = typed.update(cx, move |this, window, cx| {
                        if this.host_generation != generation {
                            return;
                        }
                        match outcome {
                            Outcome::Created {
                                terminal,
                                destination,
                                late,
                            } => {
                                match serde_json::from_value::<paseo_desktop::model::Terminal>(
                                    terminal,
                                ) {
                                    Ok(mut terminal) => {
                                        terminal.workspace_id = Some(destination.workspace.clone());
                                        let id = terminal.id.clone();
                                        // Inventory may already carry a newer OSC title.
                                        this.directory
                                            .terminals
                                            .entry(id.clone())
                                            .or_insert(terminal);
                                        let select = !late
                                            && this.terminal_focus_ticket() == destination.focus;
                                        if let Some(layout) =
                                            this.splits.get_mut(&destination.workspace)
                                        {
                                            if let Err(error) = model::place_created(
                                                layout,
                                                &destination,
                                                &id,
                                                select,
                                            ) {
                                                this.terminal_commands.state.fail(
                                                    &destination.workspace,
                                                    error.to_string(),
                                                );
                                            }
                                        }
                                        this.persist_workspace_splits(&destination.workspace);
                                        this.command_center_snapshot_dirty = true;
                                        if select
                                            && this.selected_workspace.as_deref()
                                                == Some(&destination.workspace)
                                        {
                                            this.sync_active_tab(window, cx);
                                        }
                                    }
                                    Err(_) => this.terminal_commands.state.fail(
                                        &destination.workspace,
                                        "The host returned an invalid terminal.".into(),
                                    ),
                                }
                            }
                            Outcome::Killed {
                                workspace,
                                terminal,
                            } => {
                                // Inventory can remove the session pointer before this ack.
                                // The pane still owns focus until its tab is pruned.
                                let shown = this
                                    .selected_workspace
                                    .as_ref()
                                    .and_then(|workspace| this.splits.get(workspace))
                                    .is_some_and(|layout| layout.tab_is_focused(&terminal));
                                this.directory.terminals.remove(&terminal);
                                this.terminals.retain(|id| id != terminal);
                                let mut changed = Vec::new();
                                for (workspace, layout) in &mut this.splits {
                                    if layout.find_pane_containing_tab(&terminal).is_some() {
                                        layout.prune_tabs(|id| id != terminal);
                                        changed.push(workspace.clone());
                                    }
                                }
                                for workspace in changed {
                                    this.persist_workspace_splits(&workspace);
                                }
                                this.command_center_snapshot_dirty = true;
                                if shown && this.selected_workspace.as_deref() == Some(&workspace) {
                                    this.selected_terminal = None;
                                    this.terminal = None;
                                    this.sync_active_tab(window, cx);
                                }
                            }
                            Outcome::Profiles => {
                                if let Some(menu) = this.terminal_commands.menu.take() {
                                    this.show_new_tab_menu(
                                        menu.destination,
                                        menu.previous,
                                        window,
                                        cx,
                                    );
                                }
                            }
                            _ => {}
                        }
                        cx.notify();
                    });
                }
            });
        }
        cx.notify();
        Ok(true)
    }
    pub(super) fn persist_workspace_splits(&mut self, workspace: &str) {
        if let Some(host) = self.info.as_ref().map(|info| info.server_id.clone())
            && let Some(layout) = self.splits.get(workspace).cloned()
        {
            if let Err(error) = self.split_store.save(&host, workspace, layout) {
                self.terminal_commands
                    .state
                    .fail(workspace, format!("Cannot save split layout: {error:#}"));
            }
        }
    }
    fn launcher_rows(
        &self,
        pane_id: &str,
        surface: paseo_desktop::panel_host::LaunchSurface,
        cx: &gpui::App,
    ) -> Vec<Row> {
        let (git, pr) = self
            .tools
            .as_ref()
            .map_or((false, false), |tools| tools.read(cx).launch_flags());
        let host = paseo_desktop::panel_host::Host::for_pane(pane_id);
        let existing: Vec<_> = self
            .selected_workspace
            .as_ref()
            .and_then(|id| self.splits.get(id))
            .and_then(|layout| layout.pane(pane_id))
            .map(|pane| {
                pane.tabs
                    .iter()
                    .filter_map(|tab| self.pane_tab_kind(tab).map(|kind| kind.panel_kind()))
                    .collect()
            })
            .unwrap_or_default();
        let mut rows = vec![];
        for id in resolve_built_in_launcher_ids(host, surface, &existing, git, pr) {
            let (key, icon, selection) = match id {
                "agent" => (
                    "workspace.tabs.fallback.agent",
                    "square-pen",
                    Selection::Unavailable,
                ),
                "terminal" => (
                    "workspace.tabs.fallback.terminal",
                    "square-terminal",
                    Selection::Terminal(None),
                ),
                "changes" => (
                    "panels.diff.changesLabel",
                    "files",
                    Selection::Tools(crate::workspace_tools::DockTab::Changes),
                ),
                "diff" => ("panels.diff.diffLabel", "file-diff", Selection::Unavailable),
                "files" => (
                    "panels.files.label",
                    "files",
                    Selection::Tools(crate::workspace_tools::DockTab::Files),
                ),
                "browser" => (
                    "workspace.tabs.fallback.browser",
                    "globe",
                    Selection::Browser,
                ),
                "pullRequest" => (
                    "panels.pullRequest.label",
                    "git-pull-request",
                    Selection::Unavailable,
                ),
                _ => continue,
            };
            let enabled = !matches!(selection, Selection::Unavailable)
                && (!matches!(selection, Selection::Terminal(_))
                    || !self.terminal_commands.state.creating());
            rows.push(Row {
                id: id.into(),
                label: self.launch_text(key),
                icon,
                selection,
                enabled,
                profile: None,
                shortcut: match id {
                    "agent" => Some(
                        if cfg!(target_os = "macos") {
                            "⇧⌘A"
                        } else {
                            "Ctrl+Shift+A"
                        }
                        .into(),
                    ),
                    "terminal" => Some(
                        if cfg!(target_os = "macos") {
                            "⇧⌘T"
                        } else {
                            "Ctrl+Shift+T"
                        }
                        .into(),
                    ),
                    "browser" => Some(
                        if cfg!(target_os = "macos") {
                            "⇧⌘B"
                        } else {
                            "Ctrl+Shift+B"
                        }
                        .into(),
                    ),
                    _ => None,
                },
            });
        }
        for profile in self.terminal_commands.state.profiles() {
            rows.push(Row {
                id: format!("terminal-profile:{}", profile.id),
                label: profile.name.clone(),
                icon: "",
                selection: Selection::Terminal(Some(profile.clone())),
                enabled: !self.terminal_commands.state.creating(),
                profile: Some(profile.icon_key().map(str::to_owned)),
                shortcut: None,
            });
        }
        rows
    }
    pub(super) fn new_tab_menu_owns_pane(&self, pane: &str) -> bool {
        self.terminal_commands.menu.as_ref().is_some_and(|menu| {
            self.selected_workspace.as_ref() == Some(&menu.destination.workspace)
                && menu.destination.pane == pane
        })
    }
    pub(super) fn split_menu_owns_pane(&self, pane: &str) -> bool {
        self.terminal_commands
            .split_menu
            .as_ref()
            .is_some_and(|menu| {
                self.selected_workspace.as_ref() == Some(&menu.workspace) && menu.pane == pane
            })
    }
    pub(super) fn close_new_tab_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(menu) = self.terminal_commands.menu.take() {
            if let Some(focus) = menu.previous {
                focus.focus(window, cx);
            }
            cx.notify();
        }
    }
    pub(super) fn close_split_pane_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(menu) = self.terminal_commands.split_menu.take() {
            if let Some(focus) = menu.previous {
                focus.focus(window, cx);
            }
            cx.notify();
        }
    }
    pub(super) fn close_tab_context_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(menu) = self.terminal_commands.tab_context_menu.take() {
            if let Some(focus) = menu.previous {
                focus.focus(window, cx);
            }
            cx.notify();
        }
    }
    pub(super) fn cancel_tab_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .terminal_commands
            .tab_rename
            .as_ref()
            .is_some_and(|modal| modal.pending_request_id.is_some())
        {
            return;
        }
        if let Some(modal) = self.terminal_commands.tab_rename.take() {
            if let Some(focus) = modal.previous {
                focus.focus(window, cx);
            }
            cx.notify();
        }
    }
    pub(super) fn open_split_pane_menu(
        &mut self,
        pane: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.split_menu_owns_pane(&pane) {
            self.close_split_pane_menu(window, cx);
            return;
        }
        self.close_new_tab_menu(window, cx);
        self.close_split_pane_menu(window, cx);
        self.close_tab_context_menu(window, cx);
        self.close_workspace_menu(window, cx);
        self.scripts_menu = None;
        self.focus_split_pane(pane.clone(), window, cx);
        let Some(workspace) = self.selected_workspace.clone() else {
            return;
        };
        if pane == paseo_desktop::explorer_layout::EXPLORER_PANE_ID
            || !self
                .splits
                .get(&workspace)
                .is_some_and(|layout| layout.is_main_pane_rendered(&pane))
        {
            return;
        }
        if !self.terminal_commands.split_anchors.contains_key(&pane) {
            return;
        }
        let previous = window.focused(cx);
        let mut split_right = Item::new(
            "split-right",
            self.launch_text("workspace.tabs.actions.splitRight"),
            "columns-2",
        );
        let mut split_down = Item::new(
            "split-down",
            self.launch_text("workspace.tabs.actions.splitDown"),
            "rows-2",
        );
        if cfg!(target_os = "macos") {
            split_right.shortcut = Some("⌘\\".into());
            split_down.shortcut = Some("⇧⌘\\".into());
        }
        let items = vec![split_right, split_down];
        let view =
            cx.new(|cx| ActionMenu::new(items, self.shared_settings.clone(), cx).pane_menu());
        let target_workspace = workspace.clone();
        let target_pane = pane.clone();
        let subscription =
            cx.subscribe_in(&view, window, move |this, _, event: &Chosen, window, cx| {
                this.close_split_pane_menu(window, cx);
                if this.selected_workspace.as_deref() != Some(&target_workspace) {
                    return;
                }
                match event.0.as_deref() {
                    Some("split-right") => this.split_pane_at(
                        &target_pane,
                        paseo_desktop::split_layout::SplitPosition::Right,
                        window,
                        cx,
                    ),
                    Some("split-down") => this.split_pane_at(
                        &target_pane,
                        paseo_desktop::split_layout::SplitPosition::Bottom,
                        window,
                        cx,
                    ),
                    _ => {}
                }
            });
        view.read(cx).focus_handle(cx).focus(window, cx);
        self.terminal_commands.split_menu = Some(SplitMenu {
            view,
            _subscription: subscription,
            workspace,
            pane,
            previous,
        });
        cx.notify();
    }
    pub(super) fn open_new_tab_menu(
        &mut self,
        pane: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_new_tab_menu(window, cx);
        self.close_split_pane_menu(window, cx);
        self.close_tab_context_menu(window, cx);
        self.close_workspace_menu(window, cx);
        self.scripts_menu = None;
        self.focus_split_pane(pane.clone(), window, cx);
        let Some(destination) = self.terminal_destination(&pane, None) else {
            return;
        };
        if !self.terminal_commands.anchors.contains_key(&pane) {
            return;
        }
        let previous = window.focused(cx);
        self.show_new_tab_menu(destination, previous, window, cx);
        self.load_terminal_profiles(false, cx);
    }
    fn show_new_tab_menu(
        &mut self,
        destination: Destination,
        previous: Option<FocusHandle>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut items = vec![];
        let mut selections = BTreeMap::new();
        let mut profiles = false;
        for row in self.launcher_rows(
            &destination.pane,
            paseo_desktop::panel_host::LaunchSurface::Menu,
            cx,
        ) {
            if row.profile.is_some() && !profiles {
                profiles = true;
                let mut sep = Item::new("separator", "", "");
                sep.separator = true;
                sep.enabled = false;
                items.push(sep);
                let mut heading = Item::new(
                    "profiles",
                    self.launch_text("workspace.tabs.actions.terminalProfilesMenu"),
                    "",
                );
                heading.heading = true;
                heading.enabled = false;
                items.push(heading);
            }
            let mut item = Item::new(&row.id, row.label, row.icon);
            item.enabled = row.enabled;
            item.profile_icon = row
                .profile
                .filter(|key| key.as_ref().is_some_and(|key| !key.is_empty()));
            item.shortcut = row.shortcut;
            selections.insert(row.id, row.selection);
            items.push(item);
        }
        if profiles {
            let mut sep = Item::new("separator-end", "", "");
            sep.separator = true;
            sep.enabled = false;
            items.push(sep);
            let mut edit = Item::new(
                "edit-terminal-profiles",
                self.launch_text("workspace.tabs.actions.editTerminalProfiles"),
                "",
            );
            edit.enabled = true;
            items.push(edit);
        }
        let view = cx.new(|cx| ActionMenu::new(items, self.shared_settings.clone(), cx).launcher());
        let target = destination.clone();
        let subscription =
            cx.subscribe_in(&view, window, move |this, _, event: &Chosen, window, cx| {
                this.close_new_tab_menu(window, cx);
                if event.0.as_deref() == Some("edit-terminal-profiles") {
                    this.open_pane(Pane::HostTerminals, window, cx);
                    return;
                }
                if let Some(selection) = event.0.as_ref().and_then(|id| selections.get(id)) {
                    this.choose_launcher(target.clone(), selection.clone(), window, cx);
                }
            });
        view.read(cx).focus_handle(cx).focus(window, cx);
        self.terminal_commands.menu = Some(Menu {
            view,
            _subscription: subscription,
            destination,
            previous,
        });
        cx.notify();
    }
    fn choose_launcher(
        &mut self,
        destination: Destination,
        selection: Selection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match selection {
            Selection::Terminal(profile) => self.create_terminal(destination, profile.as_ref(), cx),
            Selection::Browser => {
                if !paseo_desktop::panel_host::supports(
                    "browser",
                    paseo_desktop::panel_host::Host::for_pane(&destination.pane),
                ) {
                    return;
                }
                if self.selected_workspace.as_deref() != Some(&destination.workspace) {
                    return;
                }
                if let Some(layout) = self.splits.get_mut(&destination.workspace) {
                    layout.focus_pane(&destination.pane);
                }
                let browser_count = self.browsers.len();
                self.open_browser_tab(window, cx);
                if self.browsers.len() > browser_count
                    && let Some(replace) = destination.replace
                {
                    if let Some(layout) = self.splits.get_mut(&destination.workspace) {
                        layout.prune_tabs(|id| id != replace);
                    }
                    self.persist_splits();
                }
            }
            Selection::Tools(tab) => {
                if self.selected_workspace.as_deref() != Some(&destination.workspace) {
                    return;
                }
                let id = if tab == crate::workspace_tools::DockTab::Files {
                    paseo_desktop::explorer_layout::FILES_TAB_ID
                } else {
                    paseo_desktop::explorer_layout::CHANGES_TAB_ID
                };
                if let Some(layout) = self.splits.get_mut(&destination.workspace) {
                    layout.focus_pane(&destination.pane);
                    if layout.attach_tab(id).is_none() {
                        return;
                    }
                    if let Some(replace) = &destination.replace {
                        layout.prune_tabs(|tab| tab != replace);
                    }
                }
                self.persist_splits();
                self.sync_active_tab(window, cx);
                cx.notify();
            }
            Selection::Unavailable => {}
        }
    }
    pub(super) fn on_new_tab(&mut self, _: &NewTab, window: &mut Window, cx: &mut Context<Self>) {
        if self.pane.is_some() || self.command_center_open(cx) || self.modal_open() {
            return;
        }
        if !cfg!(target_os = "macos") && self.terminal_view_focused(window, cx) {
            cx.propagate();
            return;
        }
        let id = format!("{PREFIX}{}", uuid::Uuid::new_v4());
        if self.selected_splits().is_some_and(|layout| {
            if let Some(main) = layout.focused_pane_id.clone() {
                layout.focus_pane(&main);
            }
            layout.attach_tab(&id).is_some()
        }) {
            self.close_pane(cx);
            self.persist_splits();
            self.sync_active_tab(window, cx);
            self.focus.focus(window, cx);
            self.load_terminal_profiles(false, cx);
            cx.notify();
        }
    }
    pub(super) fn on_new_terminal(
        &mut self,
        _: &NewTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pane.is_some() || self.command_center_open(cx) || self.modal_open() {
            return;
        }
        if !cfg!(target_os = "macos") && self.terminal_view_focused(window, cx) {
            cx.propagate();
            return;
        }
        let Some(focus) = self.terminal_focus_ticket() else {
            return;
        };
        let Some(target) = self
            .splits
            .get(&focus.workspace)
            .and_then(|layout| layout.focused_pane())
            .cloned()
        else {
            return;
        };
        let replace = target
            .focused_tab
            .as_ref()
            .filter(|id| id.starts_with(PREFIX))
            .cloned();
        if let Some(destination) = self.terminal_destination(&target.id, replace) {
            self.create_terminal(destination, None, cx);
        }
    }
    pub(super) fn on_new_browser(
        &mut self,
        _: &NewBrowser,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pane.is_some() || self.command_center_open(cx) || self.modal_open() {
            return;
        }
        if !cfg!(target_os = "macos") && self.terminal_view_focused(window, cx) {
            cx.propagate();
            return;
        }
        let Some(focus) = self.terminal_focus_ticket() else {
            return;
        };
        let Some(target) = self
            .splits
            .get(&focus.workspace)
            .and_then(|layout| layout.focused_pane())
            .cloned()
        else {
            return;
        };
        let replace = target
            .focused_tab
            .as_ref()
            .filter(|id| id.starts_with(PREFIX))
            .cloned();
        if let Some(destination) = self.terminal_destination(&target.id, replace) {
            self.choose_launcher(destination, Selection::Browser, window, cx);
        }
    }
    pub(super) fn close_launcher_tab(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let shown = self
            .selected_splits()
            .is_some_and(|layout| layout.tab_is_focused(id));
        if let Some(layout) = self.selected_splits() {
            layout.prune_tabs(|tab| tab != id);
        }
        self.persist_splits();
        if shown {
            self.sync_active_tab(window, cx);
        }
        cx.notify();
    }
    pub(super) fn confirm_close_terminal(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.modal_open() || self.terminal_commands.state.killing(id) {
            return;
        }
        let Some(terminal) = self.directory.terminals.get(id) else {
            return;
        };
        let Some(workspace) = terminal.workspace_id.clone() else {
            return;
        };
        let id = id.to_owned();
        let generation = self.host_generation;
        let previous = window.focused(cx);
        let answer = crate::native_confirmation::close_terminal(
            window,
            cx,
            self.launch_text("workspace.tabs.confirmations.closeTerminalTitle"),
            self.launch_text("workspace.tabs.confirmations.closeTerminalMessage"),
            self.launch_text("common.actions.cancel"),
            self.launch_text("common.actions.close"),
        );
        match answer {
            Err(error) => {
                self.terminal_commands
                    .state
                    .fail(&workspace, error.to_string());
                cx.notify();
            }
            Ok(answer) => {
                self.focus.focus(window, cx);
                let handle = self.window_handle;
                self.terminal_commands.prompt = Some(cx.spawn(async move |this, cx| {
                    let confirmed = answer.await;
                    let _ = this.update(cx, |this, cx| {
                        if this.host_generation != generation {
                            return;
                        }
                        this.terminal_commands.prompt = None;
                        if confirmed && this.directory.terminals.contains_key(&id) {
                            let result = if !this.terminal_commands.state.room() {
                                Err(anyhow::anyhow!(
                                    "Too many terminal operations are awaiting a host response."
                                ))
                            } else {
                                this.connection
                                    .as_ref()
                                    .ok_or_else(|| anyhow::anyhow!("The host is disconnected."))
                                    .and_then(|c| {
                                        c.request("kill_terminal_request", json!({"terminalId":id}))
                                    })
                            };
                            match result {
                                Ok(request) => this.track_terminal_request(
                                    request,
                                    Operation::Kill {
                                        workspace: workspace.clone(),
                                        terminal: id,
                                    },
                                    cx,
                                ),
                                Err(error) => this
                                    .terminal_commands
                                    .state
                                    .fail(&workspace, error.to_string()),
                            }
                        }
                        if let Some(handle) = handle {
                            cx.defer(move |cx| {
                                if let Some(typed) = handle.downcast::<Paseo>() {
                                    let _ = typed.update(cx, |this, window, cx| {
                                        if this.host_generation == generation {
                                            if let Some(previous) = previous {
                                                previous.focus(window, cx);
                                            }
                                            window.activate_window();
                                            cx.notify();
                                        }
                                    });
                                }
                            });
                        }
                        cx.notify();
                    });
                }));
                cx.notify();
            }
        }
    }
    pub(super) fn open_tab_context_menu(
        &mut self,
        pane: String,
        tab_id: String,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.terminal_commands.prompt.is_some() || self.terminal_commands.tab_rename.is_some() {
            return;
        }
        self.close_new_tab_menu(window, cx);
        self.close_split_pane_menu(window, cx);
        self.close_tab_context_menu(window, cx);
        self.close_workspace_menu(window, cx);
        self.scripts_menu = None;
        let Some(workspace) = self.selected_workspace.clone() else {
            return;
        };
        let Some(tabs) = self
            .splits
            .get(&workspace)
            .and_then(|layout| layout.pane(&pane))
            .map(|pane| pane.tabs.clone())
        else {
            return;
        };
        let Some(index) = tabs.iter().position(|id| id == &tab_id) else {
            return;
        };
        let target = match self.pane_tab_kind(&tab_id) {
            Some(super::PaneTabKind::Agent) => ContextMenuTarget::Agent(tab_id.clone()),
            Some(super::PaneTabKind::Terminal) => ContextMenuTarget::Terminal(tab_id.clone()),
            Some(super::PaneTabKind::File) => {
                let Some(key) = self.file_tab_key(&tab_id) else {
                    return;
                };
                ContextMenuTarget::File(key.path)
            }
            Some(_) => ContextMenuTarget::Passive,
            None => return,
        };
        let locale = paseo_desktop::locales::active_locale(self.shared_settings.get().language);
        let items = build_context_menu_entries(locale, &target, index, tabs.len())
            .into_iter()
            .enumerate()
            .map(|(index, entry)| match entry {
                ContextMenuEntry::Separator => {
                    let mut item = Item::new(&format!("separator-{index}"), "", "");
                    item.separator = true;
                    item.enabled = false;
                    item
                }
                ContextMenuEntry::Item {
                    id,
                    label,
                    icon,
                    hint,
                    tooltip,
                    enabled,
                } => {
                    let mut item = Item::new(id, label, icon);
                    item.hint = hint;
                    item.tooltip = tooltip;
                    item.enabled = enabled;
                    item
                }
            })
            .collect();
        let previous = window.focused(cx);
        let view =
            cx.new(|cx| ActionMenu::new(items, self.shared_settings.clone(), cx).context_menu());
        let owner = workspace.clone();
        let selected = tab_id.clone();
        let parent = pane.clone();
        let subscription =
            cx.subscribe_in(&view, window, move |this, _, event: &Chosen, window, cx| {
                this.close_tab_context_menu(window, cx);
                if this.selected_workspace.as_ref() != Some(&owner) {
                    return;
                }
                if let Some(action) = &event.0 {
                    this.handle_tab_context_menu_action(&parent, &selected, action, window, cx);
                }
            });
        view.read(cx).focus_handle(cx).focus(window, cx);
        self.terminal_commands.tab_context_menu = Some(TabContextMenu {
            workspace,
            pane,
            tab_id,
            position,
            view,
            _subscription: subscription,
            previous,
        });
        cx.notify();
    }

    fn handle_tab_context_menu_action(
        &mut self,
        pane: &str,
        tab_id: &str,
        action: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            "copy-agent-id" | "copy-terminal-id" => {
                cx.write_to_clipboard(ClipboardItem::new_string(tab_id.to_owned()));
            }
            "copy-file-path" => {
                if let Some(key) = self.file_tab_key(tab_id) {
                    cx.write_to_clipboard(ClipboardItem::new_string(key.path));
                }
            }
            "copy-resume-command" => {
                if let Some(agent) = self.directory.agents.get(tab_id) {
                    if let Some(command) = agent.resume_command() {
                        cx.write_to_clipboard(ClipboardItem::new_string(command));
                    } else {
                        self.error =
                            Some(self.launch_text(if agent.provider_session_id().is_some() {
                                "workspace.tabs.toasts.resumeCommandUnavailable"
                            } else {
                                "workspace.tabs.toasts.resumeIdUnavailable"
                            }));
                        cx.notify();
                    }
                }
            }
            "rename" => self.open_tab_rename_modal(tab_id, window, cx),
            "close" => self.close_pane_tab(tab_id, window, cx),
            "close-before" | "close-after" | "close-others" => {
                self.confirm_bulk_close_tabs(pane, tab_id, action, window, cx);
            }
            "reload-agent" => {
                let result = self
                    .connection
                    .as_ref()
                    .ok_or_else(|| {
                        anyhow::anyhow!(self.launch_text("workspace.terminal.hostDisconnected"))
                    })
                    .and_then(|connection| {
                        connection.request("refresh_agent_request", json!({"agentId":tab_id}))
                    });
                match result {
                    Ok(request) => self.track_agent_tab_request(
                        request,
                        PendingAgentTabOp::Reload {
                            agent_id: tab_id.to_owned(),
                        },
                        cx,
                    ),
                    Err(error) => {
                        self.error = Some(error.to_string());
                        cx.notify();
                    }
                }
            }
            _ => {}
        }
    }

    fn track_agent_tab_request(
        &mut self,
        request: String,
        operation: PendingAgentTabOp,
        cx: &mut Context<Self>,
    ) {
        self.terminal_commands
            .pending_ops
            .insert(request.clone(), operation);
        let generation = self.host_generation;
        let timer = cx
            .background_executor()
            .timer(std::time::Duration::from_secs(150));
        cx.spawn(async move |this, cx| {
            timer.await;
            let _ = this.update(cx, |this, cx| {
                if this.host_generation == generation
                    && this
                        .terminal_commands
                        .pending_ops
                        .remove(&request)
                        .is_some()
                {
                    this.error = Some(
                        "The host did not answer. Check its sessions before trying again.".into(),
                    );
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn forget_agent_tab(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let shown = self
            .selected_splits()
            .is_some_and(|layout| layout.tab_is_focused(id));
        if let Some(view) = self.composers.get(id) {
            view.update(cx, |view, _| view.release_layout());
        }
        self.conversations.remove(id);
        self.composers
            .retain(|key, view| key != id || !view.read(cx).is_disposable());
        self.sync_timeline_subscription();
        for layout in self.splits.values_mut() {
            layout.prune_tabs(|tab| tab != id);
        }
        self.persist_splits();
        if shown {
            self.selected_agent = None;
            self.sync_active_tab(window, cx);
        }
        self.command_center_snapshot_dirty = true;
        cx.notify();
    }

    fn archive_and_close_agent_tab(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let result = self
            .connection
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("The host is disconnected."))
            .and_then(|connection| {
                connection.request("archive_agent_request", json!({"agentId":id}))
            });
        match result {
            Ok(request) => {
                self.track_agent_tab_request(
                    request,
                    PendingAgentTabOp::Archive {
                        agent_id: id.to_owned(),
                    },
                    cx,
                );
                self.forget_agent_tab(id, window, cx);
            }
            Err(error) => {
                self.error = Some(error.to_string());
                cx.notify();
            }
        }
    }

    pub(super) fn close_agent_tab(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.modal_open() {
            return;
        }
        let Some(agent) = self.directory.agents.get(id) else {
            return;
        };
        if agent.close_tab_policy() == CloseAgentTabPolicy::LayoutOnly {
            self.forget_agent_tab(id, window, cx);
            return;
        }
        if !agent.is_working() {
            self.archive_and_close_agent_tab(id, window, cx);
            return;
        }
        let answer = crate::native_confirmation::ask(
            window,
            cx,
            self.launch_text("workspace.tabs.confirmations.archiveRunningAgentTitle"),
            self.launch_text("workspace.tabs.confirmations.archiveRunningAgentMessage"),
            self.launch_text("workspace.tabs.confirmations.cancel"),
            self.launch_text("workspace.tabs.confirmations.archive"),
        );
        let generation = self.host_generation;
        let id = id.to_owned();
        let previous = window.focused(cx);
        match answer {
            Err(error) => {
                self.error = Some(error.to_string());
                cx.notify();
            }
            Ok(answer) => {
                self.focus.focus(window, cx);
                self.terminal_commands.prompt = Some(cx.spawn(async move |this, cx| {
                    let confirmed = answer.await;
                    let _ = this.update_in(cx, |this, window, cx| {
                        if this.host_generation != generation {
                            return;
                        }
                        this.terminal_commands.prompt = None;
                        if confirmed && this.directory.agents.contains_key(&id) {
                            let shown = this
                                .selected_splits()
                                .is_some_and(|layout| layout.tab_is_focused(&id));
                            this.archive_and_close_agent_tab(&id, window, cx);
                            if !shown
                                && this
                                    .selected_splits()
                                    .is_some_and(|layout| layout.tab_is_focused(&id))
                                    == false
                            {
                                if let Some(previous) = previous {
                                    previous.focus(window, cx);
                                }
                            }
                        } else if let Some(previous) = previous {
                            previous.focus(window, cx);
                        }
                        window.activate_window();
                        cx.notify();
                    });
                }));
                cx.notify();
            }
        }
    }

    fn confirm_bulk_close_tabs(
        &mut self,
        pane: &str,
        tab_id: &str,
        action: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.modal_open() {
            return;
        }
        let Some(workspace) = self.selected_workspace.clone() else {
            return;
        };
        let Some(tabs) = self
            .splits
            .get(&workspace)
            .and_then(|layout| layout.pane(pane))
            .map(|pane| pane.tabs.clone())
        else {
            return;
        };
        let Some(index) = tabs.iter().position(|id| id == tab_id) else {
            return;
        };
        let tabs: Vec<_> = tabs
            .into_iter()
            .enumerate()
            .filter(|(i, _)| match action {
                "close-before" => *i < index,
                "close-after" => *i > index,
                "close-others" => *i != index,
                _ => false,
            })
            .map(|(_, id)| id)
            .collect();
        if tabs.is_empty() {
            return;
        }
        let mut groups = BulkCloseGroups::default();
        let mut dirty = vec![];
        for id in &tabs {
            if let Some(agent) = self.directory.agents.get(id) {
                let group = if agent.close_tab_policy() == CloseAgentTabPolicy::LayoutOnly {
                    &mut groups.layout_only_agent_tabs
                } else {
                    &mut groups.archive_agent_tabs
                };
                group.push((id.clone(), id.clone()));
            } else if self.directory.terminals.contains_key(id) {
                groups.terminal_tabs.push((id.clone(), id.clone()));
            } else {
                groups.other_tabs.push(id.clone());
                if let Some(key) = self.file_tab_key(id)
                    && let Some(editor) = self.editors.get(&key)
                    && editor.read(cx).is_dirty()
                {
                    dirty.push(editor.clone());
                }
            }
        }
        let locale = paseo_desktop::locales::active_locale(self.shared_settings.get().language);
        let message = build_bulk_close_confirmation_message(locale, &groups, dirty.len());
        let title = self.launch_text(match action {
            "close-before" => "workspace.tabs.confirmations.closeTabsLeftTitle",
            "close-after" => "workspace.tabs.confirmations.closeTabsRightTitle",
            _ => "workspace.tabs.confirmations.closeOtherTabsTitle",
        });
        for editor in &dirty {
            editor.update(cx, |editor, _| editor.hold_writes());
        }
        let answer = crate::native_confirmation::ask(
            window,
            cx,
            title,
            message,
            self.launch_text("workspace.tabs.confirmations.cancel"),
            self.launch_text("workspace.tabs.confirmations.close"),
        );
        let previous = window.focused(cx);
        let generation = self.host_generation;
        let pane = pane.to_owned();
        match answer {
            Err(error) => {
                for editor in dirty {
                    editor.update(cx, |editor, cx| editor.resume_writes(cx));
                }
                self.error = Some(error.to_string());
                cx.notify();
            }
            Ok(answer) => {
                self.focus.focus(window, cx);
                self.terminal_commands.prompt = Some(cx.spawn(async move |this, cx| {
                    let confirmed = answer.await;
                    let _ = this.update_in(cx, |this, window, cx| {
                        if this.host_generation != generation {
                            for editor in dirty {
                                editor.update(cx, |editor, cx| editor.resume_writes(cx));
                            }
                            return;
                        }
                        this.terminal_commands.prompt = None;
                        if !confirmed || this.selected_workspace.as_ref() != Some(&workspace) {
                            for editor in dirty {
                                editor.update(cx, |editor, cx| editor.resume_writes(cx));
                            }
                            if let Some(previous) = previous {
                                previous.focus(window, cx);
                            }
                            window.activate_window();
                            cx.notify();
                            return;
                        }
                        let current_pane_tabs = this
                            .splits
                            .get(&workspace)
                            .and_then(|layout| layout.pane(&pane))
                            .map(|pane| pane.tabs.clone())
                            .unwrap_or_default();
                        let tabs = retain_pane_tabs(&tabs, &current_pane_tabs);
                        let mut archive_agent_ids = Vec::new();
                        let mut terminal_ids = Vec::new();
                        for id in &tabs {
                            if let Some(agent) = this.directory.agents.get(id) {
                                if agent.close_tab_policy() != CloseAgentTabPolicy::LayoutOnly {
                                    archive_agent_ids.push(id.clone());
                                }
                            } else if this.directory.terminals.contains_key(id) {
                                terminal_ids.push(id.clone());
                            }
                        }
                        if !archive_agent_ids.is_empty() || !terminal_ids.is_empty() {
                            let result = this
                                .connection
                                .as_ref()
                                .ok_or_else(|| anyhow::anyhow!("The host is disconnected."))
                                .and_then(|connection| {
                                    connection.request(
                                        "close_items_request",
                                        json!({
                                            "agentIds": archive_agent_ids,
                                            "terminalIds": terminal_ids,
                                        }),
                                    )
                                });
                            match result {
                                Ok(request) => this.track_agent_tab_request(
                                    request,
                                    PendingAgentTabOp::CloseItems {
                                        workspace: workspace.clone(),
                                        agent_ids: archive_agent_ids,
                                        terminal_ids,
                                    },
                                    cx,
                                ),
                                Err(error) => {
                                    for editor in dirty {
                                        editor.update(cx, |editor, cx| editor.resume_writes(cx));
                                    }
                                    if let Some(previous) = previous {
                                        previous.focus(window, cx);
                                    }
                                    window.activate_window();
                                    this.error = Some(error.to_string());
                                    cx.notify();
                                    return;
                                }
                            }
                        }
                        for id in tabs {
                            if let Some(key) = this.file_tab_key(&id) {
                                this.forget_file(&key, window, cx);
                            } else if let Some(agent) = this.directory.agents.get(&id) {
                                if agent.close_tab_policy() == CloseAgentTabPolicy::LayoutOnly {
                                    this.forget_agent_tab(&id, window, cx);
                                }
                            } else if this.directory.terminals.contains_key(&id) {
                                // Kept until `close_items_response` confirms termination.
                            } else if this.browsers.contains_key(&id) {
                                this.forget_browser(&id, window, cx);
                            } else {
                                this.close_launcher_tab(&id, window, cx);
                            }
                        }
                        for editor in dirty {
                            if this.editors.values().any(|live| live == &editor) {
                                editor.update(cx, |editor, cx| editor.resume_writes(cx));
                            }
                        }
                        this.persist_splits();
                        this.selected_terminal = None;
                        this.sync_active_tab(window, cx);
                        this.apply_terminal_visibility(cx);
                        window.activate_window();
                        cx.notify();
                    });
                }));
                cx.notify();
            }
        }
    }

    fn open_tab_rename_modal(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.selected_workspace.clone() else {
            return;
        };
        let Some((kind, target_id, initial)) = resolve_tab_rename_target(&self.directory, id)
        else {
            return;
        };
        let previous = window.focused(cx);
        let field = cx.new(|cx| {
            TextField::new(
                &initial,
                "",
                false,
                0.,
                false,
                self.shared_settings.clone(),
                cx,
            )
            .modal(44.)
        });
        let subscription = cx.subscribe_in(
            &field,
            window,
            |this, _, event: &FieldEvent, window, cx| match event {
                FieldEvent::Committed => this.submit_tab_rename(window, cx),
                FieldEvent::Cancelled => this.cancel_tab_rename(window, cx),
                FieldEvent::Edited => {
                    if let Some(modal) = &mut this.terminal_commands.tab_rename {
                        if let Some(submitted) = modal.submitted_title.clone() {
                            modal
                                .field
                                .update(cx, |field, cx| field.set_text(&submitted, cx));
                            return;
                        }
                        modal.error = None;
                    }
                    cx.notify();
                }
            },
        );
        field.read(cx).focus_handle(cx).focus(window, cx);
        self.terminal_commands.tab_rename = Some(TabRenameModal {
            workspace,
            tab_id: id.to_owned(),
            target_id,
            kind,
            initial_title: initial,
            field,
            pending_request_id: None,
            submitted_title: None,
            error: None,
            _subscription: subscription,
            previous,
        });
        cx.notify();
    }

    fn submit_tab_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(modal) = &self.terminal_commands.tab_rename else {
            return;
        };
        if modal.pending_request_id.is_some() {
            return;
        }
        let draft = modal.field.read(cx).text().to_owned();
        if draft == modal.initial_title {
            return;
        }
        if draft.trim().is_empty() || draft.chars().count() > 200 {
            let error = self.launch_text("common.errors.nameRequired");
            self.terminal_commands.tab_rename.as_mut().unwrap().error = Some(error);
            cx.notify();
            return;
        }
        let submitted = draft.trim().to_owned();
        let result = self
            .connection
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("The host is disconnected."))
            .and_then(|connection| match modal.kind {
                RenameTabKind::Terminal => connection.request(
                    "terminal.rename.request",
                    json!({"terminalId":modal.target_id,"title":submitted}),
                ),
                RenameTabKind::Agent => connection.request(
                    "update_agent_request",
                    json!({"agentId":modal.target_id,"name":submitted}),
                ),
            });
        if let Some(modal) = &mut self.terminal_commands.tab_rename {
            match result {
                Ok(request) => {
                    modal.pending_request_id = Some(request.clone());
                    modal.submitted_title = Some(submitted);
                    modal.error = None;
                    self.focus.focus(window, cx);
                    let generation = self.host_generation;
                    let timer = cx
                        .background_executor()
                        .timer(std::time::Duration::from_secs(150));
                    cx.spawn(async move |this, cx| {
                        timer.await;
                        let _ = this.update(cx, |this, cx| {
                            if this.host_generation == generation
                                && let Some(modal) = &mut this.terminal_commands.tab_rename
                                && modal.pending_request_id.as_deref() == Some(request.as_str())
                            {
                                modal.pending_request_id = None;
                                modal.submitted_title = None;
                                modal.error = Some(
                                    "The host did not answer. Check its sessions before trying again."
                                        .into(),
                                );
                                cx.notify();
                            }
                        });
                    })
                    .detach();
                }
                Err(error) => modal.error = Some(error.to_string()),
            }
        }
        cx.notify();
    }

    pub(super) fn render_new_tab_overlay(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        if self.terminal_commands.prompt.is_some() || self.close_window_prompt.is_some() {
            return Some(div().absolute().inset_0().occlude().into_any_element());
        }
        if let Some(modal) = &self.terminal_commands.tab_rename {
            let palette = crate::settings_view::active_palette(&self.shared_settings, window);
            let busy = modal.pending_request_id.is_some();
            let draft = modal.field.read(cx).text().to_owned();
            let submit_enabled = !busy
                && draft != modal.initial_title
                && !draft.trim().is_empty()
                && draft.chars().count() <= 200;
            let title = self.launch_text(match modal.kind {
                RenameTabKind::Terminal => "workspace.tabs.menu.renameTerminal",
                RenameTabKind::Agent => "workspace.tabs.menu.renameAgent",
            });
            let submit_label = self.launch_text(if busy {
                "renameModal.saving"
            } else {
                "workspace.tabs.menu.rename"
            });
            let cancel_label = self.launch_text("common.actions.cancel");
            return Some(
                div()
                    .absolute()
                    .inset_0()
                    .occlude()
                    .bg(rgba(0x0000008c))
                    .flex()
                    .items_center()
                    .justify_center()
                    .p_6()
                    .child(
                        div()
                            .relative()
                            .w_full()
                            .max_w(px(520.))
                            .on_mouse_down_out(cx.listener(
                                |this, event: &gpui::MouseDownEvent, window, cx| {
                                    if event.button == MouseButton::Left {
                                        cx.stop_propagation();
                                        this.cancel_tab_rename(window, cx);
                                    }
                                },
                            ))
                            .border_1()
                            .border_color(rgb(palette.surface2))
                            .rounded(px(12.))
                            .bg(rgb(palette.surface1))
                            .overflow_hidden()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .px_6()
                                    .py_4()
                                    .border_b_1()
                                    .border_color(rgb(palette.surface2))
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .child(div().font_weight(gpui::FontWeight::MEDIUM).child(title))
                                    .child(
                                        div()
                                            .id("close-tab-rename-modal")
                                            .p_2()
                                            .rounded(px(8.))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .when(!busy, |v| {
                                                v.cursor_pointer()
                                                    .hover(|v| v.bg(rgb(palette.surface2)))
                                            })
                                            .when(busy, |v| v.opacity(0.5))
                                            .child(
                                                chrome::icon("x", 16.)
                                                    .text_color(rgb(palette.foreground_muted)),
                                            )
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(|this, _, window, cx| {
                                                    cx.stop_propagation();
                                                    this.cancel_tab_rename(window, cx);
                                                }),
                                            ),
                                    ),
                            )
                            .child(
                                div()
                                    .p_6()
                                    .pb_8()
                                    .flex()
                                    .flex_col()
                                    .gap_3()
                                    .child(modal.field.clone())
                                    .when_some(modal.error.clone(), |v, error| {
                                        v.child(
                                            div()
                                                .text_color(rgb(palette.status_danger))
                                                .child(error),
                                        )
                                    })
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .id("cancel-tab-rename")
                                                    .h(px(32.))
                                                    .flex_1()
                                                    .px_3()
                                                    .rounded(px(12.))
                                                    .flex()
                                                    .items_center()
                                                    .justify_center()
                                                    .bg(rgb(palette.surface3))
                                                    .text_color(rgb(palette.foreground))
                                                    .when(!busy, |v| v.cursor_pointer())
                                                    .when(busy, |v| v.opacity(0.5))
                                                    .child(cancel_label)
                                                    .on_mouse_down(
                                                        MouseButton::Left,
                                                        cx.listener(|this, _, window, cx| {
                                                            cx.stop_propagation();
                                                            this.cancel_tab_rename(window, cx);
                                                        }),
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .id("submit-tab-rename")
                                                    .h(px(32.))
                                                    .flex_1()
                                                    .px_3()
                                                    .rounded(px(12.))
                                                    .flex()
                                                    .items_center()
                                                    .justify_center()
                                                    .bg(rgb(palette.accent))
                                                    .text_color(rgb(palette.accent_foreground))
                                                    .when(submit_enabled, |v| v.cursor_pointer())
                                                    .when(!submit_enabled, |v| v.opacity(0.5))
                                                    .child(submit_label)
                                                    .on_mouse_down(
                                                        MouseButton::Left,
                                                        cx.listener(move |this, _, window, cx| {
                                                            cx.stop_propagation();
                                                            if submit_enabled {
                                                                this.submit_tab_rename(window, cx);
                                                            }
                                                        }),
                                                    ),
                                            ),
                                    ),
                            )
                            .when(busy, |v| v.child(div().absolute().inset_0().occlude())),
                    )
                    .into_any_element(),
            );
        }
        if let Some(menu) = self.terminal_commands.tab_context_menu.as_ref() {
            let trigger = Bounds::new(menu.position, size(px(0.), px(0.)));
            return Some(
                div()
                    .absolute()
                    .inset_0()
                    .occlude()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| {
                            cx.stop_propagation();
                            this.close_tab_context_menu(window, cx);
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(|this, _, window, cx| {
                            cx.stop_propagation();
                            this.close_tab_context_menu(window, cx);
                        }),
                    )
                    .child(
                        gpui::deferred(anchored_menu(
                            trigger,
                            paseo_desktop::menu_position::Alignment::Start,
                            4.,
                            menu.view.clone(),
                        ))
                        .with_priority(4),
                    )
                    .into_any_element(),
            );
        }
        if let Some(menu) = self.terminal_commands.split_menu.as_ref() {
            let trigger = *self.terminal_commands.split_anchors.get(&menu.pane)?;
            return Some(
                div()
                    .absolute()
                    .inset_0()
                    .occlude()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| {
                            cx.stop_propagation();
                            this.close_split_pane_menu(window, cx);
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(|this, _, window, cx| {
                            cx.stop_propagation();
                            this.close_split_pane_menu(window, cx);
                        }),
                    )
                    .child(
                        gpui::deferred(anchored_menu(
                            trigger,
                            paseo_desktop::menu_position::Alignment::End,
                            4.,
                            menu.view.clone(),
                        ))
                        .with_priority(4),
                    )
                    .into_any_element(),
            );
        }
        let menu = self.terminal_commands.menu.as_ref()?;
        let trigger = *self.terminal_commands.anchors.get(&menu.destination.pane)?;
        Some(
            div()
                .absolute()
                .inset_0()
                .occlude()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        this.close_new_tab_menu(window, cx);
                    }),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        this.close_new_tab_menu(window, cx);
                    }),
                )
                .child(
                    gpui::deferred(anchored_menu(
                        trigger,
                        paseo_desktop::menu_position::Alignment::Start,
                        4.,
                        menu.view.clone(),
                    ))
                    .with_priority(4),
                )
                .into_any_element(),
        )
    }
    pub(super) fn render_launcher(
        &mut self,
        pane: &str,
        replace: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let palette = crate::settings_view::active_palette(&self.shared_settings, window);
        let Some(destination) = self.terminal_destination(pane, replace) else {
            return div().into_any_element();
        };
        self.load_terminal_profiles(false, cx);
        let rows = self.launcher_rows(
            &destination.pane,
            paseo_desktop::panel_host::LaunchSurface::Panel,
            cx,
        );
        let interactive = self
            .terminal_focus_ticket()
            .is_some_and(|focus| focus.pane == pane)
            && self.focus.is_focused(window);
        // A fresh New Tab must not inherit a previous launcher's selected profile.
        let row_key = (
            destination.workspace.clone(),
            destination.pane.clone(),
            destination.replace.clone(),
        );
        let active = self
            .terminal_commands
            .active
            .entry(row_key.clone())
            .or_insert_with(|| {
                rows.iter()
                    .find(|row| row.enabled)
                    .map(|row| row.id.clone())
                    .unwrap_or_default()
            });
        if !rows.iter().any(|row| row.enabled && row.id == *active) {
            *active = rows
                .iter()
                .find(|row| row.enabled)
                .map(|row| row.id.clone())
                .unwrap_or_default();
        }
        let active = active.clone();
        let mut builtins = vec![];
        let mut profiles = vec![];
        for row in rows {
            let target = destination.clone();
            let selection = row.selection;
            let enabled = row.enabled;
            let owner = row_key.clone();
            let selected = row.id.clone();
            let glyph = if let Some(key) = &row.profile {
                chrome::terminal_profile_icon(key.as_ref().map(String::as_str), p::ICON_SIZE)
            } else {
                chrome::icon(row.icon, p::ICON_SIZE)
            };
            let element = div()
                .id(SharedString::from(format!("launcher-{pane}-{}", row.id)))
                .min_h(px(p::ROW_HEIGHT))
                .px(px(p::ROW_PADDING))
                .rounded(px(p::ROW_RADIUS))
                .border_1()
                .border_color(if interactive && active == row.id && enabled {
                    rgb(palette.border_accent)
                } else {
                    rgb(palette.surface1)
                })
                .bg(rgb(palette.surface1))
                .flex()
                .items_center()
                .gap_2()
                .when(!enabled, |v| v.opacity(0.5))
                .when(enabled, |v| {
                    v.cursor_pointer().hover(|v| v.bg(rgb(palette.surface2)))
                })
                .when(
                    row.profile
                        .as_ref()
                        .is_none_or(|key| key.as_ref().is_some_and(|key| !key.is_empty())),
                    |row| row.child(glyph.text_color(rgb(palette.foreground_muted))),
                )
                .child(div().flex_1().child(row.label))
                .when_some(row.shortcut, |row, shortcut| {
                    row.child(chrome::shortcut_badge(shortcut, &palette))
                })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        if enabled {
                            this.terminal_commands
                                .active
                                .insert(owner.clone(), selected.clone());
                            this.focus.focus(window, cx);
                            this.choose_launcher(target.clone(), selection.clone(), window, cx);
                        }
                    }),
                )
                .into_any_element();
            if row.profile.is_some() {
                profiles.push(element);
            } else {
                builtins.push(element);
            }
        }
        let mut rail = div()
            .w_full()
            .max_w(px(p::MAX_WIDTH))
            .flex()
            .flex_col()
            .gap(px(p::GROUP_GAP))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(p::ROW_GAP))
                    .children(builtins),
            );
        if !profiles.is_empty() {
            rail = rail.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(p::ROW_GAP))
                    .child(
                        div()
                            .px(px(p::HEADER_PADDING))
                            .mb(px(p::HEADER_MARGIN))
                            .flex()
                            .items_center()
                            .justify_between()
                            .h(px(p::HEADER_HEIGHT))
                            .text_size(px(12.))
                            .text_color(rgb(palette.foreground_extra_muted))
                            .child(self.launch_text("workspace.tabs.actions.terminalProfilesMenu"))
                            .child(
                                div()
                                    .id("edit-profiles-launcher")
                                    .size(px(20.))
                                    .m(px(-4.))
                                    .rounded(px(6.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .hover(|v| v.bg(rgb(palette.surface2)))
                                    .child(chrome::icon("pencil", 12.).opacity(0.5))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _, window, cx| {
                                            cx.stop_propagation();
                                            this.open_pane(Pane::HostTerminals, window, cx);
                                        }),
                                    ),
                            ),
                    )
                    .children(profiles),
            );
        }
        let error = self
            .terminal_commands
            .state
            .error_for(Some(&destination.workspace))
            .or(self.terminal_commands.state.config_error.clone());
        div()
            .id(SharedString::from(format!("launcher-scroll-{pane}")))
            .size_full()
            .overflow_y_scroll()
            .child(
                div()
                    .min_h_full()
                    .p(px(p::PADDING))
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .child(rail)
                    .when_some(error, |v, error| {
                        v.child(
                            div()
                                .mt_3()
                                .text_color(rgb(palette.destructive))
                                .child(error),
                        )
                    }),
            )
            .into_any_element()
    }
    pub(super) fn on_launcher_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.modal_open() || self.command_center_open(cx) || !self.focus.is_focused(window) {
            return;
        }
        let Some(focus) = self.terminal_focus_ticket() else {
            return;
        };
        if focus.tab.as_ref().is_some_and(|id| !id.starts_with(PREFIX)) {
            return;
        }
        let rows = self.launcher_rows(
            &focus.pane,
            paseo_desktop::panel_host::LaunchSurface::Panel,
            cx,
        );
        let enabled: Vec<_> = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.enabled)
            .map(|(i, _)| i)
            .collect();
        if enabled.is_empty() {
            return;
        }
        let row_key = (
            focus.workspace.clone(),
            focus.pane.clone(),
            focus.tab.clone(),
        );
        let active = self
            .terminal_commands
            .active
            .get(&row_key)
            .and_then(|id| rows.iter().position(|row| &row.id == id && row.enabled))
            .unwrap_or(enabled[0]);
        let current = enabled.iter().position(|i| *i == active);
        match event.keystroke.key.as_str() {
            "up" | "down" => {
                let index = if event.keystroke.key == "up" {
                    current.map_or(enabled.len() - 1, |i| {
                        (i + enabled.len() - 1) % enabled.len()
                    })
                } else {
                    current.map_or(0, |i| (i + 1) % enabled.len())
                };
                self.terminal_commands
                    .active
                    .insert(row_key, rows[enabled[index]].id.clone());
                cx.stop_propagation();
                cx.notify();
            }
            "enter" => {
                if let Some(row) = rows.get(active).filter(|row| row.enabled)
                    && let Some(destination) = self.terminal_destination(&focus.pane, focus.tab)
                {
                    self.choose_launcher(destination, row.selection.clone(), window, cx);
                    cx.stop_propagation();
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use paseo_desktop::model::Directory;

    #[test]
    fn rename_response_checks_accepted_flag_and_errors() {
        assert_eq!(
            resolve_rename_response(
                "update_agent_response",
                &json!({"requestId":"r1","agentId":"a1","accepted":false,"error":"Agent not found"})
            ),
            RenameResponseOutcome::Failed("Agent not found".into())
        );
        assert_eq!(
            resolve_rename_response(
                "update_agent_response",
                &json!({"requestId":"r1","agentId":"a1","accepted":true,"error":null})
            ),
            RenameResponseOutcome::Completed
        );
        assert_eq!(
            resolve_rename_response(
                "terminal.rename.response",
                &json!({"requestId":"r2","success":false,"error":"Terminal not found"})
            ),
            RenameResponseOutcome::Failed("Terminal not found".into())
        );
        assert_eq!(
            resolve_rename_response(
                "terminal.rename.response",
                &json!({"requestId":"r2","success":true,"error":null})
            ),
            RenameResponseOutcome::Completed
        );
        assert_eq!(
            resolve_rename_response("rpc_error", &json!({"requestId":"r3","error":"Rejected"})),
            RenameResponseOutcome::Failed("Rejected".into())
        );
    }

    #[test]
    fn terminal_backed_agent_tab_resolves_to_agent_rename_target() {
        let mut directory = Directory::default();
        directory
            .apply(
                "list_terminals_response",
                json!({"terminals":[{"id":"pty-1","name":"zsh","workspaceId":"ws-1","title":"shell title","cwd":"/tmp"}]}),
            )
            .unwrap();
        directory
            .apply(
                "fetch_agents_response",
                json!({
                    "entries": [
                        {
                            "agent": {
                                "id": "agent-pty",
                                "provider": "pi",
                                "workspaceId": "ws-1",
                                "title": "Pi Session",
                                "status": "idle",
                                "execution": { "kind": "terminal", "terminalId": "pty-1" }
                            }
                        },
                        {
                            "agent": {
                                "id": "agent-root",
                                "provider": "claude",
                                "workspaceId": "ws-1",
                                "title": "Root Agent",
                                "status": "idle"
                            }
                        }
                    ],
                    "pageInfo": { "nextCursor": null, "hasMore": false }
                }),
            )
            .unwrap();
        directory
            .apply(
                "list_terminals_response",
                json!({
                    "terminals": [
                        {"id":"pty-1","name":"zsh","workspaceId":"ws-1","title":"shell title","cwd":"/tmp"},
                        {"id":"pty-plain","name":"bash","workspaceId":"ws-1","title":"Plain Shell","cwd":"/tmp"}
                    ]
                }),
            )
            .unwrap();

        assert_eq!(
            resolve_tab_rename_target(&directory, "pty-1"),
            Some((
                RenameTabKind::Agent,
                "agent-pty".into(),
                "Pi Session".into()
            ))
        );
        assert_eq!(
            resolve_tab_rename_target(&directory, "pty-plain"),
            Some((
                RenameTabKind::Terminal,
                "pty-plain".into(),
                "Plain Shell".into()
            ))
        );
        assert_eq!(
            resolve_tab_rename_target(&directory, "agent-root"),
            Some((
                RenameTabKind::Agent,
                "agent-root".into(),
                "Root Agent".into()
            ))
        );
    }

    #[test]
    fn close_items_resolution_preserves_failed_tabs_and_surfaces_partial_failure() {
        let resolution = resolve_close_items_response(
            &json!({
                "requestId": "req-close",
                "agents": [{ "agentId": "a1", "archivedAt": "2026-11-03T00:00:00Z" }],
                "terminals": [
                    { "terminalId": "t1", "success": true },
                    { "terminalId": "t2", "success": false }
                ]
            }),
            &["a1".into(), "a2".into()],
            &["t1".into(), "t2".into()],
        );
        assert_eq!(
            resolution.archived_agents,
            vec![("a1".into(), "2026-11-03T00:00:00Z".into())]
        );
        assert_eq!(resolution.failed_agents, vec!["a2".to_owned()]);
        assert_eq!(resolution.killed_terminals, vec!["t1".to_owned()]);
        assert_eq!(resolution.failed_terminals, vec!["t2".to_owned()]);
        assert_eq!(
            resolution.failure_message().as_deref(),
            Some("Failed to archive 1 agent(s) and close 1 terminal(s).")
        );
    }

    #[test]
    fn disconnected_clears_pending_ops_and_resets_terminal_state() {
        let mut commands = Commands::default();
        commands.profiles_subscribed = true;
        commands.state.begin(
            "req-create".into(),
            Operation::Create(Destination {
                workspace: "ws".into(),
                cwd: "/tmp".into(),
                pane: "main".into(),
                replace: None,
                focus: None,
            }),
        );
        commands.pending_ops.insert(
            "req-op".into(),
            PendingAgentTabOp::Reload {
                agent_id: "a1".into(),
            },
        );
        commands.disconnected();
        assert!(!commands.profiles_subscribed);
        assert!(commands.pending_ops.is_empty());
        assert!(!commands.state.creating());
        assert!(commands.state.error_for(Some("ws")).is_some());
    }

    #[test]
    fn retain_pane_tabs_filters_tabs_removed_while_confirmation_was_open() {
        let requested = vec!["a".into(), "b".into(), "c".into()];
        let current = vec!["b".into(), "d".into(), "a".into()];
        assert_eq!(
            retain_pane_tabs(&requested, &current),
            vec!["a".to_owned(), "b".to_owned()]
        );
    }

    #[test]
    fn launcher_order_and_singleton_filtering_match_host_and_surface() {
        use paseo_desktop::panel_host::{Host, LaunchSurface};

        assert_eq!(
            resolve_built_in_launcher_ids(
                Host::Main,
                LaunchSurface::Panel,
                &["files", "changes_tree"],
                true,
                true,
            ),
            vec!["agent", "terminal", "diff", "browser", "pullRequest"]
        );
        assert_eq!(
            resolve_built_in_launcher_ids(
                Host::Main,
                LaunchSurface::Menu,
                &["files", "changes_tree"],
                true,
                true,
            ),
            vec!["agent", "terminal", "diff", "browser", "pullRequest"]
        );
        assert_eq!(
            resolve_built_in_launcher_ids(
                Host::Explorer,
                LaunchSurface::Panel,
                &["files", "changes_tree"],
                true,
                true,
            ),
            vec![
                "changes",
                "terminal",
                "diff",
                "files",
                "agent",
                "pullRequest",
            ]
        );
        assert_eq!(
            resolve_built_in_launcher_ids(
                Host::Explorer,
                LaunchSurface::Menu,
                &["files", "changes_tree"],
                true,
                true,
            ),
            vec!["terminal", "diff", "agent", "pullRequest"]
        );
    }
}

use super::{NewBrowser, NewTab, NewTerminal, Paseo};
use crate::launcher_presentation as p;
use crate::{
    action_menu::{ActionMenu, Chosen, Item},
    chrome,
};
use gpui::{
    Bounds, Context, Entity, FocusHandle, Focusable, MouseButton, Pixels, Point, SharedString,
    Task, Window, div, point, prelude::*, px, rgb,
};
use paseo_desktop::terminal_lifecycle::{
    self as model, Destination, Focus, Operation, Outcome, Profile,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub(super) const PREFIX: &str = "launcher:";
struct Menu {
    view: Entity<ActionMenu>,
    _subscription: gpui::Subscription,
    destination: Destination,
    position: Point<Pixels>,
    previous: Option<FocusHandle>,
}
#[derive(Default)]
pub(super) struct Commands {
    pub state: model::State,
    pub profiles_subscribed: bool,
    menu: Option<Menu>,
    prompt: Option<Task<()>>,
    pub anchors: BTreeMap<String, Bounds<Pixels>>,
    active: BTreeMap<(String, String, Option<String>), String>,
}
impl Commands {
    pub fn retain(
        &mut self,
        workspace: Option<&str>,
        layout: &paseo_desktop::split_layout::SplitLayout,
    ) {
        let panes = layout.panes();
        self.anchors
            .retain(|id, _| panes.iter().any(|pane| &pane.id == id));
        self.active.retain(|(owner, pane, tab), _| {
            workspace == Some(owner.as_str())
                && layout.pane(pane).is_some_and(|pane| {
                    tab.as_ref()
                        .map_or(pane.focused_tab.is_none(), |tab| pane.tabs.contains(tab))
                })
        });
    }
    pub fn modal(&self) -> bool {
        self.prompt.is_some() || self.menu.is_some()
    }
}
#[derive(Clone)]
enum Selection {
    Terminal(Option<Profile>),
    Browser,
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
        let pane = self.splits.get(workspace)?.focused_pane()?;
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
                                        menu.position,
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
    fn launcher_rows(&self, cx: &gpui::App) -> Vec<Row> {
        let (git, pr) = self
            .tools
            .as_ref()
            .map_or((false, false), |tools| tools.read(cx).launch_flags());
        let mut rows = vec![];
        for (id, key, icon, selection, visible) in [
            (
                "agent",
                "workspace.tabs.fallback.agent",
                "square-pen",
                Selection::Unavailable,
                true,
            ),
            (
                "terminal",
                "workspace.tabs.fallback.terminal",
                "square-terminal",
                Selection::Terminal(None),
                true,
            ),
            (
                "changes",
                "panels.diff.changesLabel",
                "files",
                Selection::Unavailable,
                git,
            ),
            (
                "diff",
                "panels.diff.diffLabel",
                "file-diff",
                Selection::Unavailable,
                git,
            ),
            (
                "files",
                "panels.files.label",
                "files",
                Selection::Unavailable,
                true,
            ),
            (
                "browser",
                "workspace.tabs.fallback.browser",
                "globe",
                Selection::Browser,
                true,
            ),
            (
                "pullRequest",
                "panels.pullRequest.label",
                "git-pull-request",
                Selection::Unavailable,
                pr,
            ),
        ] {
            let kind = match id {
                "agent" => "draft",
                "changes" => "changes_tree",
                "diff" => "working_diff",
                "pullRequest" => "pull_request",
                other => other,
            };
            if visible && p::MAIN_PANEL_KINDS.contains(&kind) {
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
        }
        rows.sort_by_key(|row| {
            p::PRIMARY_ORDER
                .iter()
                .position(|id| *id == row.id)
                .unwrap_or(usize::MAX)
        });
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
    pub(super) fn close_new_tab_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(menu) = self.terminal_commands.menu.take() {
            if let Some(focus) = menu.previous {
                focus.focus(window, cx);
            }
            cx.notify();
        }
    }
    pub(super) fn open_new_tab_menu(
        &mut self,
        pane: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_new_tab_menu(window, cx);
        self.close_workspace_menu(window, cx);
        self.scripts_menu = None;
        self.focus_split_pane(pane.clone(), window, cx);
        let Some(destination) = self.terminal_destination(&pane, None) else {
            return;
        };
        let Some(bounds) = self.terminal_commands.anchors.get(&pane) else {
            return;
        };
        let position = point(bounds.left(), bounds.bottom() + px(4.));
        let previous = window.focused(cx);
        self.show_new_tab_menu(destination, position, previous, window, cx);
        self.load_terminal_profiles(false, cx);
    }
    fn show_new_tab_menu(
        &mut self,
        destination: Destination,
        position: Point<Pixels>,
        previous: Option<FocusHandle>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut items = vec![];
        let mut selections = BTreeMap::new();
        let mut profiles = false;
        for row in self.launcher_rows(cx) {
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
            edit.enabled = false;
            items.push(edit);
        }
        let view = cx.new(|cx| ActionMenu::new(items, self.shared_settings.clone(), cx).launcher());
        let target = destination.clone();
        let subscription =
            cx.subscribe_in(&view, window, move |this, _, event: &Chosen, window, cx| {
                this.close_new_tab_menu(window, cx);
                if let Some(selection) = event.0.as_ref().and_then(|id| selections.get(id)) {
                    this.choose_launcher(target.clone(), selection.clone(), window, cx);
                }
            });
        view.read(cx).focus_handle(cx).focus(window, cx);
        self.terminal_commands.menu = Some(Menu {
            view,
            _subscription: subscription,
            destination,
            position,
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
            Selection::Unavailable => {}
        }
    }
    pub(super) fn on_new_tab(&mut self, _: &NewTab, window: &mut Window, cx: &mut Context<Self>) {
        if self.command_center_open(cx) || self.terminal_commands.modal() {
            return;
        }
        if !cfg!(target_os = "macos") && self.terminal_view_focused(window, cx) {
            cx.propagate();
            return;
        }
        let id = format!("{PREFIX}{}", uuid::Uuid::new_v4());
        if self
            .selected_splits()
            .is_some_and(|layout| layout.attach_tab(&id).is_some())
        {
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
        if self.command_center_open(cx) || self.terminal_commands.modal() {
            return;
        }
        if !cfg!(target_os = "macos") && self.terminal_view_focused(window, cx) {
            cx.propagate();
            return;
        }
        let Some(focus) = self.terminal_focus_ticket() else {
            return;
        };
        let replace = focus
            .tab
            .as_ref()
            .filter(|id| id.starts_with(PREFIX))
            .cloned();
        if let Some(destination) = self.terminal_destination(&focus.pane, replace) {
            self.create_terminal(destination, None, cx);
        }
    }
    pub(super) fn on_new_browser(
        &mut self,
        _: &NewBrowser,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.command_center_open(cx) || self.terminal_commands.modal() {
            return;
        }
        if !cfg!(target_os = "macos") && self.terminal_view_focused(window, cx) {
            cx.propagate();
            return;
        }
        let Some(focus) = self.terminal_focus_ticket() else {
            return;
        };
        let replace = focus
            .tab
            .as_ref()
            .filter(|id| id.starts_with(PREFIX))
            .cloned();
        if let Some(destination) = self.terminal_destination(&focus.pane, replace) {
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
        if self.terminal_commands.modal() || self.terminal_commands.state.killing(id) {
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
    pub(super) fn render_new_tab_overlay(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        if self.terminal_commands.prompt.is_some() {
            return Some(div().absolute().inset_0().occlude().into_any_element());
        }
        let menu = self.terminal_commands.menu.as_ref()?;
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
                    gpui::deferred(
                        gpui::anchored()
                            .position(menu.position)
                            .anchor(gpui::Anchor::TopLeft)
                            .snap_to_window_with_margin(px(8.))
                            .child(menu.view.clone()),
                    )
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
        let rows = self.launcher_rows(cx);
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
                            .child(chrome::icon("pencil", 12.).opacity(0.5)),
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
        if self.terminal_commands.modal()
            || self.command_center_open(cx)
            || !self.focus.is_focused(window)
        {
            return;
        }
        let Some(focus) = self.terminal_focus_ticket() else {
            return;
        };
        if focus.tab.as_ref().is_some_and(|id| !id.starts_with(PREFIX)) {
            return;
        }
        let rows = self.launcher_rows(cx);
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

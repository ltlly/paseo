#[path = "terminal_commands.rs"]
mod terminal_commands;
#[path = "workspace_tab_row.rs"]
mod workspace_tab_row;
use std::{collections::BTreeMap, collections::BTreeSet, collections::HashSet, sync::Arc};

use anyhow::{Context as _, Result};
use gpui::{
    AnyWindowHandle, App, Bounds, Context, CursorStyle, Entity, FocusHandle, Focusable, Global,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Render, SharedString,
    SystemNotification, TitlebarOptions, Window, WindowBounds, WindowOptions, actions, canvas, div,
    point, prelude::*, px, rgb, size,
};
use paseo_desktop::split_layout::{
    FocusDirection, MAIN_PANE_ID, SplitDirection, SplitIds, SplitLayout, SplitNode, SplitPane,
    SplitPosition,
};
use serde_json::{Value, json};

use crate::file_editor::{CloseRequested, FileEditor};
use crate::hosts_surface::{HostsPanel, HostsPanelEvent, HostsSnapshot};
use crate::terminal_view::TerminalView;
use crate::workspace_tools::{
    OpenFile, ScriptAction, ScriptActionKind, ScriptsMenu, ScriptsPicked, ViewTerminal,
    WorkspaceTools,
};
use paseo_desktop::activation::{
    self, ActivationStep, ActivationTarget, ActivationWindow, AttentionDecision, PendingActivation,
};
use paseo_desktop::browser_model::BrowserFocusOwner;
use paseo_desktop::command_center_model::{
    Row as CommandRow, Scope as CommandScope, Target as CommandTarget,
};
use paseo_desktop::hosts::{
    HostClient, HostConnection, HostPhase, HostProfile, HostRuntimeEvent, HostTarget, UpsertInput,
    connection_from_target, daemon_registrations, grammar_for_connection, parse_host_target,
    save as save_registry, upsert_host_connection_in_profiles,
};
use paseo_desktop::terminal_retention::TerminalCache;
use paseo_desktop::workspace_layout_storage::SplitLayoutStore;
use paseo_desktop::{
    client::{Connection, Event, Target, default_home},
    model::{AgentExecution, AgentPage, Directory, FocusedSession, ServerInfo, WorkspacePage},
};

/// Provider identities from a providers snapshot payload.
fn provider_ids(payload: &Value) -> Vec<String> {
    payload["entries"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry["provider"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// One settings store per window; the pane owns reads and writes. A path that
/// cannot be resolved is an error the settings pane shows, never a silent temp
/// directory fallback.
fn settings_store() -> Result<paseo_desktop::settings::SettingsStore> {
    let path = paseo_desktop::settings::SettingsStore::default_path()?;
    Ok(paseo_desktop::settings::SettingsStore::new(path))
}

fn resolve_launch_target() -> Result<Target> {
    match std::env::var("PASEO_ENDPOINT") {
        Ok(url) => Target::remote(url, std::env::var("PASEO_PASSWORD").ok()),
        Err(_) => default_home().and_then(|home| Target::local(&home)),
    }
}

struct WorkspaceMenu {
    view: Entity<crate::action_menu::ActionMenu>,
    _subscription: gpui::Subscription,
    position: gpui::Point<gpui::Pixels>,
    previous_focus: Option<FocusHandle>,
    header: bool,
}

/// Windows of this native client. Every window holds its own host connection
/// and terminal claims while sharing the settings file; closing one window
/// closes only its own session — the daemon keeps every terminal alive.
#[derive(Default)]
pub struct WindowCoordinator {
    pub windows: Vec<AnyWindowHandle>,
}
impl Global for WindowCoordinator {}

/// Opens one client window. The first window of the session resolves the
/// launch target; every window after it resolves the same target again, so
/// additional windows are independent claimants like the desktop app's.
pub fn open_paseo_window(
    cx: &mut App,
    pending: Option<ActivationTarget>,
) -> Result<AnyWindowHandle> {
    let target = resolve_launch_target();
    let bounds = Bounds::centered(None, size(px(1280.), px(800.)), cx);
    let handle = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("Paseo Native".into()),
                appears_transparent: cfg!(target_os = "windows"),
                ..Default::default()
            }),
            window_decorations: cfg!(target_os = "linux")
                .then_some(gpui::WindowDecorations::Client),
            ..Default::default()
        },
        |window, cx| {
            let target = crate::window_chrome::configure(window).and(target);
            cx.new(|cx| Paseo::new(target, window, cx))
        },
    )?;
    cx.default_global::<WindowCoordinator>()
        .windows
        .push(handle.into());
    if let Some(target) = pending {
        let typed = handle.downcast::<Paseo>().context("window root type")?;
        typed.update(cx, |paseo, window, cx| paseo.activate(target, window, cx))?;
    }
    Ok(handle.into())
}

/// Focuses the window an activation should land in: the window that posted a
/// notification, then one already on the target's host, then one that may
/// connect to it, then focus and visibility. A window-less app opens its primary
/// window, like the desktop window owner.
pub fn route_activation(cx: &mut App, target: ActivationTarget, sender: Option<u64>) {
    cx.activate(true);
    let windows = cx
        .try_global::<WindowCoordinator>()
        .map(|coordinator| coordinator.windows.clone())
        .unwrap_or_default();
    let active_window_id = cx.active_window().map(|active| active.window_id().as_u64());
    let presence: Vec<ActivationWindow> = windows
        .iter()
        .map(|handle| {
            let window_id = handle.window_id().as_u64();
            let paseo = handle
                .downcast::<Paseo>()
                .and_then(|typed| typed.read(cx).ok());
            ActivationWindow {
                window_id,
                focused: active_window_id == Some(window_id),
                visible: paseo.is_some_and(|paseo| paseo.window_visible),
                connected_server_id: paseo.and_then(Paseo::admitted_server_id),
                host_saved: paseo.is_some_and(|paseo| {
                    paseo
                        .hosts
                        .profiles
                        .iter()
                        .any(|profile| profile.server_id == target.server_id())
                }),
            }
        })
        .collect();
    match activation::decide_activation_window(&presence, &target, sender) {
        activation::WindowDecision::Focus(index) => {
            if let Some(handle) = windows.get(index)
                && let Some(typed) = handle.downcast::<Paseo>()
            {
                let _ = typed.update(cx, |paseo, window, cx| {
                    window.activate_window();
                    paseo.activate(target, window, cx);
                });
            }
        }
        activation::WindowDecision::OpenPrimary => {
            let _ = open_paseo_window(cx, Some(target));
        }
    }
}

/// macOS-style reopen: only a window-less app opens its primary window.
pub fn restore_when_activated(cx: &mut App) {
    let count = cx
        .try_global::<WindowCoordinator>()
        .map(|coordinator| coordinator.windows.len())
        .unwrap_or(0);
    if activation::restore_when_activated(count) {
        let _ = open_paseo_window(cx, None);
    }
}

/// Client-side saved-host bookkeeping: the native registry, the active host
/// phase, and the daemon's own registrations observed over the connection.
struct HostsState {
    profiles: Vec<HostProfile>,
    /// Set when the registry exists but cannot be read. Saving stays blocked so
    /// a corrupt file is never silently replaced with an empty list.
    registry_error: Option<String>,
    local_server_id: Option<String>,
    active_server_id: Option<String>,
    phase: Option<HostPhase>,
    message: Option<(bool, String)>,
    /// An in-flight "connect & save" attempt with its retry memory.
    add: Option<AddAttempt>,
    registrations: Option<daemon_registrations::HostDirectory>,
    registration_busy: Option<String>,
    registration_error: Option<String>,
    registration_requests: HashSet<String>,
    observation_request: Option<String>,
    /// The active daemon declared the `connectedHosts` capability. Gates the
    /// management entry, the handlers, and automatic observation together.
    connected_hosts_supported: bool,
}

#[derive(Clone)]
struct AddAttempt {
    grammar: String,
    password: Option<String>,
}

impl HostsState {
    fn load() -> Self {
        let (profiles, registry_error) = match default_home() {
            Ok(home) => match paseo_desktop::hosts::load(&home) {
                Ok(profiles) => (profiles, None),
                Err(error) => (Vec::new(), Some(registry_load_error(&home, &error))),
            },
            Err(error) => (
                Vec::new(),
                Some(format!(
                    "Cannot locate the native client state root: {error:#}"
                )),
            ),
        };
        let message = registry_error.clone().map(|error| (true, error));
        Self {
            profiles,
            registry_error,
            local_server_id: None,
            active_server_id: None,
            phase: None,
            message,
            add: None,
            registrations: None,
            registration_busy: None,
            registration_error: None,
            registration_requests: HashSet::new(),
            observation_request: None,
            connected_hosts_supported: false,
        }
    }

    /// Returns false when the registry could not be written. A registry load
    /// failure keeps every save blocked so the unreadable file stays intact.
    fn persist(&mut self) -> bool {
        if let Some(error) = &self.registry_error {
            self.message = Some((true, error.clone()));
            return false;
        }
        let result = default_home().and_then(|home| save_registry(&home, &self.profiles));
        match result {
            Ok(()) => true,
            Err(error) => {
                self.message = Some((true, format!("Cannot save hosts: {error:#}")));
                false
            }
        }
    }

    fn snapshot(&self, connected: bool) -> HostsSnapshot {
        HostsSnapshot {
            profiles: paseo_desktop::hosts::profile::order_profiles_local_first(
                self.profiles.clone(),
                self.local_server_id.as_deref(),
            ),
            local_server_id: self.local_server_id.clone(),
            active_server_id: self.active_server_id.clone(),
            phase: self.phase.clone(),
            message: self.message.clone(),
            adding: self.add.is_some(),
            plaintext_warning: true,
            registrations: self.registrations.clone(),
            registration_busy: self.registration_busy.clone(),
            registration_error: self.registration_error.clone(),
            connected,
            connected_hosts_supported: self.connected_hosts_supported,
        }
    }

    /// The saved profile's connections as concrete grammar inputs, preferred
    /// connection first. The payload password is encoded once here; the
    /// socket/pipe branches carry theirs in the target, not the grammar.
    fn connect_grammar(profile: &HostProfile) -> Option<String> {
        let preferred = profile
            .preferred_connection_id
            .as_deref()
            .and_then(|id| profile.connections.iter().find(|c| c.id() == id));
        let mut ordered = preferred.into_iter().chain(
            profile
                .connections
                .iter()
                .filter(move |c| Some(*c) != preferred),
        );
        ordered.next().and_then(|connection| match connection {
            // Relay routes keep their server id, daemon key, password and
            // security floor in the typed grammar; the direct grammar has no
            // relay shape and would drop the identity.
            HostConnection::Relay { .. } => {
                connection.relay_connection_uri(&profile.server_id, profile.password.as_deref())
            }
            _ => grammar_for_connection(connection, profile.password.as_deref()),
        })
    }
}

fn registry_load_error(home: &std::path::Path, error: &anyhow::Error) -> String {
    format!(
        "The saved host registry could not be read, so it was left untouched. Move {} aside to re-enable saving. ({error:#})",
        paseo_desktop::hosts::registry_path(home).display()
    )
}

/// A saved connection record for a grammar input. The record comes from the
/// parsed target facts, never from display text, so TLS and custom URLs keep
/// their meaning; an address the model cannot replay is reported instead of
/// being saved incorrectly. Relay targets parse into their offer record, so a
/// connect & save round trip keeps the relay identity.
fn connection_from_grammar(grammar: &str) -> Option<HostConnection> {
    let target = parse_host_target(grammar).ok()?;
    connection_from_target(&target)
}

#[derive(Clone)]
enum PendingConnection {
    /// The startup target, already resolved by `Target`.
    Target(Target),
    /// A grammar connection attempt retained across failures.
    Grammar {
        target: HostTarget,
        expected_server_id: Option<String>,
    },
}

actions!(
    paseo,
    [
        SplitPaneRight,
        SplitPaneDown,
        FocusPaneLeft,
        FocusPaneRight,
        FocusPaneUp,
        FocusPaneDown,
        MoveTabLeft,
        MoveTabRight,
        MoveTabUp,
        MoveTabDown,
        CloseFocusedPane,
        NewWindow,
        NewTab,
        NewTerminal,
        NewBrowser,
        ToggleExplorer,
        ToggleExplorerTerminal,
        SearchFiles,
        ToggleCommandCenter,
    ]
);

/// Retained terminal views: one view per pane stays live. The restore bound
/// (`MAX_PANES`) is the practical split ceiling, so a smaller cache would evict
/// a rendered pane and reopen it on the next frame.
const TERMINAL_CACHE_CAPACITY: usize = 64;
/// Hit area around the 1px separator line, in logical pixels.
const SEPARATOR_HIT: f32 = 8.;
/// Prefix of a pane tab that owns a browser surface. Persisted layouts carry
/// these ids, so the prefix is how a restore tells a browser tab from a terminal
/// or agent tab whose daemon session is simply gone.
const BROWSER_TAB_PREFIX: &str = "browser-";
/// A tab drag starts moving the tab after this squared distance so a plain
/// click still selects it.
const TAB_DRAG_THRESHOLD_SQUARED: f64 = 16.;

#[derive(Clone)]
struct SplitDrag {
    group_id: String,
    index: usize,
    horizontal: bool,
    start_axis: f64,
    start_sizes: Vec<f64>,
    group_px: f64,
}

/// A tab pressed in a pane strip. Clicking selects the tab immediately; moving
/// past the threshold and releasing over another pane moves ownership, like
/// the app's tab drag-and-drop between panes.
#[derive(Clone)]
struct TabDrag {
    tab_id: String,
    from_pane: String,
    start_x: f64,
    start_y: f64,
    moved: bool,
    hovered_pane: Option<String>,
    /// Pane hover events reach the root handler afterwards, so a root move
    /// with no pane hover since the last one means the pointer left every
    /// pane and a release must cancel instead of reusing the stale target.
    pane_hover_fresh: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PaneTabKind {
    Terminal,
    Agent,
    File,
    Browser,
    Launcher,
}

#[derive(Clone)]
struct ExplorerDrag {
    start_x: f64,
    start_width: f64,
    preview_width: f64,
}

pub struct Paseo {
    connection: Option<Arc<Connection>>,
    host_generation: u64,
    host: Option<HostClient>,
    hosts: HostsState,
    /// The daemon public key of the active relay target, if any. Once the
    /// connection comes online with negotiated protected security, every saved
    /// relay route to this key is pinned and persisted.
    active_relay_daemon_key: Option<String>,
    /// True until the first Ready proves the startup target was local
    /// discovery, so an explicit PASEO_ENDPOINT is never marked local.
    local_discovery: bool,
    /// The last connection attempt, kept after failure so Retry reconnects the
    /// same destination instead of falling back to the previous host.
    retry: Option<PendingConnection>,
    hosts_panel: Option<Entity<HostsPanel>>,
    _hosts_panel_subscription: Option<gpui::Subscription>,
    hosts_mode: bool,
    info: Option<ServerInfo>,
    provider_icons_request: Option<String>,
    directory_refresh: Option<paseo_desktop::model::DirectoryRefresh>,
    /// Provider identities from the daemon snapshot; the schedule form offers
    /// them as new-agent targets.
    provider_ids: Vec<String>,
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
    /// Session split layouts keyed by workspace id. The store persists them
    /// per daemon host + workspace; this map is the live copy for the host the
    /// window is connected to.
    splits: BTreeMap<String, SplitLayout>,
    split_store: SplitLayoutStore,
    split_area_px: Option<(f64, f64)>,
    pane_bounds: BTreeMap<String, gpui::Bounds<gpui::Pixels>>,
    split_drag: Option<SplitDrag>,
    tab_drag: Option<TabDrag>,
    focus: FocusHandle,
    error: Option<String>,
    selected_agent: Option<String>,
    /// This window's handle; activation defers route into the window thread.
    window_handle: Option<AnyWindowHandle>,
    /// A deep-link or notification target waiting to be admitted.
    pending_activation: Option<PendingActivation>,
    /// Per-agent timestamps of shown attention notifications, for dedup.
    attention_notified: BTreeMap<String, String>,
    /// Expected server id of an in-flight connect started by an activation.
    connecting_server_id: Option<String>,
    /// Workspace whose terminal inventory snapshot has arrived. A terminal
    /// activation is admitted only against its own workspace's inventory.
    terminal_inventory_workspace: Option<String>,
    /// The in-flight terminals subscribe and the workspace it was opened for;
    /// a snapshot is admitted only through this pair, never through the
    /// current selection.
    terminal_inventory_request: Option<(String, String)>,
    /// Last observed user input; the daemon expires client presence after it.
    last_user_activity: std::time::Instant,
    /// One conversation per open agent tab, keyed by agent id. Panes render
    /// their own tab, so two visible panes can show two agents at once.
    conversations: BTreeMap<String, Entity<crate::conversation::Conversation>>,
    /// Agent ids the daemon currently streams a timeline for. Resubscribed
    /// only when the set of open agent tabs changes.
    timeline_agents: BTreeSet<String>,
    composers: BTreeMap<String, Entity<crate::composer_view::Composer>>,
    tools: Option<Entity<WorkspaceTools>>,
    /// Raw workspace descriptors, so tools can read scripts without widening the Directory model.
    workspace_payloads: BTreeMap<String, Value>,
    explorer_drag: Option<ExplorerDrag>,
    explorer_container_width: f64,
    editors: BTreeMap<EditorKey, Entity<FileEditor>>,
    tab_rail: workspace_tab_row::RailCache,
    terminal_commands: terminal_commands::Commands,
    /// Retained browser surfaces keyed by the pane tab that owns them. A pane
    /// tab keeps its page and its own page tabs for the session, so moving
    /// panes, splitting, or switching workspaces does not reload the page.
    browsers: BTreeMap<String, Entity<crate::browser_view::BrowserSurface>>,
    /// Label subscriptions, so a page title reaches the pane strip that shows it.
    browser_subscriptions: BTreeMap<String, gpui::Subscription>,
    scripts_menu: Option<Entity<ScriptsMenu>>,
    _scripts_menu_subscription: Option<gpui::Subscription>,
    scripts_bounds: gpui::Bounds<gpui::Pixels>,
    workspace_menu: Option<WorkspaceMenu>,
    workspace_menu_bounds: gpui::Bounds<gpui::Pixels>,
    command_center: Option<Entity<crate::command_center::CommandCenter>>,
    command_center_events: Option<gpui::Subscription>,
    command_center_restore: Option<FocusHandle>,
    command_center_snapshot: Option<Arc<Vec<CommandRow>>>,
    command_center_snapshot_dirty: bool,
    sidebar_open: bool,
    window_visible: bool,
    /// Whether this window has keyboard focus. Visibility alone means the frame
    /// is drawn, which a background window also satisfies.
    window_active: bool,
    shared_settings: crate::settings_view::SharedSettings,
    settings_store: Option<paseo_desktop::settings::SettingsStore>,
    settings_store_error: Option<String>,
    pane: Option<Pane>,
    settings_pane: Option<Entity<crate::settings_view::SettingsPane>>,
    host_terminals: Option<Entity<crate::terminal_profile_settings_view::HostTerminals>>,
    host_terminals_events: Option<gpui::Subscription>,
    schedules_pane: Option<Entity<crate::schedules_view::SchedulesPane>>,
    schedule_form_pane: Option<Entity<crate::schedule_form_view::ScheduleFormPane>>,
    plugins_pane: Option<Entity<crate::plugins_view::PluginsPane>>,
    plugin_logs_pane: Option<Entity<crate::plugin_logs_view::PluginLogsPane>>,
    pending_plugin_logs_id: Option<String>,
    settings_pane_events: Option<gpui::Subscription>,
    schedules_pane_events: Vec<gpui::Subscription>,
    schedule_form_pane_events: Option<gpui::Subscription>,
    plugins_pane_events: Vec<gpui::Subscription>,
    plugin_logs_pane_events: Option<gpui::Subscription>,
    _visibility: gpui::Subscription,
    _activation: gpui::Subscription,
    _appearance: gpui::Subscription,
    /// App-level keystroke interception: a focused terminal consumes its keys
    /// before keystroke observers run, so presence has to see them earlier.
    _keystrokes: gpui::Subscription,
    _tools: Vec<gpui::Subscription>,
    editor_subscriptions: BTreeMap<EditorKey, gpui::Subscription>,
}

/// Full-window product panes that replace the terminal/conversation area.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pane {
    Settings,
    HostTerminals,
    Schedules,
    ScheduleForm,
    Plugins,
    PluginLogs,
}

/// What a sidebar row does. The browser is a pane tab, not a full-window pane,
/// so it needs a row action of its own.
#[derive(Clone, Copy, Debug)]
enum SidebarAction {
    Pane(Pane),
    NewWorkspace,
    History,
    Search,
    Browser,
}

/// An editor tab is one file in one workspace. Keying by path alone lets the
/// second workspace reuse the first workspace's buffer, so edits and the save
/// land in the other workspace's file.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct EditorKey {
    cwd: String,
    path: String,
}

impl EditorKey {
    fn new(cwd: &str, path: &str) -> Self {
        Self {
            cwd: cwd.to_owned(),
            path: path.to_owned(),
        }
    }

    fn tab_id(&self) -> String {
        format!(
            "file:{}",
            serde_json::to_string(&(&self.cwd, &self.path)).expect("serialize editor identity")
        )
    }
}

impl Drop for Paseo {
    fn drop(&mut self) {
        if let Some(host) = &self.host {
            host.close();
        }
    }
}

impl Paseo {
    pub fn new(target: Result<Target>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let window_handle = window.window_handle();
        let visibility = cx.observe_window_visibility(window, |this, visibility, _, cx| {
            if visibility.is_visible() {
                this.last_user_activity = std::time::Instant::now();
            }
            let visible = visibility.is_visible();
            if visible
                && !this.window_visible
                && matches!(this.hosts.phase, Some(HostPhase::Online))
            {
                for conversation in this.conversations.values() {
                    conversation.update(cx, |view, cx| view.resume(cx));
                }
            }
            // Rendered pane terminals stay live; retained views that no pane
            // renders release their glyph shapes until they are shown again.
            this.window_visible = visible;
            this.apply_terminal_visibility(cx);
            this.apply_browser_visibility(cx);
        });
        let activation = cx.observe_window_activation(window, |this, window, cx| {
            this.window_active = window.is_window_active();
            if !this.window_active && this.explorer_drag.take().is_some() {
                cx.notify();
            }
            if this.window_active {
                this.note_user_activity();
            }
            if this.window_visible {
                // Every visible pane's terminal is mounted; each one claims its
                // own PTY size, so none stays on a stale grid.
                let rendered = this.rendered_terminal_ids();
                for (id, terminal) in this.terminals.iter() {
                    if rendered.contains(id) {
                        terminal.update(cx, |view, cx| view.request_claim(cx));
                    }
                }
            }
        });
        let appearance = cx.observe_window_appearance(window, |this, _, cx| {
            this.apply_settings_changed(cx);
        });
        // The daemon drops client presence after three minutes without input and
        // switches that client to push. A focused terminal consumes its keys
        // (`stop_propagation`) before keystroke observers run, so presence
        // intercepts keys at the app level and keeps only this window's.
        let keystrokes = {
            let entity = cx.entity().downgrade();
            let window_id = window_handle.window_id();
            cx.intercept_keystrokes(move |_, window, cx| {
                if window.window_handle().window_id() == window_id {
                    let _ = entity.update(cx, |this, _| this.note_user_activity());
                }
            })
        };
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
        let (settings_store, settings_store_error, initial_settings) = match settings_store() {
            Ok(store) => match store.load() {
                Ok(settings) => (Some(store), None, settings),
                Err(error) => (
                    Some(store),
                    Some(format!("Couldn't load settings: {error:#}")),
                    paseo_desktop::settings::Settings::default(),
                ),
            },
            Err(error) => (
                None,
                Some(format!("Couldn't load settings: {error:#}")),
                paseo_desktop::settings::Settings::default(),
            ),
        };
        let shared_settings = crate::settings_view::SharedSettings::new(initial_settings);
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let layouts_path = paseo_desktop::client::default_home()
            .map(|home| home.join("native-workspace-layouts.json"))
            .unwrap_or_else(|_| std::env::temp_dir().join("paseo-native-workspace-layouts.json"));
        let (split_store, layout_error) = match SplitLayoutStore::load(layouts_path.clone()) {
            Ok(store) => (store, None),
            Err(error) => (
                SplitLayoutStore::at(layouts_path),
                Some(format!("Cannot load saved split layouts: {error:#}")),
            ),
        };
        let mut app = Self {
            connection: None,
            host_generation: 0,
            host: None,
            hosts: HostsState::load(),
            active_relay_daemon_key: None,
            local_discovery: target
                .as_ref()
                .is_ok_and(|target| target.is_local_discovery()),
            retry: target.as_ref().ok().cloned().map(PendingConnection::Target),
            hosts_panel: None,
            _hosts_panel_subscription: None,
            hosts_mode: false,
            info: None,
            provider_icons_request: None,
            directory_refresh: None,
            provider_ids: Vec::new(),
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
            // Sized for a split workspace: every rendered pane holds a view.
            terminals: TerminalCache::new(TERMINAL_CACHE_CAPACITY),
            splits: BTreeMap::new(),
            split_store,
            split_area_px: None,
            pane_bounds: BTreeMap::new(),
            split_drag: None,
            tab_drag: None,
            error: remembered_error
                .or(settings_store_error.clone())
                .or(layout_error),
            selected_agent: None,
            window_handle: Some(window_handle),
            pending_activation: None,
            attention_notified: BTreeMap::new(),
            connecting_server_id: None,
            terminal_inventory_workspace: None,
            terminal_inventory_request: None,
            last_user_activity: std::time::Instant::now(),
            conversations: BTreeMap::new(),
            timeline_agents: BTreeSet::new(),
            composers: BTreeMap::new(),
            tools: None,
            workspace_payloads: BTreeMap::new(),
            explorer_drag: None,
            explorer_container_width: 0.,
            editors: BTreeMap::new(),
            tab_rail: Default::default(),
            terminal_commands: Default::default(),
            browsers: BTreeMap::new(),
            browser_subscriptions: BTreeMap::new(),
            scripts_menu: None,
            _scripts_menu_subscription: None,
            scripts_bounds: gpui::Bounds::default(),
            workspace_menu: None,
            workspace_menu_bounds: gpui::Bounds::default(),
            command_center: None,
            command_center_events: None,
            command_center_restore: None,
            command_center_snapshot: None,
            command_center_snapshot_dirty: true,
            sidebar_open: true,
            window_visible: window.is_visible(),
            window_active: window.is_window_active(),
            shared_settings,
            settings_store,
            settings_store_error,
            pane: None,
            settings_pane: None,
            host_terminals: None,
            host_terminals_events: None,
            schedules_pane: None,
            schedule_form_pane: None,
            plugins_pane: None,
            plugin_logs_pane: None,
            pending_plugin_logs_id: None,
            settings_pane_events: None,
            schedules_pane_events: Vec::new(),
            schedule_form_pane_events: None,
            plugins_pane_events: Vec::new(),
            plugin_logs_pane_events: None,
            _visibility: visibility,
            _activation: activation,
            _appearance: appearance,
            _keystrokes: keystrokes,
            _tools: Vec::new(),
            editor_subscriptions: BTreeMap::new(),
            focus,
        };
        // The daemon's presence plan uses client heartbeats to pick the in-app
        // attention recipient and to suppress notifications for the target the
        // user is already looking at. `lastActivityAt` only advances on real
        // user input, so an idle client does not block push delivery.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(15))
                    .await;
                if this.update(cx, |this, cx| this.send_heartbeat(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        match target.and_then(|target| HostClient::start_with_target(target, None)) {
            Ok(host) => {
                let events = host.events.clone();
                app.hosts.phase = Some(HostPhase::Connecting);
                let connection = host.connection.clone();
                app.connection = Some(connection.clone());
                app.bind_tools(connection, window, cx);
                app.host = Some(host);
                let generation = app.host_generation;
                cx.spawn(async move |this, cx| {
                    while let Ok(event) = events.recv().await {
                        if this
                            .update(cx, |this, cx| {
                                if this.host_generation == generation {
                                    this.receive_host_event(event, cx);
                                }
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                })
                .detach();
            }
            Err(error) => {
                app.error = Some(format!("{error:#}"));
                app.hosts.phase = Some(HostPhase::Failed(format!("{error:#}")));
            }
        }
        app
    }

    fn bind_tools(
        &mut self,
        connection: Arc<Connection>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tools = cx.new(|cx| WorkspaceTools::new(connection, self.shared_settings.clone(), cx));
        self._tools.clear();
        self._tools.push(cx.subscribe_in(
            &tools,
            window,
            |this, _, event: &OpenFile, window, cx| {
                this.open_file(event.0.clone(), window, cx);
            },
        ));
        self._tools.push(cx.subscribe_in(
            &tools,
            window,
            |this, _, event: &ViewTerminal, window, cx| {
                this.select_terminal(event.0.clone(), window, cx);
            },
        ));
        self.tools = Some(tools);
    }

    /// Replaces the active host connection. Closing the old client never
    /// touches the daemon's terminals, and the new host never inherits the
    /// previous host's selection or view state.
    fn replace_host(&mut self, input: paseo_desktop::hosts::ConnectInput, cx: &mut Context<Self>) {
        if self
            .editors
            .values()
            .any(|editor| editor.read(cx).is_dirty())
            || self
                .composers
                .values()
                .any(|composer| !composer.read(cx).is_disposable())
        {
            self.hosts.message = Some((
                true,
                "Save or discard open drafts and file edits before switching hosts.".into(),
            ));
            self.hosts.add = None;
            cx.notify();
            return;
        }
        self.host_generation = self.host_generation.saturating_add(1);
        self.terminal_commands = Default::default();
        // The relay identity of the new target. Once the connection comes
        // online with negotiated protected security, every saved relay route to
        // this key is pinned and persisted (the security floor survives adopt).
        self.active_relay_daemon_key = match &input.target {
            paseo_desktop::hosts::HostTarget::Socket {
                offer: Some(offer), ..
            } => Some(offer.daemon_public_key_b64.clone()),
            _ => None,
        };
        if let Some(host) = self.host.take() {
            host.close();
        }
        self.close_pane(cx);
        self.connection = None;
        self.command_center = None;
        self.command_center_events = None;
        self.command_center_restore = None;
        self.command_center_snapshot = None;
        self.command_center_snapshot_dirty = true;
        self.tools = None;
        self._tools.clear();
        for editor in self.editors.values() {
            editor.update(cx, |view, _| view.closed());
        }
        self.editors.clear();
        self.editor_subscriptions.clear();
        self.workspace_payloads.clear();
        self.workspace_form = None;
        self.history = None;
        self.lifecycle_subscriptions.clear();
        self.directory_refresh = None;
        self.provider_icons_request = None;
        self.provider_ids.clear();
        self.startup_workspace = None;
        self.hosts.registrations = None;
        self.hosts.registration_requests.clear();
        self.hosts.registration_busy = None;
        self.hosts.observation_request = None;
        self.hosts.connected_hosts_supported = false;
        self.local_discovery = false;
        self.retry = Some(PendingConnection::Grammar {
            target: input.target.clone(),
            expected_server_id: input.expected_server_id.clone(),
        });
        self.hosts.active_server_id = input.expected_server_id.clone();
        self.connecting_server_id = input.expected_server_id.clone();
        self.hosts.observation_request = None;
        self.clear_conversations(cx);
        self.hide_terminal(cx);
        self.terminals.clear();
        self.splits.clear();
        // The browser surfaces belong to the host being dropped: their native
        // pages point at the previous daemon's pages and workspace tabs.
        self.browsers.clear();
        self.browser_subscriptions.clear();
        self.split_drag = None;
        self.explorer_drag = None;
        self.pane_bounds.clear();
        self.tab_drag = None;
        self.composers.clear();
        self.directory = Directory::default();
        self.selected_workspace = None;
        self.selected_agent = None;
        self.info = None;
        self.provider_images.clear();
        self.error = None;
        match HostClient::start(input) {
            Ok(host) => {
                let events = host.events.clone();
                self.connection = Some(host.connection.clone());
                let generation = self.host_generation;
                self.host = Some(host);
                self.hosts.phase = Some(HostPhase::Connecting);
                cx.spawn(async move |this, cx| {
                    while let Ok(event) = events.recv().await {
                        if this
                            .update(cx, |this, cx| {
                                if this.host_generation == generation {
                                    this.receive_host_event(event, cx);
                                }
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                })
                .detach();
            }
            Err(error) => {
                let message = format!("{error:#}");
                self.hosts.phase = Some(HostPhase::Failed(message.clone()));
                self.hosts.message = Some((true, message));
                self.hosts.add = None;
                self.hosts.active_server_id = None;
                // The dial itself failed: no identity is coming, so a stale
                // connecting marker must not hold targets in Wait.
                self.connecting_server_id = None;
                if let Some(outcome) = paseo_desktop::activation::connect_failure_outcome(
                    self.pending_activation.as_ref(),
                ) {
                    self.pending_activation = None;
                    self.error = Some(outcome.to_owned());
                }
            }
        }
        cx.notify();
    }

    fn receive_host_event(&mut self, event: HostRuntimeEvent, cx: &mut Context<Self>) {
        match event {
            HostRuntimeEvent::Connected(info) => self.host_ready(info, cx),
            HostRuntimeEvent::ConnectedWithoutIdentity(info) => self.host_ready(info, cx),
            HostRuntimeEvent::IdentityMismatch(mismatch) => {
                let message = mismatch.to_string();
                self.hosts.phase = Some(HostPhase::Failed(message.clone()));
                self.hosts.message = Some((true, message));
                self.hosts.add = None;
                self.hosts.active_server_id = None;
                self.connecting_server_id = None;
                // A host identity mismatch is fatal: the pending activation
                // will never be admitted on this connection.
                self.pending_activation = None;
                self.on_disconnected(cx);
            }
            HostRuntimeEvent::Event(event) => {
                self.receive(event, cx);
            }
            HostRuntimeEvent::Disconnected(error) => {
                let message = error.clone();
                if self.hosts.add.is_some() {
                    self.hosts.add = None;
                }
                self.hosts.phase = Some(HostPhase::Failed(message.clone()));
                self.on_disconnected(cx);
                if !self
                    .hosts
                    .message
                    .as_ref()
                    .is_some_and(|(danger, _)| *danger)
                {
                    self.hosts.message = Some((true, message));
                }
                // The connection is gone; an in-flight dial cannot complete.
                self.connecting_server_id = None;
                // A failed dial can never admit the target it was started
                // for: end that activation with an actionable error instead
                // of waiting forever.
                if let Some(outcome) = paseo_desktop::activation::connect_failure_outcome(
                    self.pending_activation.as_ref(),
                ) {
                    self.pending_activation = None;
                    self.error = Some(outcome.to_owned());
                }
                // A failed connection can no longer produce the identity a
                // pending target was waiting for, so its outcome is decided now.
                self.schedule_activation(cx);
            }
        }
        cx.notify();
    }

    fn on_disconnected(&mut self, cx: &mut Context<Self>) {
        self.terminal_commands.state.disconnected();
        self.terminal_commands.profiles_subscribed = false;
        if let Some(view) = &self.host_terminals {
            view.update(cx, |view, cx| view.disconnected(cx));
        }
        if let Some(view) = &self.command_center {
            view.update(cx, |view, cx| view.disconnected(cx));
        }
        // The transport reset dropped every subscription, so a captured
        // terminals request id can never be answered again.
        self.terminal_inventory_workspace = None;
        self.terminal_inventory_request = None;
        self.timeline_agents.clear();
        if let Some(form) = &self.workspace_form {
            form.update(cx, |view, cx| view.disconnected(cx));
        }
        if let Some(history) = &self.history {
            history.update(cx, |view, cx| view.disconnected(cx));
        }
        self.directory.clear_provider_icons();
        self.provider_images.clear();
        self.provider_icons_request = None;
        for terminal in self.terminals.values() {
            terminal.update(cx, |view, cx| view.disconnected(cx));
        }
        for conversation in self.conversations.values() {
            conversation.update(cx, |view, cx| view.disconnected(cx));
        }
        for composer in self.composers.values() {
            composer.update(cx, |view, cx| view.disconnected(cx));
        }
        for editor in self.editors.values() {
            editor.update(cx, |view, cx| view.disconnected(cx));
        }
        if let Some(tools) = &self.tools {
            tools.update(cx, |view, cx| view.disconnected(cx));
        }
        self.scripts_menu = None;
        self.workspace_menu = None;
        if let Some(pane) = &self.schedules_pane {
            pane.update(cx, |view, cx| view.disconnected("Host disconnected.", cx));
        }
        if let Some(pane) = &self.plugins_pane {
            pane.update(cx, |view, cx| view.disconnected("Host disconnected.", cx));
        }
        if let Some(pane) = &self.schedule_form_pane {
            pane.update(cx, |view, cx| view.disconnected(cx));
        }
        if let Some(pane) = &self.plugin_logs_pane {
            pane.update(cx, |view, cx| view.disconnected("Host disconnected.", cx));
        }
        self.hosts.registrations = None;
        self.hosts.registration_busy = None;
        self.hosts.registration_requests.clear();
        self.hosts.observation_request = None;
        self.hosts.connected_hosts_supported = false;
    }

    fn host_ready(&mut self, info: ServerInfo, cx: &mut Context<Self>) {
        let learned = info.server_id.clone();
        let supports_connected_hosts =
            info.features.get("connectedHosts") == Some(&Value::Bool(true));
        self.hosts.connected_hosts_supported = supports_connected_hosts;
        if self.local_discovery {
            self.hosts.local_server_id = Some(learned.clone());
            self.local_discovery = false;
        }
        self.hosts.phase = Some(HostPhase::Online);
        self.hosts.active_server_id = Some(learned.clone());
        self.hosts.message = None;
        self.connecting_server_id = None;
        if let Some(attempt) = self.hosts.add.take() {
            // The connect & save attempt learned its identity: save the host.
            match connection_from_grammar(&attempt.grammar) {
                Some(connection) => {
                    let profiles = std::mem::take(&mut self.hosts.profiles);
                    let result = upsert_host_connection_in_profiles(
                        profiles.clone(),
                        UpsertInput {
                            server_id: learned.clone(),
                            label: None,
                            connection,
                            password: attempt.password.clone(),
                        },
                    );
                    match result {
                        Ok(next) => {
                            self.hosts.profiles = next;
                            if self.hosts.persist() {
                                self.hosts.message =
                                    Some((false, format!("Connected to {learned}. Host saved.")));
                            }
                        }
                        Err(error) => {
                            self.hosts.profiles = profiles;
                            self.hosts.message = Some((
                                true,
                                format!(
                                    "Connected to {learned}, but the host was not saved: {error:#}"
                                ),
                            ));
                        }
                    }
                }
                None => {
                    self.hosts.message = Some((
                        false,
                        format!(
                            "Connected to {learned}, but this address cannot be saved as a host."
                        ),
                    ));
                }
            }
        }
        // Adopt-time persistence of the protected floor: with the daemon
        // identity authenticated (online) and the handshake protected, pin
        // every saved relay route to this daemon key. A failed or probe-only
        // connection never reaches host_ready, so it never writes a pin.
        if let Some(key) = self.active_relay_daemon_key.clone()
            && self
                .host
                .as_ref()
                .is_some_and(|host| host.connection.negotiated_security() == Some("protected"))
        {
            let pinned = paseo_desktop::hosts::profile::pin_relay_protected_security_in_profiles(
                self.hosts.profiles.clone(),
                &learned,
                &key,
            );
            if pinned != self.hosts.profiles {
                self.hosts.profiles = pinned;
                self.hosts.persist();
            }
        }
        self.receive(Event::Ready(info), cx);
        if let Some(view) = &self.host_terminals {
            view.update(cx, |view, cx| view.load(cx));
        }
        if self.terminal_commands.state.demanded {
            self.load_terminal_profiles(true, cx);
        }
        self.hosts.observation_request = None;
        if supports_connected_hosts {
            if let Some(host) = &self.host {
                match daemon_registrations::observe_request(&host.connection) {
                    Ok(request_id) => self.hosts.observation_request = Some(request_id),
                    Err(error) => {
                        self.error =
                            Some(format!("Cannot observe daemon registrations: {error:#}"));
                    }
                }
            }
        } else {
            self.hosts.registrations = None;
        }
        self.schedule_activation(cx);
    }

    fn handle_hosts_event(&mut self, event: HostsPanelEvent, cx: &mut Context<Self>) {
        match event {
            HostsPanelEvent::Connect { server_id } => self.connect_saved_host(&server_id, cx),
            HostsPanelEvent::Retry => self.retry_connection(cx),
            HostsPanelEvent::AddHost { grammar, password } => self.add_host(grammar, password, cx),
            HostsPanelEvent::RemoveHost { server_id } => {
                self.hosts.profiles = paseo_desktop::hosts::registry::remove_profile(
                    &self.hosts.profiles,
                    &server_id,
                );
                if self.hosts.active_server_id.as_deref() == Some(server_id.as_str()) {
                    // The active connection stays; only the saved entry goes.
                    self.hosts.active_server_id = None;
                }
                self.hosts.persist();
            }
            HostsPanelEvent::RegisterHost { grammar } => self.register_host(&grammar, cx),
            HostsPanelEvent::RemoveRegistration { registration_id } => {
                self.remove_registration(&registration_id, cx)
            }
        }
        cx.notify();
    }

    fn connect_saved_host(&mut self, server_id: &str, cx: &mut Context<Self>) {
        let Some(profile) = self
            .hosts
            .profiles
            .iter()
            .find(|profile| profile.server_id == server_id)
            .cloned()
        else {
            return;
        };
        let Some(grammar) = HostsState::connect_grammar(&profile) else {
            self.hosts.message = Some((
                true,
                "This saved host needs a transport the native client does not have yet: unix socket, named pipe or routed.".into(),
            ));
            return;
        };
        self.hosts.message = None;
        self.hosts.add = None;
        match parse_host_target(&grammar) {
            Ok(target) => self.replace_host(
                paseo_desktop::hosts::ConnectInput {
                    target: target.with_password(profile.password.clone()),
                    expected_server_id: Some(server_id.to_owned()),
                },
                cx,
            ),
            Err(error) => {
                let message = format!("{error:#}");
                self.hosts.phase = Some(HostPhase::Failed(message.clone()));
                self.hosts.message = Some((true, message));
            }
        }
    }

    fn add_host(&mut self, grammar: String, password: Option<String>, cx: &mut Context<Self>) {
        let target = match parse_host_target(&grammar) {
            Ok(target) => target,
            Err(error) => {
                let message = format!("{error:#}");
                self.hosts.message = Some((true, message));
                return;
            }
        };
        // A password typed in the separate field and one parsed from the
        // grammar are the same credential: pick one and use it for both the
        // handshake and the saved profile.
        let password = password
            .filter(|password| !password.is_empty())
            .or_else(|| target.password().map(str::to_owned));
        let target = target.with_password(password.clone());
        self.hosts.message = None;
        self.hosts.add = Some(AddAttempt { grammar, password });
        self.replace_host(
            paseo_desktop::hosts::ConnectInput {
                target,
                expected_server_id: None,
            },
            cx,
        );
    }

    fn retry_connection(&mut self, cx: &mut Context<Self>) {
        if let Some(attempt) = self.hosts.add.clone() {
            self.add_host(attempt.grammar, attempt.password, cx);
            return;
        }
        match self.retry.clone() {
            Some(PendingConnection::Target(target)) => {
                self.start_initial_target(target, cx);
            }
            Some(PendingConnection::Grammar {
                target,
                expected_server_id,
            }) => self.replace_host(
                paseo_desktop::hosts::ConnectInput {
                    target,
                    expected_server_id,
                },
                cx,
            ),
            None => {}
        }
    }

    fn start_initial_target(&mut self, target: Target, cx: &mut Context<Self>) {
        if self.terminals.values().next().is_some()
            || !self.composers.is_empty()
            || !self.editors.is_empty()
        {
            self.hosts.message = Some((true, "This connection is already restoring open tabs. Wait for automatic reconnect, or save your drafts before changing the host.".into()));
            cx.notify();
            return;
        }
        self.host_generation = self.host_generation.saturating_add(1);
        self.terminal_commands = Default::default();
        if let Some(host) = self.host.take() {
            host.close();
        }
        self.tools = None;
        self._tools.clear();
        self.command_center = None;
        self.command_center_events = None;
        self.command_center_restore = None;
        self.command_center_snapshot = None;
        self.command_center_snapshot_dirty = true;
        self.connection = None;
        self.connecting_server_id = None;
        self.local_discovery = target.is_local_discovery();
        self.retry = Some(PendingConnection::Target(target.clone()));
        match HostClient::start_with_target(target, None) {
            Ok(host) => {
                let events = host.events.clone();
                self.connection = Some(host.connection.clone());
                self.host = Some(host);
                self.hosts.phase = Some(HostPhase::Connecting);
                let generation = self.host_generation;
                cx.spawn(async move |this, cx| {
                    while let Ok(event) = events.recv().await {
                        if this
                            .update(cx, |this, cx| {
                                if this.host_generation == generation {
                                    this.receive_host_event(event, cx);
                                }
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                })
                .detach();
            }
            Err(error) => {
                let message = format!("{error:#}");
                self.hosts.phase = Some(HostPhase::Failed(message.clone()));
                self.hosts.message = Some((true, message));
            }
        }
        cx.notify();
    }

    fn register_host(&mut self, grammar: &str, cx: &mut Context<Self>) {
        if !self.hosts.connected_hosts_supported {
            self.hosts.registration_error = Some(
                "This daemon does not manage connected hosts. Update the daemon on that host first."
                    .into(),
            );
            cx.notify();
            return;
        }
        if !matches!(self.hosts.phase, Some(HostPhase::Online)) {
            self.hosts.registration_error = Some(
                "Wait for this host's identity to be verified before managing registrations."
                    .into(),
            );
            cx.notify();
            return;
        }
        let Some(host) = &self.host else {
            self.hosts.registration_error =
                Some("Connect to a host before registering another daemon on it.".into());
            return;
        };
        match daemon_registrations::add_request(&host.connection, grammar, None) {
            Ok(request_id) => {
                self.hosts.registration_requests.insert(request_id.clone());
                self.hosts.registration_busy = Some("Registering host on the daemon…".into());
                self.hosts.registration_error = None;
                self.watch_registration(request_id, "Registration", cx);
            }
            Err(error) => {
                self.hosts.registration_error = Some(format!("Registration failed: {error:#}"));
            }
        }
    }

    fn remove_registration(&mut self, registration_id: &str, cx: &mut Context<Self>) {
        if !self.hosts.connected_hosts_supported {
            self.hosts.registration_error = Some(
                "This daemon does not manage connected hosts. Update the daemon on that host first."
                    .into(),
            );
            cx.notify();
            return;
        }
        if !matches!(self.hosts.phase, Some(HostPhase::Online)) {
            self.hosts.registration_error = Some(
                "Wait for this host's identity to be verified before managing registrations."
                    .into(),
            );
            cx.notify();
            return;
        }
        let Some(host) = &self.host else {
            return;
        };
        match daemon_registrations::remove_request(&host.connection, registration_id) {
            Ok(request_id) => {
                self.hosts.registration_requests.insert(request_id.clone());
                self.hosts.registration_busy = Some("Removing registration…".into());
                self.hosts.registration_error = None;
                self.watch_registration(request_id, "Removal", cx);
            }
            Err(error) => {
                self.hosts.registration_error = Some(format!("Removal failed: {error:#}"));
            }
        }
    }

    /// A daemon that never answers must not leave the panel busy forever. The
    /// request id is removed on timeout, so a late response is ignored.
    fn watch_registration(
        &mut self,
        request_id: String,
        action: &'static str,
        cx: &mut Context<Self>,
    ) {
        let timer = cx
            .background_executor()
            .timer(std::time::Duration::from_secs(20));
        cx.spawn(async move |this, cx| {
            timer.await;
            let _ = this.update(cx, |this, cx| {
                if this.hosts.registration_requests.remove(&request_id) {
                    this.hosts.registration_busy = None;
                    this.hosts.registration_error = Some(format!(
                        "{action} timed out. The host did not answer; its directory refreshes when it does."
                    ));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn receive(&mut self, event: Event, cx: &mut Context<Self>) {
        let result = match event {
            Event::Ready(info) => {
                // Initial restore only. Reconnect restores the active view without navigating again.
                let changed_host = self
                    .info
                    .as_ref()
                    .is_none_or(|current| current.server_id != info.server_id);
                let host_id = info.server_id.clone();
                if changed_host {
                    self.startup_workspace = self.remembered.get(&host_id).map(str::to_owned);
                }
                self.directory.set_host(&host_id);
                self.provider_images.clear();
                // Feature gates are read once per daemon introduction.
                if let Some(tools) = &self.tools {
                    tools.update(cx, |tools, _| tools.set_features(&info.features));
                }
                self.info = Some(info);
                if let Some(view) = &self.command_center {
                    view.update(cx, |view, cx| view.reconnected(cx));
                }
                if let Some(tools) = &self.tools {
                    tools.update(cx, |tools, cx| tools.reconnected(cx));
                }
                for editor in self.editors.values() {
                    editor.update(cx, |editor, cx| editor.reconnected(cx));
                }
                if changed_host {
                    // Layouts belong to one daemon host; workspace ids are not
                    // globally unique across hosts.
                    self.splits.clear();
                    self.browsers.clear();
                    self.browser_subscriptions.clear();
                    self.clear_conversations(cx);
                }
                for (workspace, layout) in self.split_store.layouts_for(&host_id) {
                    // A layout that already changed during this session wins
                    // over the version that was on disk at startup.
                    self.splits.entry(workspace).or_insert(layout);
                }
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
                let restored = self.restore_observations(cx);
                // The named host may be the activation's target; admission
                // still waits for its directory pages to complete.
                self.schedule_activation(cx);
                restored
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
            Event::Binary(bytes) => {
                // Binary frames carry their own request ownership; the first
                // conversation whose image cache claims the frame consumes it.
                for conversation in self.conversations.values() {
                    if conversation.update(cx, |view, cx| view.receive_binary(&bytes, cx)) {
                        break;
                    }
                }
                return;
            }
            Event::Disconnected(error) => {
                self.on_disconnected(cx);
                self.error = Some(error.clone());
                self.hosts.phase = Some(HostPhase::Failed(error));
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
            let request_id = connection.subscribe(
                "terminals",
                "subscribe_terminals_request",
                json!({"cwd":cwd,"workspaceId":workspace.id}),
            )?;
            self.terminal_inventory_request = Some((request_id, workspace.id.clone()));
        }
        // Attention notifications: the daemon picks one in-app recipient via
        // client presence; `notifications: true` makes this client a
        // candidate. A host that refuses the subscription still works without
        // OS notifications, so this stays non-fatal.
        if let Err(error) = connection.subscribe(
            "attention",
            "session.events.set_subscription.request",
            json!({"events":["agent_attention_required","terminal_attention_required"],"notifications":true}),
        ) {
            self.error = Some(format!("Cannot subscribe to attention notifications: {error}"));
        }
        for terminal in self.terminals.values() {
            terminal.update(cx, |view, cx| view.reconnect(cx));
        }
        for composer in self.composers.values() {
            composer.update(cx, |view, cx| view.reconnected(cx));
        }
        self.sync_timeline_subscription();
        for conversation in self.conversations.values() {
            conversation.update(cx, |view, cx| view.resume(cx));
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
        let connection = &self
            .host
            .as_ref()
            .context("host is disconnected")?
            .connection;
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

    /// Appearance, font, or layout settings changed. Every visible consumer
    /// reads the shared settings during render, so notify the live entities.
    fn apply_settings_changed(&mut self, cx: &mut Context<Self>) {
        if let Some(view) = &self.command_center {
            view.update(cx, |_, cx| cx.notify());
        }
        for terminal in self.terminals.values() {
            terminal.update(cx, |view, cx| view.settings_changed(cx));
        }
        for conversation in self.conversations.values() {
            conversation.update(cx, |view, cx| view.settings_changed(cx));
        }
        for composer in self.composers.values() {
            composer.update(cx, |view, cx| view.settings_changed(cx));
        }
        if let Some(pane) = &self.host_terminals {
            pane.update(cx, |pane, cx| pane.settings_changed(cx));
        }
        if let Some(pane) = &self.settings_pane {
            pane.update(cx, |_, cx| cx.notify());
        }
        if let Some(pane) = &self.schedules_pane {
            pane.update(cx, |_, cx| cx.notify());
        }
        if let Some(pane) = &self.schedule_form_pane {
            pane.update(cx, |_, cx| cx.notify());
        }
        if let Some(pane) = &self.plugins_pane {
            pane.update(cx, |_, cx| cx.notify());
        }
        if let Some(pane) = &self.plugin_logs_pane {
            pane.update(cx, |_, cx| cx.notify());
        }
        if let Some(tools) = &self.tools {
            tools.update(cx, |_, cx| cx.notify());
        }
        if let Some(menu) = &self.scripts_menu {
            menu.update(cx, |_, cx| cx.notify());
        }
        cx.notify();
    }

    fn close_pane(&mut self, cx: &mut Context<Self>) {
        if self.pane.take().is_some() {
            self.settings_pane = None;
            self.host_terminals = None;
            self.host_terminals_events = None;
            self.schedules_pane = None;
            self.schedule_form_pane = None;
            self.plugins_pane = None;
            self.plugin_logs_pane = None;
            self.pending_plugin_logs_id = None;
            self.settings_pane_events = None;
            self.schedules_pane_events = Vec::new();
            self.schedule_form_pane_events = None;
            self.plugins_pane_events.clear();
            self.plugin_logs_pane_events = None;
            cx.notify();
        }
    }

    fn open_pane(&mut self, pane: Pane, window: &mut Window, cx: &mut Context<Self>) {
        if self.pane == Some(pane) {
            return;
        }
        let plugin_logs_id = self.pending_plugin_logs_id.take();
        self.close_pane(cx);
        self.pending_plugin_logs_id = plugin_logs_id;
        self.pane = Some(pane);
        match pane {
            Pane::HostTerminals => {
                if let Some(connection) = self.connection.clone() {
                    let host = self
                        .info
                        .as_ref()
                        .map(|info| {
                            self.hosts
                                .profiles
                                .iter()
                                .find(|host| host.server_id == info.server_id)
                                .map(|host| host.label.clone())
                                .or_else(|| info.hostname.clone())
                                .unwrap_or_else(|| info.server_id.clone())
                        })
                        .unwrap_or_default();
                    let shared = self.shared_settings.clone();
                    let view = cx.new(|cx| {
                        crate::terminal_profile_settings_view::HostTerminals::new(
                            connection, host, shared, cx,
                        )
                    });
                    self.host_terminals_events = Some(cx.subscribe_in(
                        &view,
                        window,
                        |this,
                         sender,
                         event: &crate::terminal_profile_settings_view::Event,
                         window,
                         cx| {
                            match event {
                                crate::terminal_profile_settings_view::Event::Close => {
                                    this.close_pane(cx);
                                    // Settings took keyboard focus while the PTY
                                    // ID stayed selected. Re-activate the retained
                                    // view instead of taking sync's same-ID exit.
                                    this.selected_terminal = None;
                                    this.sync_active_tab(window, cx);
                                }
                                crate::terminal_profile_settings_view::Event::Focus => {
                                    sender.read(cx).focus_handle(cx).focus(window, cx);
                                    window.activate_window();
                                }
                            }
                            cx.notify();
                        },
                    ));
                    view.read(cx).focus_handle(cx).focus(window, cx);
                    self.host_terminals = Some(view);
                }
            }
            Pane::Settings => {
                let store = self.settings_store.clone();
                let store_error = self.settings_store_error.clone();
                let shared = self.shared_settings.clone();
                let view = cx.new(|cx| {
                    crate::settings_view::SettingsPane::new(store, store_error, shared, window, cx)
                });
                self.settings_pane_events = Some(cx.subscribe_in(
                    &view,
                    window,
                    |this, _, event: &crate::settings_view::SettingsPaneEvent, window, cx| {
                        match event {
                            crate::settings_view::SettingsPaneEvent::Changed => {
                                this.apply_settings_changed(cx)
                            }
                            crate::settings_view::SettingsPaneEvent::Close => {
                                this.close_pane(cx);
                            }
                            crate::settings_view::SettingsPaneEvent::OpenPlugins => {
                                this.open_pane(Pane::Plugins, window, cx);
                            }
                        }
                    },
                ));
                self.settings_pane = Some(view);
            }
            Pane::Schedules => {
                let connection = self.connection.clone();
                let shared = self.shared_settings.clone();
                let view =
                    cx.new(|cx| crate::schedules_view::SchedulesPane::new(connection, shared, cx));
                self.schedules_pane_events = vec![
                    cx.subscribe_in(
                        &view,
                        window,
                        |this, _, event: &crate::schedules_view::SchedulesPaneEvent, window, cx| {
                            match event {
                                crate::schedules_view::SchedulesPaneEvent::Close => {
                                    this.close_pane(cx);
                                }
                                crate::schedules_view::SchedulesPaneEvent::NewSchedule => {
                                    this.open_pane(Pane::ScheduleForm, window, cx);
                                }
                            }
                        },
                    ),
                    cx.subscribe_in(
                        &view,
                        window,
                        |this,
                         _,
                         event: &crate::schedules_view::SchedulesEditRequest,
                         window,
                         cx| {
                            let schedule = event.0.clone();
                            this.open_schedule_form(Some(schedule), window, cx);
                        },
                    ),
                ];
                self.schedules_pane = Some(view);
            }
            Pane::ScheduleForm => {
                self.open_schedule_form(None, window, cx);
            }
            Pane::Plugins => {
                let connection = self.connection.clone();
                let shared = self.shared_settings.clone();
                let view =
                    cx.new(|cx| crate::plugins_view::PluginsPane::new(connection, shared, cx));
                self.plugins_pane_events.push(cx.subscribe_in(
                    &view,
                    window,
                    |this, _, event: &crate::plugins_view::PluginsPaneEvent, _window, cx| {
                        match event {
                            crate::plugins_view::PluginsPaneEvent::Close => this.close_pane(cx),
                        }
                    },
                ));
                let logs_subscription = cx.subscribe_in(
                    &view,
                    window,
                    |this, _, event: &crate::plugins_view::PluginLogsRequest, window, cx| {
                        this.pending_plugin_logs_id = Some(event.0.clone());
                        this.open_pane(Pane::PluginLogs, window, cx);
                    },
                );
                self.plugins_pane_events.push(logs_subscription);
                self.plugins_pane = Some(view);
            }
            Pane::PluginLogs => {
                let connection = self.connection.clone();
                let shared = self.shared_settings.clone();
                let plugin_id = self.pending_plugin_logs_id.take().unwrap_or_default();
                let view = cx.new(|cx| {
                    crate::plugin_logs_view::PluginLogsPane::new(connection, shared, plugin_id, cx)
                });
                self.plugin_logs_pane_events = Some(cx.subscribe_in(
                    &view,
                    window,
                    |this, _, _: &crate::plugin_logs_view::PluginLogsPaneEvent, window, cx| {
                        // Back to the plugin list.
                        this.open_pane(Pane::Plugins, window, cx);
                    },
                ));
                self.plugin_logs_pane = Some(view);
            }
        }
        cx.notify();
    }

    /// Records real user input; daemon presence expires without it.
    fn note_user_activity(&mut self) {
        self.last_user_activity = std::time::Instant::now();
    }

    /// A notification click or deep link named a target for this window. The
    /// target waits in the pending model until its named host is the connected
    /// one and the directory is loaded, so activation never mutates the wrong
    /// host.
    pub fn activate(
        &mut self,
        target: ActivationTarget,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pending_activation = Some(PendingActivation::from_target(target));
        self.schedule_activation(cx);
    }

    /// The session in the focused pane. A product pane or focused editor must
    /// not report a previously selected Agent or terminal as watched.
    fn displayed_session(&self) -> activation::DisplayedSession<'_> {
        activation::displayed_session(
            self.pane.is_some(),
            self.selected_workspace
                .as_ref()
                .and_then(|id| self.splits.get(id))
                .and_then(|layout| layout.focused_pane())
                .and_then(|pane| pane.focused_tab.as_deref())
                .is_some_and(|tab| self.pane_tab_kind(tab) == Some(PaneTabKind::File)),
            self.selected_agent
                .as_deref()
                .filter(|id| self.conversations.contains_key(*id)),
            self.terminal
                .as_ref()
                .and(self.selected_terminal.as_deref()),
        )
    }

    fn view_watched(&self) -> bool {
        activation::window_watched(self.window_visible, self.window_active)
    }

    fn admitted_server_id(&self) -> Option<String> {
        self.connection
            .as_ref()
            .filter(|connection| connection.connected())
            .and(self.info.as_ref().map(|info| info.server_id.clone()))
    }

    /// The launch connection exists but its handshake has not named the host
    /// yet, so a pending target must not be judged against a missing identity.
    fn identity_pending(&self) -> bool {
        self.info.is_none()
            && self.host.is_some()
            && matches!(self.hosts.phase, Some(HostPhase::Connecting))
    }

    fn window_id(&self) -> u64 {
        self.window_handle
            .map(|handle| handle.window_id().as_u64())
            .unwrap_or_default()
    }

    fn activation_step(&self) -> Option<ActivationStep> {
        let pending = self.pending_activation.as_ref()?;
        let saved_hosts: Vec<String> = self
            .hosts
            .profiles
            .iter()
            .map(|profile| profile.server_id.clone())
            .collect();
        let admitted = self.admitted_server_id();
        let context = paseo_desktop::activation::ActivationContext {
            connected_server_id: admitted.as_deref(),
            connecting_server_id: self.connecting_server_id.as_deref(),
            identity_pending: self.identity_pending(),
            directory_complete: self.info.is_some() && self.directory_refresh.is_none(),
            terminal_inventory_workspace: self.terminal_inventory_workspace.as_deref(),
            selected_workspace: self.selected_workspace.as_deref(),
            saved_hosts: &saved_hosts,
        };
        Some(pending.advance(context, &self.directory))
    }

    /// Runs a decided activation on the window thread; host events and
    /// directory updates arrive from the connection task without a window
    /// borrow.
    fn schedule_activation(&mut self, cx: &mut Context<Self>) {
        if matches!(self.activation_step(), Some(ActivationStep::Wait) | None) {
            return;
        }
        let Some(handle) = self.window_handle else {
            return;
        };
        cx.defer(move |cx| {
            if let Some(typed) = handle.downcast::<Paseo>() {
                let _ = typed.update(cx, |paseo, window, cx| {
                    paseo.consume_pending_activation(window, cx)
                });
            }
        });
    }

    fn consume_pending_activation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(step) = self.activation_step() else {
            return;
        };
        match step {
            ActivationStep::Wait => {}
            ActivationStep::ConnectHost { server_id } => {
                if let Some(pending) = self.pending_activation.as_mut() {
                    pending.begin_connecting();
                }
                self.connect_saved_host(&server_id, cx);
            }
            ActivationStep::Unavailable(message) => {
                self.pending_activation = None;
                self.error = Some(message);
                cx.notify();
            }
            ActivationStep::FocusHost => {
                self.pending_activation = None;
                cx.notify();
            }
            ActivationStep::OpenAgent { agent_id } => {
                self.pending_activation = None;
                self.select_agent(agent_id, window, cx);
            }
            ActivationStep::SelectWorkspace { workspace_id } => {
                // Switching loads that workspace's terminal inventory; the target
                // stays pending until the inventory can admit its terminal.
                self.select_workspace(workspace_id, cx);
            }
            ActivationStep::OpenTerminal {
                workspace_id,
                terminal_id,
            } => {
                self.select_workspace(workspace_id, cx);
                self.select_terminal(terminal_id.clone(), window, cx);
                // Consume only once the terminal is on screen. A workspace
                // whose inventory has not arrived keeps the target pending.
                if self.selected_terminal.as_deref() == Some(terminal_id.as_str()) {
                    self.pending_activation = None;
                }
            }
        }
    }

    fn receive_agent_attention(&mut self, payload: &Value, cx: &mut Context<Self>) {
        let agent_id = payload["agentId"].as_str().unwrap_or_default().to_owned();
        let displayed = self.displayed_session();
        let decision = paseo_desktop::activation::agent_attention(
            payload,
            displayed.agent_id,
            self.view_watched(),
            self.attention_notified
                .get(agent_id.as_str())
                .map(String::as_str),
            self.admitted_server_id().as_deref().unwrap_or_default(),
            self.window_id(),
        );
        if let AttentionDecision::Show(notification) = decision {
            if let Some(timestamp) = notification.timestamp {
                if self.attention_notified.len() >= 64 {
                    if let Some(oldest) = self.attention_notified.keys().next().cloned() {
                        self.attention_notified.remove(&oldest);
                    }
                }
                self.attention_notified.insert(agent_id, timestamp);
            }
            cx.show_system_notification(SystemNotification {
                tag: notification.tag.into(),
                title: notification.title.into(),
                body: notification.body.into(),
                actions: Vec::new(),
            });
        }
    }

    fn receive_terminal_attention(&mut self, payload: &Value, cx: &mut Context<Self>) {
        let admitted = self.admitted_server_id();
        if let AttentionDecision::Show(notification) = paseo_desktop::activation::terminal_attention(
            payload,
            admitted.as_deref().unwrap_or_default(),
            self.window_id(),
        ) {
            cx.show_system_notification(SystemNotification {
                tag: notification.tag.into(),
                title: notification.title.into(),
                body: notification.body.into(),
                actions: Vec::new(),
            });
        }
    }

    /// The daemon's presence plan uses client heartbeats to pick the in-app
    /// attention recipient and to suppress notifications for the target the
    /// user is already looking at. `lastActivityAt` only advances on real user
    /// input (keys, pointer, wheel, window activation), so an idle client does
    /// not block push delivery.
    fn send_heartbeat(&self, _: &mut Context<Self>) {
        let Some(connection) = &self.connection else {
            return;
        };
        if !connection.connected() {
            return;
        }
        let idle = self.last_user_activity.elapsed();
        let last_activity_at =
            chrono::Utc::now() - chrono::Duration::from_std(idle).unwrap_or_default();
        // Report the session the user can actually see; a product pane or an
        // editor tab leaves the selection behind it on screen nowhere.
        let displayed = self.displayed_session();
        let _ = connection.send_session(json!({
            "type": "client_heartbeat",
            "deviceType": "web",
            "focusedAgentId": displayed.agent_id,
            "focusedTerminalId": displayed.terminal_id,
            "lastActivityAt":
                last_activity_at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            "appVisible": self.view_watched(),
        }));
    }

    fn terminal_view_focused(&self, window: &Window, cx: &App) -> bool {
        self.terminals
            .values()
            .any(|view| view.read(cx).focus_handle(cx).is_focused(window))
    }

    fn command_center_open(&self, cx: &App) -> bool {
        self.command_center
            .as_ref()
            .is_some_and(|view| view.read(cx).is_open())
    }
    fn command_center_cwd(&self) -> Option<String> {
        self.selected_workspace
            .as_ref()
            .and_then(|id| self.directory.workspaces.get(id))
            .map(|workspace| {
                workspace
                    .workspace_directory
                    .clone()
                    .unwrap_or_else(|| workspace.project_root_path.clone())
            })
    }
    fn command_center_rows(&mut self) -> Arc<Vec<CommandRow>> {
        if self.command_center_snapshot_dirty || self.command_center_snapshot.is_none() {
            self.command_center_snapshot = Some(Arc::new(self.build_command_center_rows()));
            self.command_center_snapshot_dirty = false;
        }
        self.command_center_snapshot.as_ref().unwrap().clone()
    }
    fn build_command_center_rows(&self) -> Vec<CommandRow> {
        let mut rows = vec![];
        for (id, title, icon, keywords, query_only) in [
            (
                "new-workspace",
                "New workspace",
                "plus",
                "new workspace worktree branch",
                false,
            ),
            (
                "history",
                "History",
                "history",
                "history sessions recent",
                false,
            ),
            (
                "schedules",
                "Schedules",
                "calendar-clock",
                "schedules scheduled automation recurring",
                false,
            ),
            (
                "settings",
                "Settings",
                "settings",
                "settings preferences appearance",
                false,
            ),
            (
                "toggle-left-sidebar",
                "Toggle left sidebar",
                "panel-left",
                "toggle sidebar left panel workspaces",
                true,
            ),
            (
                "toggle-explorer",
                "Toggle Explorer sidebar",
                "panel-right",
                "toggle explorer right sidebar files changes",
                true,
            ),
        ] {
            rows.push(CommandRow {
                id: format!("action:{id}"),
                title: title.into(),
                subtitle: String::new(),
                target: CommandTarget::Action(id.into()),
                icon: icon.into(),
                keywords: keywords.split_whitespace().map(str::to_owned).collect(),
                activity: 0,
                query_only,
            });
        }
        for workspace in self
            .directory
            .workspaces
            .values()
            .filter(|workspace| workspace.available())
        {
            let payload = self.workspace_payloads.get(&workspace.id);
            let branch = payload.and_then(|payload| payload["currentBranch"].as_str());
            let subtitle = [Some(workspace.project_display_name.as_str()), branch]
                .into_iter()
                .flatten()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" · ");
            let activity = payload
                .and_then(|payload| payload["statusEnteredAt"].as_str())
                .and_then(|date| chrono::DateTime::parse_from_rfc3339(date).ok())
                .map(|date| date.timestamp_millis())
                .unwrap_or(0);
            rows.push(CommandRow {
                id: format!("workspace:{}", workspace.id),
                title: self
                    .directory
                    .workspace_title(&workspace.id, &workspace.name),
                subtitle,
                target: CommandTarget::Workspace(workspace.id.clone()),
                icon: "folder".into(),
                keywords: vec![],
                activity,
                query_only: false,
            });
        }
        for agent in self
            .directory
            .agents
            .values()
            .filter(|agent| !agent.internal && agent.archived_at.is_none())
        {
            let workspace = agent
                .workspace_id
                .as_ref()
                .and_then(|id| self.directory.workspaces.get(id));
            rows.push(CommandRow {
                id: format!("agent:{}", agent.id),
                title: agent.display_title().into(),
                subtitle: workspace
                    .map(|w| self.directory.workspace_title(&w.id, &w.name))
                    .unwrap_or_default(),
                target: CommandTarget::Agent(agent.id.clone()),
                icon: agent.provider.clone(),
                keywords: vec![],
                activity: agent
                    .updated_at
                    .as_ref()
                    .or(agent.created_at.as_ref())
                    .and_then(|date| chrono::DateTime::parse_from_rfc3339(date).ok())
                    .map(|date| date.timestamp_millis())
                    .unwrap_or(0),
                query_only: false,
            });
        }
        rows
    }
    fn close_command_center(&mut self, restore: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = &self.command_center {
            view.update(cx, |view, cx| view.close(cx));
        }
        if let Some(focus) = self.command_center_restore.take().filter(|_| restore) {
            focus.focus(window, cx);
        }
        cx.notify();
    }
    fn open_command_center(
        &mut self,
        scope: CommandScope,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.command_center.is_none() {
            let Some(connection) = self.connection.clone() else {
                return;
            };
            let shared = self.shared_settings.clone();
            let view =
                cx.new(|cx| crate::command_center::CommandCenter::new(connection, shared, cx));
            self.command_center_events = Some(cx.subscribe_in(
                &view,
                window,
                |this, _, event: &crate::command_center::Command, window, cx| {
                    let cancel = matches!(event, crate::command_center::Command::Cancel) || matches!(event,crate::command_center::Command::Pick(CommandTarget::Action(id)) if matches!(id.as_str(),"toggle-left-sidebar"|"toggle-explorer"));
                    this.close_command_center(cancel, window, cx);
                    if let crate::command_center::Command::Pick(target) = event {
                        match target {
                            CommandTarget::File(path) => this.open_file(path.clone(), window, cx),
                            CommandTarget::Workspace(id) => this.select_workspace(id.clone(), cx),
                            CommandTarget::Agent(id) => this.select_agent(id.clone(), window, cx),
                            CommandTarget::Action(id) => match id.as_str() {
                                "new-workspace" => this.open_workspace_form(
                                    crate::workspace_form::FormMode::NewWorkspace,
                                    window,
                                    cx,
                                ),
                                "history" => this.open_history(window, cx),
                                "settings" => this.open_pane(Pane::Settings, window, cx),
                                "schedules" => this.open_pane(Pane::Schedules, window, cx),
                                "toggle-left-sidebar" => {
                                    this.sidebar_open = !this.sidebar_open;
                                    cx.notify();
                                }
                                "toggle-explorer" => this.toggle_explorer(window, cx),
                                _ => {}
                            },
                        }
                    }
                },
            ));
            self.command_center = Some(view);
        }
        if !self.command_center_open(cx) {
            self.command_center_restore = window.focused(cx);
        }
        self.workspace_menu = None;
        self.scripts_menu = None;
        let cwd = self.command_center_cwd();
        let rows = self.command_center_rows();
        self.command_center
            .as_ref()
            .unwrap()
            .update(cx, |view, cx| view.open(scope, cwd, rows, window, cx));
        cx.notify();
    }
    fn on_search_files(&mut self, _: &SearchFiles, window: &mut Window, cx: &mut Context<Self>) {
        if self.pane.is_some() || self.command_center_open(cx) || self.terminal_commands.modal() {
            return;
        }
        if cfg!(target_os = "macos") || !self.terminal_view_focused(window, cx) {
            self.open_command_center(CommandScope::Files, window, cx);
        } else {
            cx.propagate();
        }
    }
    fn on_toggle_command_center(
        &mut self,
        _: &ToggleCommandCenter,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.terminal_commands.modal() {
            return;
        }
        if !cfg!(target_os = "macos") && self.terminal_view_focused(window, cx) {
            cx.propagate();
            return;
        }
        if self.command_center_open(cx) {
            self.close_command_center(true, window, cx);
        } else {
            self.open_command_center(CommandScope::All, window, cx);
        }
    }

    fn toggle_explorer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let return_focus = self
            .tools
            .as_ref()
            .is_some_and(|tools| tools.read(cx).focus_handle(cx).is_focused(window));
        if let Some(layout) = self.selected_splits() {
            layout.explorer.visible = !layout.explorer.visible;
            let closing = !layout.explorer.visible;
            let tab = layout
                .focused_pane()
                .and_then(|pane| pane.focused_tab.clone());
            if closing && return_focus {
                let focus = tab
                    .and_then(|tab| match self.pane_tab_kind(&tab) {
                        Some(PaneTabKind::Terminal) => self
                            .terminals
                            .iter()
                            .find(|(id, _)| id.as_str() == tab)
                            .map(|(_, view)| view.read(cx).focus_handle(cx)),
                        Some(PaneTabKind::Agent) => self
                            .composers
                            .get(&tab)
                            .map(|view| view.read(cx).focus_handle(cx)),
                        Some(PaneTabKind::File) => self
                            .editors
                            .iter()
                            .find(|(key, _)| key.tab_id() == tab)
                            .map(|(_, view)| view.read(cx).focus_handle(cx)),
                        Some(PaneTabKind::Browser) => self
                            .browsers
                            .get(&tab)
                            .map(|view| view.read(cx).focus_handle(cx)),
                        Some(PaneTabKind::Launcher) | None => None,
                    })
                    .unwrap_or_else(|| self.focus.clone());
                focus.focus(window, cx);
            }
            self.persist_splits();
            cx.notify();
        }
    }
    fn on_toggle_explorer(
        &mut self,
        _: &ToggleExplorer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pane.is_some() || self.command_center_open(cx) || self.terminal_commands.modal() {
            return;
        }
        if cfg!(target_os = "macos") || !self.terminal_view_focused(window, cx) {
            self.toggle_explorer(window, cx);
        } else {
            cx.propagate();
        }
    }
    fn on_toggle_explorer_terminal(
        &mut self,
        _: &ToggleExplorerTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pane.is_some() || self.command_center_open(cx) || self.terminal_commands.modal() {
            return;
        }
        self.toggle_explorer(window, cx);
    }
    fn move_explorer_drag(&mut self, position: gpui::Point<gpui::Pixels>, cx: &mut Context<Self>) {
        if let Some(drag) = &mut self.explorer_drag {
            let requested = drag.start_width + drag.start_x - position.x.to_f64();
            let next = paseo_desktop::explorer_layout::resolve_width(
                requested,
                self.explorer_container_width,
            );
            if drag.preview_width != next {
                drag.preview_width = next;
                cx.notify();
            }
        }
    }
    fn finish_explorer_drag(&mut self, cx: &mut Context<Self>) {
        if let Some(drag) = self.explorer_drag.take() {
            if let Some(layout) = self.selected_splits() {
                layout.explorer.width = drag.preview_width;
            }
            self.persist_splits();
            cx.notify();
        }
    }

    fn on_new_window(&mut self, _: &NewWindow, window: &mut Window, cx: &mut Context<Self>) {
        if self.command_center_open(cx) || self.terminal_commands.modal() {
            return;
        }
        // Windows/Linux hand Ctrl+N to a focused terminal program; only the
        // client surface opens an additional window, like the desktop app.
        if !cfg!(target_os = "macos") && self.terminal_view_focused(window, cx) {
            return;
        }
        let _ = open_paseo_window(cx, None);
    }
    fn apply_session(
        &mut self,
        kind: &str,
        mut payload: Value,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        if self
            .host_terminals
            .as_ref()
            .is_some_and(|view| view.update(cx, |view, cx| view.receive(kind, &payload, cx)))
        {
            return Ok(());
        }
        if self.receive_terminal_command(kind, &payload, cx)? {
            return Ok(());
        }
        if let Some(view) = &self.command_center {
            if view.update(cx, |view, cx| view.receive(kind, &payload, cx)) {
                return Ok(());
            }
        }
        if kind == "fetch_agents_response" {
            let _: AgentPage = serde_json::from_value(payload.clone())?;
        } else if kind == "fetch_workspaces_response" {
            let _: WorkspacePage = serde_json::from_value(payload.clone())?;
        }
        if let Some(refresh) = &mut self.directory_refresh {
            refresh.apply(kind, &mut payload, &mut self.directory);
            if refresh.complete() {
                self.directory_refresh = None;
                self.schedule_activation(cx);
            }
        }
        if kind == "agent_attention_required" {
            self.receive_agent_attention(&payload, cx);
            return Ok(());
        }
        if kind == "terminal_attention_required" {
            self.receive_terminal_attention(&payload, cx);
            return Ok(());
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
                self.provider_ids = provider_ids(&payload);
                self.provider_images =
                    crate::chrome::render_provider_icons(&self.directory, &payload, cx)?;
                return Ok(());
            }
        }
        if kind == "get_providers_snapshot_response" {
            return Ok(());
        }
        if kind == "hosts.list.response" || kind == "hosts.changed" {
            match daemon_registrations::parse_directory(&payload) {
                Ok(directory) => self.hosts.registrations = Some(directory),
                Err(error) => {
                    self.hosts.registration_error =
                        Some(format!("Invalid host directory: {error:#}"));
                }
            }
            if let Some(id) = payload["requestId"].as_str() {
                if self.hosts.observation_request.as_deref() == Some(id) {
                    self.hosts.observation_request = None;
                }
                self.hosts.registration_requests.remove(id);
            }
            if self.hosts.registration_requests.is_empty() {
                self.hosts.registration_busy = None;
            }
            return Ok(());
        }
        if kind == "hosts.add.response" {
            if let Some(id) = payload["requestId"].as_str() {
                self.hosts.registration_requests.remove(id);
            }
            if self.hosts.registration_requests.is_empty() {
                self.hosts.registration_busy = None;
            }
            match daemon_registrations::parse_summary(&payload) {
                Ok(host) => {
                    let label = host.label.unwrap_or_else(|| host.registration_id.clone());
                    self.hosts.registration_error = None;
                    self.hosts.message =
                        Some((false, format!("Registered {label} on this daemon.")));
                }
                Err(error) => {
                    self.hosts.registration_error =
                        Some(format!("Invalid host summary: {error:#}"));
                }
            }
            return Ok(());
        }
        if kind == "hosts.remove.response" {
            if let Some(id) = payload["requestId"].as_str() {
                self.hosts.registration_requests.remove(id);
            }
            if self.hosts.registration_requests.is_empty() {
                self.hosts.registration_busy = None;
            }
            self.hosts.registration_error = None;
            return Ok(());
        }
        if kind == "rpc_error"
            && payload["requestId"]
                .as_str()
                .is_some_and(|id| self.hosts.observation_request.as_deref() == Some(id))
        {
            self.hosts.observation_request = None;
            self.hosts.registration_error = Some(format!(
                "Cannot observe host registrations: {}",
                payload["error"]
            ));
            return Ok(());
        }
        if kind == "rpc_error"
            && payload["requestId"]
                .as_str()
                .is_some_and(|id| self.hosts.registration_requests.remove(id))
        {
            if self.hosts.registration_requests.is_empty() {
                self.hosts.registration_busy = None;
            }
            self.hosts.registration_error = Some(format!(
                "Daemon refused the registration: {}",
                payload["error"]
            ));
            return Ok(());
        }
        if kind == "providers_snapshot_update" {
            self.directory.replace_provider_icons(&payload)?;
            self.provider_images =
                crate::chrome::render_provider_icons(&self.directory, &payload, cx)?;
            self.provider_ids = provider_ids(&payload);
            return Ok(());
        }

        // Workspace tools answer only their own pending requests and updates.
        if kind == "fetch_workspaces_response" {
            for entry in payload["entries"].as_array().unwrap_or(&Vec::new()) {
                if let Some(id) = entry["id"].as_str() {
                    self.workspace_payloads.insert(id.to_owned(), entry.clone());
                }
            }
        }
        if kind == "workspace_update" && payload["kind"] == "upsert" {
            if let Some(id) = payload["workspace"]["id"].as_str() {
                self.workspace_payloads
                    .insert(id.to_owned(), payload["workspace"].clone());
            }
        }
        if let Some(tools) = &self.tools {
            if kind == "fetch_workspaces_response" {
                for entry in payload["entries"].as_array().unwrap_or(&Vec::new()) {
                    if let Some(id) = entry["id"].as_str() {
                        tools.update(cx, |tools, _| tools.adopt_workspace_scripts(id, entry));
                    }
                }
            }
            if kind == "workspace_update" && payload["kind"] == "upsert" {
                if let Some(id) = payload["workspace"]["id"].as_str() {
                    tools.update(cx, |tools, _| {
                        tools.adopt_workspace_scripts(id, &payload["workspace"])
                    });
                }
            }
            // Workspace tools claim their own replies by pending request id
            // and workspace-scoped updates, so every session message is
            // offered here and ownership is decided by that data — not by a
            // response-kind whitelist that silently drops reply kinds it does
            // not name yet.
            if tools.update(cx, |tools, cx| tools.receive(kind, &payload, cx)) {
                return Ok(());
            }
        }
        if matches!(
            kind,
            "file_explorer_response"
                | "fs.file.subscribe.response"
                | "fs.file.write.response"
                | "fs.file.update"
                | "rpc_error"
        ) {
            for editor in self.editors.values() {
                if editor.update(cx, |editor, cx| editor.receive(kind, &payload, cx)) {
                    return Ok(());
                }
            }
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
                self.conversations.contains_key(id) || !view.read(cx).is_disposable()
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
        let mut timeline_error_handled = false;
        for conversation in self.conversations.values() {
            let owns_error = kind == "rpc_error" && conversation.read(cx).owns_request(&payload);
            conversation.update(cx, |view, cx| view.receive(kind, &payload, cx))?;
            timeline_error_handled |= owns_error;
        }
        if timeline_error_handled {
            return Ok(());
        }
        if let Some(pane) = &self.schedules_pane
            && pane.update(cx, |view, cx| view.receive(kind, &payload, cx))
        {
            return Ok(());
        }
        if let Some(pane) = &self.plugins_pane
            && pane.update(cx, |view, cx| view.receive(kind, &payload, cx))
        {
            return Ok(());
        }
        if let Some(pane) = &self.schedule_form_pane
            && pane.update(cx, |view, cx| view.receive(kind, &payload, cx))
        {
            return Ok(());
        }
        if let Some(pane) = &self.plugin_logs_pane
            && pane.update(cx, |view, cx| view.receive(kind, &payload, cx))
        {
            return Ok(());
        }
        match kind {
            "fetch_agents_response" => {
                let page: AgentPage = serde_json::from_value(payload.clone())?;
                if page.page_info.has_more {
                    let cursor = page
                        .page_info
                        .next_cursor
                        .context("agent continuation cursor missing")?;
                    self.host
                        .as_ref()
                        .context("host disconnected")?
                        .connection
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
                    self.host
                        .as_ref()
                        .context("host is disconnected")?
                        .connection
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
        let arrival = paseo_desktop::model::terminal_inventory_arrival(
            kind,
            payload["requestId"].as_str(),
            payload["workspaceId"].as_str(),
            self.terminal_inventory_request
                .as_ref()
                .map(|(request, workspace)| (request.as_str(), workspace.as_str())),
            self.selected_workspace.as_deref(),
        );
        if arrival.answered {
            self.terminal_inventory_request = None;
        }
        let terminal_inventory = arrival.loaded;
        let directory_ready =
            kind == "fetch_workspaces_response" && payload["pageInfo"]["hasMore"] == false;
        if matches!(
            kind,
            "fetch_agents_response"
                | "agent_update"
                | "agent_status"
                | "project.list.response"
                | "project.update"
                | "fetch_workspaces_response"
                | "workspace_update"
                | "list_terminals_response"
                | "terminals_changed"
        ) {
            self.command_center_snapshot_dirty = true;
        }
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
        if let Some(agent_id) = self.selected_agent.clone() {
            self.sync_conversation_agent(&agent_id, cx);
        }
        if terminal_inventory {
            // The snapshot describes the workspace that subscribed for it; a
            // pending terminal activation is admitted against its own.
            self.terminal_inventory_workspace = self.selected_workspace.clone();
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
            self.adopt_workspace_terminals(cx);
            // A pending terminal activation was waiting for exactly this
            // snapshot; adopting tabs does not consume it.
            self.schedule_activation(cx);
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
        self.workspace_menu = None;
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
                                this.clear_conversations(cx);
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
            |this, _, event: &crate::workspace_form::OpenForm, window, cx| {
                this.open_workspace_form(event.0.clone(), window, cx);
            },
        ));
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
        self.lifecycle_subscriptions.push(cx.subscribe_in(
            &panel,
            window,
            |this, _, _: &crate::agent_history::RecoverWorkspace, window, cx| {
                this.history = None;
                this.open_workspace_form(
                    crate::workspace_form::FormMode::Recover {
                        workspace_id: String::new(),
                    },
                    window,
                    cx,
                );
            },
        ));
        self.history = Some(panel);
        self.workspace_menu = None;
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
    fn open_schedule_form(
        &mut self,
        editing: Option<crate::schedules_view::ScheduleSummary>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_pane(cx);
        self.pane = Some(Pane::ScheduleForm);
        let connection = self.connection.clone();
        let shared = self.shared_settings.clone();
        let providers = self.provider_ids.clone();
        let mut agents = self
            .directory
            .agents
            .values()
            .filter(|agent| agent.archived_at.is_none() && !agent.internal)
            .map(|agent| (agent.id.clone(), agent.display_title().to_owned()))
            .collect::<Vec<_>>();
        agents.sort_by(|a, b| a.1.cmp(&b.1));
        let view = cx.new(|cx| {
            crate::schedule_form_view::ScheduleFormPane::new(
                connection, shared, editing, providers, agents, window, cx,
            )
        });
        self.schedule_form_pane_events = Some(cx.subscribe_in(
            &view,
            window,
            |this, _, event: &crate::schedule_form_view::ScheduleFormEvent, window, cx| {
                match event {
                    crate::schedule_form_view::ScheduleFormEvent::Close => this.close_pane(cx),
                    crate::schedule_form_view::ScheduleFormEvent::Saved => {
                        // A fresh schedules pane reloads the list.
                        this.open_pane(Pane::Schedules, window, cx);
                    }
                }
            },
        ));
        self.schedule_form_pane = Some(view);
        cx.notify();
    }

    fn select_workspace(&mut self, id: String, cx: &mut Context<Self>) {
        self.command_center_snapshot_dirty = true;
        if self.selected_workspace.as_ref() == Some(&id) {
            return;
        }
        self.close_pane(cx);
        // The terminal inventory belongs to the workspace that requested it.
        self.terminal_inventory_workspace = None;
        self.terminal_inventory_request = None;
        for terminal in self.terminals.values() {
            if let Err(error) = terminal.update(cx, |view, _| view.prepare_detach()) {
                self.error = Some(error.to_string());
                cx.notify();
                return;
            }
        }
        self.error = None;
        self.clear_conversations(cx);
        self.hide_terminal(cx);
        self.terminals.clear();
        self.split_drag = None;
        self.explorer_drag = None;
        self.pane_bounds.clear();
        self.tab_drag = None;
        self.selected_workspace = Some(id.clone());
        self.scripts_menu = None;
        self.workspace_menu = None;
        if let Some(tools) = &self.tools {
            let workspace = self.directory.workspaces.get(&id);
            let cwd = workspace
                .and_then(|workspace| {
                    workspace
                        .workspace_directory
                        .clone()
                        .or_else(|| Some(workspace.project_root_path.clone()))
                })
                .context("Workspace is no longer available");
            match cwd {
                Ok(cwd) => {
                    let raw = self
                        .workspace_payloads
                        .get(&id)
                        .cloned()
                        .unwrap_or_else(|| json!({}));
                    let scripts = crate::workspace_tools::parse_scripts(&raw);
                    tools.update(cx, |tools, cx| {
                        tools.open_workspace(id.clone(), cwd, scripts, cx)
                    });
                }
                Err(error) => self.error = Some(error.to_string()),
            }
        }
        self.tab_drag = None;
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
                    Ok((
                        connection.subscribe(
                            "terminals",
                            "subscribe_terminals_request",
                            json!({"cwd":cwd,"workspaceId":workspace.id}),
                        )?,
                        workspace.id.clone(),
                    ))
                });
            match result {
                Ok((request_id, workspace_id)) => {
                    self.terminal_inventory_request = Some((request_id, workspace_id));
                }
                Err(error) => self.error = Some(error.to_string()),
            }
        }
        if let Err(error) = self.remember_workspace() {
            self.error = Some(error.to_string());
        }
        cx.notify();
    }

    /// Open a host file in an editor tab; the buffer survives workspace switches.
    fn open_file(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tools) = &self.tools else {
            return;
        };
        let cwd = tools.read(cx).cwd().to_owned();
        if cwd.is_empty() || self.connection.is_none() {
            return;
        }
        // The focused pane owns the editor; sibling terminal and Agent panes
        // keep their content, focus history and subscriptions.
        self.close_pane(cx);
        self.selected_agent = None;
        self.selected_terminal = None;
        self.terminal = None;
        let key = EditorKey::new(&cwd, &path);
        if !self.editors.contains_key(&key) {
            match self.connection.clone().context("host is disconnected") {
                Ok(connection) => {
                    let editor = cx.new(|cx| FileEditor::new(cwd, path, connection, cx));
                    let subscription = {
                        let key = key.clone();
                        cx.subscribe_in(
                            &editor,
                            window,
                            move |this, _, _: &CloseRequested, window, cx| {
                                // The editor resolved its close; drop the tab with it.
                                this.forget_file(&key, window, cx);
                            },
                        )
                    };
                    self.editor_subscriptions.insert(key.clone(), subscription);
                    self.editors.insert(key.clone(), editor);
                }
                Err(error) => {
                    self.error = Some(error.to_string());
                    return;
                }
            }
        }
        let tab = key.tab_id();
        let adopted = self
            .selected_splits()
            .is_some_and(|layout| layout.attach_tab(&tab).is_some());
        if !adopted {
            self.error = Some("This pane cannot accept another file tab.".into());
            cx.notify();
            return;
        }
        self.persist_splits();
        self.sync_active_tab(window, cx);
        cx.notify();
    }

    fn close_file(&mut self, key: &EditorKey, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(editor) = self.editors.get(key) {
            let editor = editor.clone();
            if !editor.update(cx, |editor, cx| editor.begin_close(cx)) {
                // The editor owns its close confirmation. Reveal a modified
                // background buffer so its existing confirmation is visible.
                let owner = self.selected_splits().and_then(|layout| {
                    layout
                        .panes()
                        .into_iter()
                        .find(|pane| pane.tabs.contains(&key.tab_id()))
                });
                if let Some(pane) = owner {
                    self.select_pane_tab(pane.id, key.tab_id(), window, cx);
                }
                return;
            }
        }
        self.forget_file(key, window, cx);
    }

    /// Drop a tab: release its host subscription, its event binding, and its
    /// selection, so a closed buffer cannot outlive the tab.
    fn forget_file(&mut self, key: &EditorKey, window: &mut Window, cx: &mut Context<Self>) {
        let restore_focus = self
            .selected_splits()
            .and_then(|layout| layout.focused_pane())
            .is_some_and(|pane| pane.focused_tab.as_deref() == Some(key.tab_id().as_str()));
        if let Some(editor) = self.editors.remove(key) {
            editor.update(cx, |editor, _| editor.closed());
        }
        self.editor_subscriptions.remove(key);
        let tab = key.tab_id();
        for layout in self.splits.values_mut() {
            layout.prune_tabs(|id| id != tab);
        }
        self.persist_splits();
        if restore_focus {
            self.sync_active_tab(window, cx);
        }
        self.apply_terminal_visibility(cx);
        cx.notify();
    }

    fn close_workspace_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(menu) = self.workspace_menu.take() {
            if let Some(focus) = menu.previous_focus {
                focus.focus(window, cx);
            }
            cx.notify();
        }
    }

    fn open_workspace_menu(
        &mut self,
        workspace_id: String,
        context: bool,
        position: gpui::Point<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_workspace_menu(window, cx);
        let Some(workspace) = self.directory.workspaces.get(&workspace_id) else {
            return;
        };
        let locale = paseo_desktop::locales::active_locale(self.shared_settings.get().language);
        let text =
            |key| paseo_desktop::locales::translate(locale, paseo_desktop::locales::Tx::key(key));
        let mut items = vec![crate::action_menu::Item::new(
            "copy-path",
            text("workspace.header.actions.copyPath"),
            "copy",
        )];
        if context {
            items.push(crate::action_menu::Item::new(
                "archive",
                text("sidebar.workspace.actions.archiveWorkspace"),
                "archive",
            ));
        }
        let name = workspace.name.clone();
        let previous_focus = window.focused(cx);
        self.scripts_menu = None;
        let view = cx
            .new(|cx| crate::action_menu::ActionMenu::new(items, self.shared_settings.clone(), cx));
        let subscription = cx.subscribe_in(
            &view,
            window,
            move |this, _, event: &crate::action_menu::Chosen, window, cx| {
                this.close_workspace_menu(window, cx);
                match event.0.as_deref() {
                    Some("copy-path") => {
                        if let Some(workspace) = this.directory.workspaces.get(&workspace_id) {
                            let path = workspace
                                .workspace_directory
                                .as_ref()
                                .unwrap_or(&workspace.project_root_path);
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(path.clone()));
                        }
                    }
                    Some("archive") => this.open_workspace_form(
                        crate::workspace_form::FormMode::Archive {
                            id: workspace_id.clone(),
                            name: name.clone(),
                        },
                        window,
                        cx,
                    ),
                    _ => {}
                }
            },
        );
        view.read(cx).focus_handle(cx).focus(window, cx);
        self.workspace_menu = Some(WorkspaceMenu {
            view,
            _subscription: subscription,
            position,
            previous_focus,
            header: !context,
        });
        cx.notify();
    }

    fn toggle_scripts_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.scripts_menu.take().is_some() {
            cx.notify();
            return;
        }
        let Some(tools) = &self.tools else {
            return;
        };
        let (rows, host, local_endpoint, pending, error) = tools.update(cx, |tools, _| {
            (
                tools.scripts().into_iter().cloned().collect::<Vec<_>>(),
                tools.host().to_owned(),
                tools.local_endpoint(),
                tools.script_pending().map(str::to_owned),
                tools.script_error().map(str::to_owned),
            )
        });
        let menu = cx.new(|cx| {
            ScriptsMenu::new(
                rows,
                host,
                local_endpoint,
                pending,
                error,
                self.shared_settings.clone(),
                cx,
            )
        });
        let subscription = cx.subscribe_in(
            &menu,
            window,
            |this, _, event: &ScriptsPicked, window, cx| {
                this.scripts_menu = None;
                if let Some(action) = &event.0 {
                    let ScriptAction(name, kind) = action;
                    match kind {
                        ScriptActionKind::ViewTerminal => {
                            this.select_terminal(name.clone(), window, cx);
                        }
                        _ => {
                            if let Some(tools) = &this.tools {
                                tools.update(cx, |tools, cx| {
                                    tools.script_action(name.clone(), *kind, cx)
                                });
                            }
                        }
                    }
                }
                cx.notify();
            },
        );
        menu.read(cx).focus_handle(cx).focus(window, cx);
        self.scripts_menu = Some(menu);
        self._scripts_menu_subscription = Some(subscription);
        cx.notify();
    }

    fn sync_conversation_agent(&mut self, agent_id: &str, cx: &mut Context<Self>) {
        let Some(agent) = self.directory.agents.get(agent_id) else {
            return;
        };
        let pending = agent.pending_permissions.clone();
        let running = agent.is_working();
        if let Some(conversation) = self.conversations.get(agent_id) {
            conversation.update(cx, |view, cx| {
                view.set_permissions(agent_id, pending, cx);
                view.set_running(running, cx);
            });
        }
    }

    /// Terminals created outside this window (CLI, another client) still need a
    /// tab. Adopt every workspace terminal the layout does not own into the
    /// pane that already hosts tabs, so no PTY becomes unreachable when the
    /// global strip is gone.
    fn adopt_workspace_terminals(&mut self, cx: &mut Context<Self>) {
        let Some(workspace) = self.selected_workspace.clone() else {
            return;
        };
        let mut terminals: Vec<String> = self
            .directory
            .terminals
            .values()
            .filter(|terminal| terminal.workspace_id.as_deref() == Some(workspace.as_str()))
            .map(|terminal| terminal.id.clone())
            .collect();
        terminals.sort();
        let layout = self
            .splits
            .entry(workspace)
            .or_insert_with(|| SplitLayout::single(MAIN_PANE_ID));
        let mut changed = false;
        for terminal in terminals {
            changed |= layout.adopt_tab(&terminal);
        }
        if changed {
            self.persist_splits();
            cx.notify();
        }
    }

    fn clear_conversations(&mut self, cx: &mut Context<Self>) {
        if !self.conversations.is_empty() {
            for id in self.conversations.keys().cloned().collect::<Vec<_>>() {
                if let Some(composer) = self.composers.get(&id) {
                    composer.update(cx, |view, _| view.release_layout());
                }
            }
            self.conversations.clear();
            if let Some(connection) = &self.connection {
                connection.unsubscribe("timeline");
            }
        }
        self.selected_agent = None;
        self.timeline_agents.clear();
        self.composers
            .retain(|_, view| !view.read(cx).is_disposable());
    }

    /// Builds the conversation for an agent tab without touching focus. Panes
    /// call this when they render or restore an agent tab.
    fn open_agent_conversation(&mut self, id: &str, cx: &mut Context<Self>) -> bool {
        if self.conversations.contains_key(id) {
            return true;
        }
        let Some(agent) = self.directory.agents.get(id).cloned() else {
            return false;
        };
        let Some(connection) = self.connection.clone() else {
            return false;
        };
        let whole_turns = self
            .info
            .as_ref()
            .and_then(|info| info.features.get("wholeTurnTimeline"))
            == Some(&Value::Bool(true));
        let composer = self
            .composers
            .entry(id.to_owned())
            .or_insert_with(|| {
                let agent_id = id.to_owned();
                let shared = self.shared_settings.clone();
                cx.new(|cx| {
                    crate::composer_view::Composer::new(
                        agent_id,
                        connection.clone(),
                        shared,
                        agent.is_working(),
                        cx,
                    )
                })
            })
            .clone();
        let agent_id = id.to_owned();
        let shared = self.shared_settings.clone();
        let conversation = cx.new(|cx| {
            crate::conversation::Conversation::new(
                agent_id,
                connection,
                shared,
                whole_turns,
                composer,
                cx,
            )
        });
        // Local image sources resolve against the agent's workspace root; a
        // conversation created without it can never load them.
        let root = agent.workspace_id.as_ref().and_then(|id| {
            self.directory.workspaces.get(id).map(|workspace| {
                workspace
                    .workspace_directory
                    .clone()
                    .unwrap_or_else(|| workspace.project_root_path.clone())
            })
        });
        conversation.update(cx, |view, cx| view.set_workspace_root(root, cx));
        self.conversations.insert(id.to_owned(), conversation);
        true
    }

    /// Keeps one timeline subscription for every open agent tab. The daemon
    /// replaces the subscription, so only a changed id set sends a request.
    fn sync_timeline_subscription(&mut self) {
        let agents: BTreeSet<String> = self.conversations.keys().cloned().collect();
        if agents == self.timeline_agents {
            return;
        }
        let Some(connection) = &self.connection else {
            return;
        };
        let result = if agents.is_empty() {
            connection.unsubscribe("timeline");
            Ok(())
        } else {
            let ids: Vec<String> = agents.iter().cloned().collect();
            connection
                .subscribe(
                    "timeline",
                    "agent.timeline.set_subscription.request",
                    json!({"agentIds": ids}),
                )
                .map(|_: String| ())
        };
        // A refused request keeps the previous set, so a later sync retries
        // instead of remembering a subscription the daemon never accepted.
        match result {
            Ok(()) => self.timeline_agents = agents,
            Err(error) => self.error = Some(error.to_string()),
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
                if !self.open_agent_conversation(&id, cx) {
                    self.error = Some("This agent is no longer available.".into());
                    cx.notify();
                    return;
                }
                self.sync_timeline_subscription();
                // Agent tabs own a pane slot just like terminals; selecting
                // an agent already open in another pane focuses that pane.
                if let Some(layout) = self.selected_splits() {
                    layout.attach_tab(&id);
                }
                self.persist_splits();
                if let Some(composer) = self.composers.get(&id) {
                    composer.read(cx).focus_handle(cx).focus(window, cx);
                }
                // Keep the previous pane's views mounted; only the active
                // session pointer moves to the agent tab.

                self.terminal = None;
                self.selected_terminal = None;
                self.selected_agent = Some(id.clone());
                self.command_center_snapshot_dirty = true;
                self.directory
                    .remember_focus(&workspace, FocusedSession::Agent(id.clone()));
                self.error = None;
            }
        }
        cx.notify();
    }

    fn open_terminal(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<TerminalView>> {
        let connection = self.connection.clone()?;
        // Terminal tabs other panes currently render are live: eviction would
        // rebuild them on the next render and lose their viewport and
        // selection. The tab being replaced is not protected.
        let rendered = self.rendered_terminal_ids();
        let mut protected: std::collections::BTreeSet<String> = self
            .terminals
            .iter()
            .filter(|(_, view)| !view.read(cx).can_release())
            .map(|(id, _)| id.clone())
            .collect();
        protected.extend(rendered);
        let shared = self.shared_settings.clone();
        let terminal_id = id.to_owned();
        match self.terminals.open(id, &protected, || {
            cx.new(|cx| TerminalView::new(terminal_id.clone(), connection, shared, window, cx))
        }) {
            Ok(view) => Some(view),
            Err(_) => {
                self.error = Some("Wait for the other terminals to finish opening or sending input, then select this tab again.".into());
                None
            }
        }
    }

    fn select_terminal(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.directory.terminal_workspace(&id) != self.selected_workspace.as_deref()
            || self.selected_workspace.is_none()
        {
            self.error = Some("This terminal is no longer available in this workspace.".into());
            cx.notify();
            return;
        }

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
        self.close_pane(cx);
        // Tab ownership follows pane focus: a terminal already owned by
        // another pane refocuses that pane instead of moving.
        if let Some(layout) = self.selected_splits() {
            layout.attach_tab(&id);
        }
        self.persist_splits();
        let Some(view) = self.open_terminal(&id, window, cx) else {
            cx.notify();
            return;
        };
        if let Some(workspace) = &self.selected_workspace {
            self.directory
                .remember_focus(workspace, FocusedSession::Terminal(id.clone()));
        }

        self.selected_agent = self
            .directory
            .terminal_agent(&id)
            .map(|agent| agent.id.clone());
        self.selected_terminal = Some(id);
        self.error = None;
        view.update(cx, |view, cx| view.activate(window, cx));
        self.terminal = Some(view);
        cx.notify();
    }

    fn selected_splits(&mut self) -> Option<&mut SplitLayout> {
        let workspace = self.selected_workspace.clone()?;
        Some(
            self.splits
                .entry(workspace)
                .or_insert_with(|| SplitLayout::single(MAIN_PANE_ID)),
        )
    }

    /// The terminal tabs a pane currently renders; unrendered retained views
    /// release their glyph shapes.
    fn rendered_terminal_ids(&self) -> BTreeSet<String> {
        self.selected_workspace
            .as_ref()
            .and_then(|workspace| self.splits.get(workspace))
            .map(|layout| {
                layout
                    .panes()
                    .into_iter()
                    .filter_map(|pane| pane.focused_tab)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Every pane's visible tab stays mounted and visible; only tabs that no
    /// pane shows release their glyphs. Runs after tab/pane changes so a
    /// background pane keeps painting without being focused.
    fn apply_terminal_visibility(&mut self, cx: &mut Context<Self>) {
        let rendered = self.rendered_terminal_ids();
        let window_visible = self.window_visible
            && self.pane.is_none()
            && self.workspace_form.is_none()
            && self.history.is_none();
        for (id, view) in self.terminals.iter() {
            let expected = window_visible && rendered.contains(id);
            view.update(cx, |view, cx| view.visible(expected, cx));
        }
    }

    fn pane_tab_kind(&self, tab_id: &str) -> Option<PaneTabKind> {
        if tab_id.starts_with(terminal_commands::PREFIX) {
            Some(PaneTabKind::Launcher)
        } else if self.directory.terminals.contains_key(tab_id) {
            Some(PaneTabKind::Terminal)
        } else if self.directory.agents.contains_key(tab_id) {
            Some(PaneTabKind::Agent)
        } else if self.editors.keys().any(|key| key.tab_id() == tab_id) {
            Some(PaneTabKind::File)
        } else if self.browsers.contains_key(tab_id) {
            Some(PaneTabKind::Browser)
        } else {
            None
        }
    }

    fn pane_tab_title(&self, tab_id: &str, cx: &gpui::App) -> Option<String> {
        if tab_id.starts_with(terminal_commands::PREFIX) {
            return Some(self.launch_text("workspace.tabs.actions.newTab"));
        }
        if let Some(terminal) = self.directory.terminals.get(tab_id) {
            return Some(self.directory.terminal_title(terminal));
        }
        if let Some((_, editor)) = self.editors.iter().find(|(key, _)| key.tab_id() == tab_id) {
            return Some(editor.read(cx).title());
        }
        if let Some(browser) = self.browsers.get(tab_id) {
            return Some(browser.read(cx).title());
        }
        self.directory
            .agents
            .get(tab_id)
            .map(|agent| agent.display_title().to_owned())
    }

    /// The browser tabs the current workspace shows in some pane. A surface
    /// that no pane shows keeps its native page alive but hidden.
    fn rendered_browser_ids(&self) -> BTreeSet<String> {
        self.selected_workspace
            .as_ref()
            .and_then(|workspace| self.splits.get(workspace))
            .map(|layout| {
                layout
                    .panes()
                    .into_iter()
                    .filter_map(|pane| pane.focused_tab)
                    .filter(|tab| self.browsers.contains_key(tab))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Drops surfaces no layout owns any more. Dropping the surface drops its
    /// native page, so a closed pane releases its webviews instead of keeping
    /// them for the rest of the session.
    fn prune_browsers(&mut self) {
        if self.browsers.is_empty() {
            return;
        }
        let live: BTreeSet<String> = self
            .splits
            .values()
            .flat_map(|layout| layout.panes())
            .flat_map(|pane| pane.tabs)
            .collect();
        self.browsers.retain(|tab_id, _| live.contains(tab_id));
    }

    /// A page is a native child window, so it may only present while its tab is
    /// the focused tab of a pane in the visible workspace. Every other surface
    /// stays retained but hidden rather than being torn down and reloaded.
    fn apply_browser_visibility(&mut self, cx: &mut Context<Self>) {
        self.prune_browsers();
        if self.browsers.is_empty() {
            return;
        }
        let rendered = self.rendered_browser_ids();
        let window_visible = self.window_visible
            && self.pane.is_none()
            && self.workspace_form.is_none()
            && self.history.is_none();
        // Floating menus sit outside the retained pane visibility state; a native
        // page would otherwise paint over them.
        let obscured = self.scripts_menu.is_some()
            || self.workspace_menu.is_some()
            || self.terminal_commands.modal()
            || self.command_center_open(cx);
        for (tab_id, browser) in self.browsers.iter() {
            let presented = window_visible && rendered.contains(tab_id);
            browser.update(cx, |browser, cx| {
                browser.set_presented(presented, cx);
                browser.set_obscured(obscured, cx);
            });
        }
    }

    /// Opens a browser tab in the focused pane. The pane owns the tab, so a
    /// split keeps its page when another pane takes focus.
    fn open_browser_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_pane(cx);
        // The id is written into the persisted layout, so it must never be handed
        // out twice: a restored tab that reused a number would show a page it
        // never owned, in another workspace.
        let tab_id = format!("{BROWSER_TAB_PREFIX}{}", uuid::Uuid::new_v4());
        let shared = self.shared_settings.clone();
        let surface =
            cx.new(|cx| crate::browser_view::BrowserSurface::new(tab_id.clone(), shared, cx));
        if !self
            .selected_splits()
            .is_some_and(|layout| layout.attach_tab(&tab_id).is_some())
        {
            self.error = Some("This pane cannot accept another browser tab.".into());
            cx.notify();
            return;
        }
        let subscription = cx.subscribe_in(&surface, window, {
            let tab_id = tab_id.clone();
            move |this, _, event, window, cx| match event {
                crate::browser_view::BrowserSurfaceEvent::LabelChanged => {
                    // The page title is the pane tab label, so the strip redraws.
                    cx.notify()
                }
                crate::browser_view::BrowserSurfaceEvent::PageFocused => {
                    this.focus_pane_for_focused_page(&tab_id, cx);
                    // The page has the real keyboard now; release the location bar
                    // so its stale GPUI focus stops outranking it.
                    if let Some(browser) = this.browsers.get(&tab_id) {
                        browser.update(cx, |browser, cx| browser.release_location_bar(window, cx));
                    }
                }
                crate::browser_view::BrowserSurfaceEvent::PageActivity => {
                    // Input in the page the user is already watching: presence
                    // stays fresh without touching focus or the session pointer.
                    this.note_user_activity();
                }
            }
        });
        self.browser_subscriptions
            .insert(tab_id.clone(), subscription);
        self.browsers.insert(tab_id, surface);
        self.persist_splits();
        self.sync_active_tab(window, cx);
        cx.notify();
    }

    /// The native page of `tab_id` reported platform keyboard focus.
    ///
    /// A page is a child window: neither GPUI nor the daemon sees its clicks, so
    /// without this the pane keeps reporting whatever agent or terminal it showed
    /// before. The pane follows the tab and the session pointer clears, but GPUI
    /// focus is left alone - the page already owns the keyboard, and focusing the
    /// surface here would take it back from the element the user just clicked.
    fn focus_pane_for_focused_page(&mut self, tab_id: &str, cx: &mut Context<Self>) {
        if !self.browsers.contains_key(tab_id) {
            return;
        }
        self.note_user_activity();
        let already_focused = self
            .selected_splits()
            .is_some_and(|layout| layout.tab_is_focused(tab_id));
        if !already_focused {
            let focused = self
                .selected_splits()
                .is_some_and(|layout| layout.focus_tab(tab_id).is_some());
            if !focused {
                return;
            }
            self.persist_splits();
        }
        self.clear_session_pointer(cx);
        cx.notify();
    }

    /// Drops a browser tab: its native page goes away with the surface, and the
    /// tab leaves every layout so a restore cannot point at it.
    fn forget_browser(&mut self, tab_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        // Deleting the focused tab's native page blurs the window: the handle
        // the keyboard sat on is gone. Only a close that takes the shown tab
        // away hands focus to what replaces it; closing a background page must
        // not pull the keyboard out of the pane the user is in.
        let was_shown = self
            .selected_splits()
            .is_some_and(|layout| layout.tab_is_focused(tab_id));
        self.browsers.remove(tab_id);
        self.browser_subscriptions.remove(tab_id);
        for layout in self.splits.values_mut() {
            layout.prune_tabs(|id| id != tab_id);
        }
        self.persist_splits();
        if was_shown {
            self.sync_active_tab(window, cx);
        }
        cx.notify();
    }

    /// Writes the current workspace layout after a structural change. Saves
    /// are skipped when nothing changed, so focus churn does not rewrite disk.
    fn persist_splits(&mut self) {
        let Some(host_id) = self.info.as_ref().map(|info| info.server_id.clone()) else {
            return;
        };
        let Some(workspace_id) = self.selected_workspace.clone() else {
            return;
        };
        let Some(layout) = self.splits.get(&workspace_id).cloned() else {
            return;
        };
        if let Err(error) = self.split_store.save(&host_id, &workspace_id, layout) {
            self.error = Some(format!("Cannot save split layout: {error:#}"));
        }
    }

    /// Points the active session at the focused pane's tab. Panes that are not
    /// focused keep their views mounted; only the focus and active-session
    /// pointer move, so no PTY is torn down when switching panes.
    fn sync_active_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let active = self
            .selected_workspace
            .clone()
            .and_then(|workspace| self.splits.get(&workspace).cloned())
            .and_then(|layout| layout.focused_pane().cloned())
            .and_then(|pane| pane.focused_tab.map(|tab| (pane.id, tab)));
        let Some((_pane_id, tab)) = active else {
            self.terminal = None;
            self.selected_terminal = None;
            self.selected_agent = None;
            self.focus.focus(window, cx);
            return;
        };
        match self.pane_tab_kind(&tab) {
            Some(PaneTabKind::Terminal) => {
                if self.selected_terminal.as_deref() == Some(tab.as_str()) {
                    return;
                }
                let Some(view) = self.open_terminal(&tab, window, cx) else {
                    return;
                };
                if let Some(workspace) = &self.selected_workspace {
                    self.command_center_snapshot_dirty = true;
                    self.directory
                        .remember_focus(workspace, FocusedSession::Terminal(tab.clone()));
                }
                self.selected_agent = self
                    .directory
                    .terminal_agent(&tab)
                    .map(|agent| agent.id.clone());
                self.selected_terminal = Some(tab);
                view.update(cx, |view, cx| view.activate(window, cx));
                self.terminal = Some(view);
            }
            Some(PaneTabKind::Agent) => {
                if !self.open_agent_conversation(&tab, cx) {
                    return;
                }
                self.sync_timeline_subscription();
                if let Some(workspace) = &self.selected_workspace {
                    self.command_center_snapshot_dirty = true;
                    self.directory
                        .remember_focus(workspace, FocusedSession::Agent(tab.clone()));
                }
                if let Some(composer) = self.composers.get(&tab) {
                    composer.read(cx).focus_handle(cx).focus(window, cx);
                }
                self.terminal = None;
                self.selected_terminal = None;
                self.selected_agent = Some(tab);
            }
            Some(PaneTabKind::File) => {
                if let Some(editor) = self
                    .editors
                    .iter()
                    .find(|(key, _)| key.tab_id() == tab)
                    .map(|(_, view)| view)
                {
                    editor.read(cx).focus_handle(cx).focus(window, cx);
                }
                self.terminal = None;
                self.selected_terminal = None;
                self.selected_agent = None;
            }
            Some(PaneTabKind::Browser) => {
                // The page is a native child window; GPUI focus sits on the
                // surface so a composer or editor behind the pane cannot keep
                // claiming the keyboard or report itself as the shown session.
                // A page or location bar that already owns the keyboard keeps
                // it: this runs from the pane's mouse-down, after the clicked
                // element took focus.
                if let Some(browser) = self.browsers.get(&tab) {
                    let browser = browser.read(cx);
                    if browser.focus_owner(window, cx).surface_may_take_focus() {
                        browser.focus_handle(cx).focus(window, cx);
                    }
                }
                self.terminal = None;
                self.selected_terminal = None;
                self.selected_agent = None;
            }
            Some(PaneTabKind::Launcher) | None => {
                self.terminal = None;
                self.selected_terminal = None;
                self.selected_agent = None;
                self.focus.focus(window, cx);
            }
        }
    }

    /// Selects a tab in the pane that owns it. Ownership does not move: the
    /// pane was clicked, so it becomes the focused pane and opens its tab.
    fn select_pane_tab(
        &mut self,
        pane_id: String,
        tab_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(layout) = self.selected_splits() {
            layout.focus_pane(&pane_id);
            layout.set_focused_tab(&pane_id, &tab_id);
        }
        self.persist_splits();
        match self.pane_tab_kind(&tab_id) {
            Some(PaneTabKind::Terminal) => self.select_terminal(tab_id, window, cx),
            Some(PaneTabKind::Agent) => self.select_agent(tab_id, window, cx),
            Some(PaneTabKind::File | PaneTabKind::Launcher) => {
                self.close_pane(cx);
                self.sync_active_tab(window, cx);
                cx.notify();
            }
            Some(PaneTabKind::Browser) => {
                self.close_pane(cx);
                self.sync_active_tab(window, cx);
                cx.notify();
            }
            None => cx.notify(),
        }
    }

    fn begin_tab_drag(
        &mut self,
        from_pane: String,
        tab_id: String,
        position: gpui::Point<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.tab_drag = Some(TabDrag {
            tab_id: tab_id.clone(),
            from_pane: from_pane.clone(),
            start_x: position.x.to_f64(),
            start_y: position.y.to_f64(),
            moved: false,
            hovered_pane: None,
            pane_hover_fresh: false,
        });
        self.select_pane_tab(from_pane, tab_id, window, cx);
    }

    fn track_tab_drag(&mut self, position: gpui::Point<gpui::Pixels>, cx: &mut Context<Self>) {
        let Some(drag) = &mut self.tab_drag else {
            return;
        };
        if !drag.moved {
            let dx = position.x.to_f64() - drag.start_x;
            let dy = position.y.to_f64() - drag.start_y;
            if dx * dx + dy * dy >= TAB_DRAG_THRESHOLD_SQUARED {
                drag.moved = true;
                cx.notify();
            }
            return;
        }
        // Pane hover fired before this root move for the same position; a
        // move the panes did not see means the pointer is outside every pane.
        if drag.pane_hover_fresh {
            drag.pane_hover_fresh = false;
        } else if drag.hovered_pane.take().is_some() {
            cx.notify();
        }
    }

    fn hover_tab_drag(&mut self, pane_id: &str, cx: &mut Context<Self>) {
        let Some(drag) = &mut self.tab_drag else {
            return;
        };
        if !drag.moved {
            return;
        }
        drag.pane_hover_fresh = true;
        if drag.hovered_pane.as_deref() == Some(pane_id) {
            return;
        }
        drag.hovered_pane = Some(pane_id.to_owned());
        cx.notify();
    }

    fn finish_tab_drag(
        &mut self,
        position: gpui::Point<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(drag) = self.tab_drag.take() else {
            return false;
        };
        if !drag.moved {
            return false;
        }
        let Some(target) = self
            .pane_bounds
            .iter()
            .find(|(_, bounds)| bounds.contains(&position))
            .map(|(id, _)| id.clone())
        else {
            return false;
        };
        if target == drag.from_pane {
            return false;
        }
        let moved = self
            .selected_splits()
            .map(|layout| layout.move_tab_to_pane(&drag.tab_id, &target))
            .unwrap_or(false);
        if moved {
            self.persist_splits();
            self.sync_active_tab(window, cx);
        }
        moved
    }

    fn split_focused_pane(
        &mut self,
        position: SplitPosition,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(layout) = self.selected_splits() else {
            return;
        };
        let target = layout
            .focused_pane()
            .map(|pane| pane.id.clone())
            .unwrap_or_else(|| MAIN_PANE_ID.to_owned());
        let mut ids = SplitIds::for_layout(&layout.root);
        match layout.split_empty(&target, position, &mut ids) {
            Some(_) => {
                // The new pane holds no tab yet, like a new-tab placeholder.
                self.persist_splits();
                self.terminal = None;
                self.selected_terminal = None;
                self.selected_agent = None;
                self.error = None;
                // Keyboard focus cannot stay in the old pane's terminal, or the
                // next render would give pane focus back to it.
                self.focus.focus(window, cx);
                cx.notify();
            }
            None => {
                self.error = Some("Close a pane before creating more splits.".into());
                cx.notify();
            }
        }
    }

    fn focus_adjacent_pane(
        &mut self,
        direction: FocusDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(layout) = self.selected_splits() else {
            return;
        };
        let Some(focused) = layout
            .focused_pane()
            .map(|pane| pane.id.clone())
            .or_else(|| layout.panes().first().map(|pane| pane.id.clone()))
        else {
            return;
        };
        let Some(adjacent) = layout.find_adjacent_pane(&focused, direction) else {
            return;
        };
        if layout.focus_pane(&adjacent) {
            self.persist_splits();
            self.sync_active_tab(window, cx);
            cx.notify();
        }
    }

    /// Moves the focused pane's tab to the adjacent pane, like
    /// `workspace.pane.move-tab.*` in the original app.
    fn move_focused_tab(
        &mut self,
        direction: FocusDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(layout) = self.selected_splits() else {
            return;
        };
        let Some(focused) = layout.focused_pane().map(|pane| pane.id.clone()) else {
            return;
        };
        let Some(tab) = layout
            .focused_pane()
            .and_then(|pane| pane.focused_tab.clone())
        else {
            return;
        };
        let Some(adjacent) = layout.find_adjacent_pane(&focused, direction) else {
            return;
        };
        if layout.move_tab_to_pane(&tab, &adjacent) {
            self.persist_splits();
            self.sync_active_tab(window, cx);
            cx.notify();
        }
    }

    fn close_focused_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(layout) = self.selected_splits() else {
            return;
        };
        let Some(focused) = layout.focused_pane().map(|pane| pane.id.clone()) else {
            return;
        };
        // Closing a pane must not strand its PTYs: the tabs move to the pane
        // that takes focus, keeping them reachable in its strip.
        let tabs = layout
            .pane(&focused)
            .map(|pane| pane.tabs.clone())
            .unwrap_or_default();
        if !layout.close_pane(&focused) {
            return;
        }
        for tab in tabs {
            layout.attach_tab(&tab);
        }
        self.persist_splits();
        self.sync_active_tab(window, cx);
        cx.notify();
    }

    fn focus_split_pane(&mut self, pane_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let changed = self
            .selected_splits()
            .map(|layout| layout.focus_pane(&pane_id))
            .unwrap_or(false);
        if changed {
            self.persist_splits();
            self.sync_active_tab(window, cx);
            cx.notify();
        }
    }

    fn begin_split_drag(
        &mut self,
        group_id: String,
        index: usize,
        horizontal: bool,
        position: gpui::Point<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) {
        let (width, height) = self.split_area_px.unwrap_or_default();
        if width <= 0. || height <= 0. {
            return;
        }
        let Some(layout) = self.selected_splits() else {
            return;
        };
        let Some(group_px) = layout.group_size_px(&group_id, (0., 0., width, height)) else {
            return;
        };
        let Some(start_sizes) = layout.group_sizes(&group_id) else {
            return;
        };
        let start_axis = if horizontal {
            position.x.to_f64()
        } else {
            position.y.to_f64()
        };
        self.split_drag = Some(SplitDrag {
            group_id,
            index,
            horizontal,
            start_axis,
            start_sizes,
            group_px,
        });
        cx.notify();
    }

    fn move_split_drag(&mut self, position: gpui::Point<gpui::Pixels>, cx: &mut Context<Self>) {
        let Some(drag) = self.split_drag.clone() else {
            return;
        };
        let axis = if drag.horizontal {
            position.x.to_f64()
        } else {
            position.y.to_f64()
        };
        let delta = (axis - drag.start_axis) / drag.group_px.max(1.0);
        let mut sizes = drag.start_sizes.clone();
        sizes[drag.index] += delta;
        sizes[drag.index + 1] -= delta;
        let resized = self
            .selected_splits()
            .map(|layout| layout.resize_group(&drag.group_id, sizes))
            .unwrap_or(false);
        if resized {
            cx.notify();
        }
    }

    /// Reopens retained views for panes the workspace restored, mirroring the
    /// original app's tab restoration; subscriptions belong to the view, never
    /// to the pane that shows it. Agent conversations are built here too, so a
    /// restored agent tab renders inside its pane.
    fn restore_split_panes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.selected_workspace.clone() else {
            return;
        };
        if self.connection.is_none() {
            return;
        }
        let layout = self
            .splits
            .entry(workspace)
            .or_insert_with(|| SplitLayout::single(MAIN_PANE_ID))
            .clone();
        let mut opened_agent = false;
        for pane in layout.panes() {
            let Some(tab) = pane.focused_tab.clone() else {
                continue;
            };
            match self.pane_tab_kind(&tab) {
                Some(PaneTabKind::Terminal) => {
                    let view = if let Some((_, view)) =
                        self.terminals.iter().find(|(id, _)| id.as_str() == tab)
                    {
                        Some(view.clone())
                    } else {
                        self.open_terminal(&tab, window, cx)
                    };
                    if Some(&pane.id) == layout.focused_pane_id.as_ref()
                        && self.selected_terminal.as_deref() != Some(tab.as_str())
                        && let Some(view) = view
                    {
                        view.update(cx, |view, cx| view.activate(window, cx));
                        self.terminal = Some(view);
                        self.selected_terminal = Some(tab.clone());
                    }
                }
                Some(PaneTabKind::Agent) => {
                    opened_agent |= self.open_agent_conversation(&tab, cx);
                    if Some(&pane.id) == layout.focused_pane_id.as_ref()
                        && self.selected_terminal.is_none()
                    {
                        self.selected_agent = Some(tab.clone());
                    }
                }
                Some(PaneTabKind::File) => {}
                Some(PaneTabKind::Browser | PaneTabKind::Launcher) => {}
                None => {}
            }
        }
        // A browser tab with no surface cannot be restored: the native page died
        // with the previous session. Drop the tab so a restored pane does not
        // hold a slot with nothing to click.
        let live_browsers: BTreeSet<String> = self.browsers.keys().cloned().collect();
        if let Some(layout) = self.selected_splits() {
            layout.prune_tabs(|tab| {
                !tab.starts_with(BROWSER_TAB_PREFIX) || live_browsers.contains(tab)
            });
        }
        if opened_agent {
            self.sync_timeline_subscription();
        }
    }

    /// Clicking a pane's terminal, composer, or file editor gives it keyboard
    /// focus; pane focus follows so new tabs, traversal and drag targets follow
    /// the pane the user is working in. State only changes when focus actually
    /// moved, otherwise this runs on every render and would notify forever.
    fn sync_focused_pane_from_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // A native page holds the platform keyboard, so a GPUI handle that still
        // reports focus elsewhere is stale: moving onto the page never took it
        // away. The page therefore outranks every GPUI handle here.
        let focused_page = self
            .browsers
            .iter()
            .find(|(_, view)| view.read(cx).focus_owner(window, cx) == BrowserFocusOwner::Page)
            .map(|(tab_id, _)| tab_id.clone());
        let focused_terminal = self
            .terminals
            .iter()
            .find(|(_, view)| view.read(cx).focus_handle(cx).is_focused(window))
            .map(|(id, view)| (id.clone(), view.clone()));
        // A file editor stops mouse-down propagation on its content, so its
        // focus handle is the only place pane focus can learn that the editor,
        // not the agent behind the pane, owns the keyboard.
        let focused_editor = self
            .editors
            .iter()
            .find(|(_, view)| view.read(cx).focus_handle(cx).is_focused(window))
            .map(|(key, _)| key.tab_id());
        // A browser keeps GPUI focus on its surface: the page itself is a native
        // child window and never reports GPUI focus, and its location bar is a
        // child input that focuses itself on the click the pane is handling.
        let focused_browser = self
            .browsers
            .iter()
            .find(|(_, view)| view.read(cx).focus_owner(window, cx).is_focused())
            .map(|(tab_id, _)| tab_id.clone());
        let focused_tab = focused_page
            .clone()
            .or_else(|| focused_terminal.as_ref().map(|(id, _)| id.clone()))
            .or_else(|| {
                self.composers
                    .iter()
                    .find(|(_, view)| view.read(cx).focus_handle(cx).is_focused(window))
                    .map(|(id, _)| id.clone())
            })
            .or_else(|| focused_editor.clone())
            .or_else(|| focused_browser.clone());
        let Some(tab) = focused_tab else {
            return;
        };
        let already_focused = self
            .selected_splits()
            .map(|layout| layout.tab_is_focused(&tab))
            .unwrap_or(false);
        if !already_focused {
            let Some(layout) = self.selected_splits() else {
                return;
            };
            if layout.focus_tab(&tab).is_none() {
                return;
            }
            self.persist_splits();
            cx.notify();
        }
        if focused_page.is_some() {
            // The focused pane shows a page, so the agent or terminal the pane
            // was pointed at before is no longer on screen: presence and
            // attention must not keep reporting it as the displayed session.
            self.clear_session_pointer(cx);
        } else if let Some((tab, view)) = focused_terminal {
            if self.selected_terminal.as_ref() != Some(&tab) {
                if let Some(workspace) = &self.selected_workspace {
                    self.command_center_snapshot_dirty = true;
                    self.directory
                        .remember_focus(workspace, FocusedSession::Terminal(tab.clone()));
                }
                self.selected_agent = self
                    .directory
                    .terminal_agent(&tab)
                    .map(|agent| agent.id.clone());
                self.selected_terminal = Some(tab);
                self.terminal = Some(view);
                cx.notify();
            }
        } else if focused_editor.is_some() || focused_browser.is_some() {
            self.clear_session_pointer(cx);
        } else if self.selected_agent.as_deref() != Some(tab.as_str()) {
            self.terminal = None;
            self.selected_terminal = None;
            self.selected_agent = Some(tab);
            cx.notify();
        }
    }

    /// The surface on screen is not the selected agent or terminal any more, so
    /// the heartbeat and attention gates stop reporting one.
    fn clear_session_pointer(&mut self, cx: &mut Context<Self>) {
        if self.selected_agent.is_some()
            || self.selected_terminal.is_some()
            || self.terminal.is_some()
        {
            self.terminal = None;
            self.selected_terminal = None;
            self.selected_agent = None;

            cx.notify();
        }
    }

    fn on_split_pane_right(
        &mut self,
        _: &SplitPaneRight,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pane.is_some() || self.command_center_open(cx) || self.terminal_commands.modal() {
            return;
        }
        self.split_focused_pane(SplitPosition::Right, window, cx);
    }

    fn on_split_pane_down(
        &mut self,
        _: &SplitPaneDown,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pane.is_some() || self.command_center_open(cx) || self.terminal_commands.modal() {
            return;
        }
        self.split_focused_pane(SplitPosition::Bottom, window, cx);
    }

    fn on_focus_pane_left(
        &mut self,
        _: &FocusPaneLeft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pane.is_some() || self.command_center_open(cx) || self.terminal_commands.modal() {
            return;
        }
        self.focus_adjacent_pane(FocusDirection::Left, window, cx);
    }

    fn on_focus_pane_right(
        &mut self,
        _: &FocusPaneRight,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pane.is_some() || self.command_center_open(cx) || self.terminal_commands.modal() {
            return;
        }
        self.focus_adjacent_pane(FocusDirection::Right, window, cx);
    }

    fn on_focus_pane_up(&mut self, _: &FocusPaneUp, window: &mut Window, cx: &mut Context<Self>) {
        if self.pane.is_some() || self.command_center_open(cx) || self.terminal_commands.modal() {
            return;
        }
        self.focus_adjacent_pane(FocusDirection::Up, window, cx);
    }

    fn on_focus_pane_down(
        &mut self,
        _: &FocusPaneDown,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pane.is_some() || self.command_center_open(cx) || self.terminal_commands.modal() {
            return;
        }
        self.focus_adjacent_pane(FocusDirection::Down, window, cx);
    }

    fn on_move_tab_left(&mut self, _: &MoveTabLeft, window: &mut Window, cx: &mut Context<Self>) {
        if self.pane.is_some() || self.command_center_open(cx) || self.terminal_commands.modal() {
            return;
        }
        self.move_focused_tab(FocusDirection::Left, window, cx);
    }

    fn on_move_tab_right(&mut self, _: &MoveTabRight, window: &mut Window, cx: &mut Context<Self>) {
        if self.pane.is_some() || self.command_center_open(cx) || self.terminal_commands.modal() {
            return;
        }
        self.move_focused_tab(FocusDirection::Right, window, cx);
    }

    fn on_move_tab_up(&mut self, _: &MoveTabUp, window: &mut Window, cx: &mut Context<Self>) {
        if self.pane.is_some() || self.command_center_open(cx) || self.terminal_commands.modal() {
            return;
        }
        self.move_focused_tab(FocusDirection::Up, window, cx);
    }

    fn on_move_tab_down(&mut self, _: &MoveTabDown, window: &mut Window, cx: &mut Context<Self>) {
        if self.pane.is_some() || self.command_center_open(cx) || self.terminal_commands.modal() {
            return;
        }
        self.move_focused_tab(FocusDirection::Down, window, cx);
    }
    fn on_close_focused_pane(
        &mut self,
        _: &CloseFocusedPane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pane.is_some() || self.command_center_open(cx) || self.terminal_commands.modal() {
            return;
        }
        self.close_focused_pane(window, cx);
    }

    fn on_root_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.note_user_activity();
        self.move_split_drag(event.position, cx);
        self.track_tab_drag(event.position, cx);
    }

    fn on_root_pointer_down(&mut self, _: &MouseDownEvent, _: &mut Window, _: &mut Context<Self>) {
        self.note_user_activity();
    }

    fn on_root_mouse_up(
        &mut self,
        event: &MouseUpEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let resized = self.split_drag.take().is_some();
        if resized {
            self.persist_splits();
        }
        let moved = self.finish_tab_drag(event.position, window, cx);
        if resized || moved {
            cx.notify();
        }
    }
}

impl Render for Paseo {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use crate::chrome::{Tooltip, icon};
        use crate::theme::radius;
        // Resolved appearance: authored palette for the selected theme plus the
        // scaled UI font ramp. Other views still read the generated default
        // dark tokens until their styling accessors are migrated.
        self.apply_terminal_visibility(cx);
        self.apply_browser_visibility(cx);
        let settings = self.shared_settings.get();
        let palette = crate::settings_view::active_palette(&self.shared_settings, window);
        let font_base = paseo_desktop::appearance::scale_ui_font(
            crate::theme::font_size::BASE,
            settings.ui_base_font_size,
        );
        let font_sm = paseo_desktop::appearance::scale_ui_font(
            crate::theme::font_size::SM,
            settings.ui_base_font_size,
        );
        // The authored hover and label overlays flatten their group opacity onto
        // the sidebar surface, so theme switches stay exact without alpha layers.
        let hover_overlay =
            crate::chrome::control_color(palette.foreground, palette.surface_sidebar, 0.08);
        let header_hover = paseo_desktop::appearance::interaction_background(
            if palette.is_dark {
                crate::action_menu_presentation::DARK_HEADER_HIGHLIGHT_COLOR
            } else {
                crate::action_menu_presentation::LIGHT_HEADER_HIGHLIGHT_COLOR
            },
            palette.surface0,
            if palette.is_dark {
                crate::action_menu_presentation::DARK_HEADER_HIGHLIGHT_OPACITY
            } else {
                crate::action_menu_presentation::LIGHT_HEADER_HIGHLIGHT_OPACITY
            },
        );
        let label_overlay =
            crate::chrome::control_color(palette.foreground, palette.surface_sidebar, 0.76);
        let dot_overlay = crate::chrome::control_color(
            palette.foreground_extra_muted,
            palette.surface_sidebar,
            0.3,
        );
        if self.tools.is_none()
            && let Some(connection) = self.connection.clone()
        {
            self.bind_tools(connection, window, cx);
        }
        if self.hosts_panel.is_none() {
            let panel = cx.new(|cx| HostsPanel::new(cx));
            self._hosts_panel_subscription = Some(cx.subscribe(&panel, |this, _, event, cx| {
                this.handle_hosts_event(event.clone(), cx)
            }));
            self.hosts_panel = Some(panel);
        }
        if let Some(panel) = &self.hosts_panel {
            let snapshot = self
                .hosts
                .snapshot(matches!(self.hosts.phase, Some(HostPhase::Online)));
            panel.update(cx, |panel, cx| panel.set_snapshot(snapshot, cx));
        }
        if self.command_center_open(cx) {
            let cwd = self.command_center_cwd();
            let rows = self.command_center_rows();
            if let Some(view) = &self.command_center {
                view.update(cx, |view, cx| {
                    view.update_context(cwd, rows, cx);
                    view.set_providers(&self.provider_images, cx);
                });
                if !view.read(cx).focus_handle(cx).is_focused(window) {
                    view.read(cx).focus_handle(cx).focus(window, cx);
                }
            }
        }
        let explorer_state = self
            .selected_workspace
            .as_ref()
            .and_then(|id| self.splits.get(id))
            .map(|layout| layout.explorer.clone())
            .unwrap_or_default();
        let explorer_presented = explorer_state.visible
            && self.selected_workspace.is_some()
            && self.pane.is_none()
            && self.tools.is_some();
        let requested_width = self
            .explorer_drag
            .as_ref()
            .map_or(explorer_state.width, |drag| drag.preview_width);
        let explorer_geometry = paseo_desktop::explorer_layout::geometry(
            window.viewport_size().width.to_f64(),
            self.sidebar_open,
            f64::from(crate::window_geometry::DEFAULT_SIDEBAR_WIDTH),
            explorer_presented,
            requested_width,
        );
        self.explorer_container_width = explorer_geometry.container_width;
        let sidebar_presented = explorer_geometry.navigation_visible;
        if let Some(tools) = &self.tools {
            tools.update(cx, |tools, cx| {
                tools.set_window_corner(explorer_presented, cx)
            });
        }
        let mut projects = self
            .directory
            .projects
            .values()
            .cloned()
            .collect::<Vec<_>>();
        projects.sort_by(|a, b| a.project_display_name.cmp(&b.project_display_name));
        let nav_entries = [
            ("plus", "New workspace", SidebarAction::NewWorkspace),
            ("history", "History", SidebarAction::History),
            ("search", "Search", SidebarAction::Search),
            (
                "calendar-clock",
                "Schedules",
                SidebarAction::Pane(Pane::Schedules),
            ),
            ("eye", "Browser", SidebarAction::Browser),
        ];
        let sidebar = div()
            .id("workspace-sidebar")
            .w(px(crate::window_geometry::DEFAULT_SIDEBAR_WIDTH))
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface_sidebar))
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
                    .border_color(rgb(palette.border))
                    .children(nav_entries.map(|(glyph, label, action)| {
                        let row = div()
                            .h(px(28.))
                            .px_2()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_color(rgb(palette.foreground_muted))
                            .child(icon(glyph, 14.))
                            .child(label);
                        match action {
                            SidebarAction::Pane(pane) => row
                                .id(SharedString::from(format!("nav-{label}")))
                                .rounded(px(radius::MD))
                                .cursor_pointer()
                                .hover(|row| row.bg(rgb(palette.surface_sidebar_hover)))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.open_pane(pane, window, cx);
                                    }),
                                )
                                .into_any_element(),
                            SidebarAction::Browser => row
                                .id(SharedString::from(format!("nav-{label}")))
                                .rounded(px(radius::MD))
                                .cursor_pointer()
                                .hover(|row| row.bg(rgb(palette.surface_sidebar_hover)))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.open_browser_tab(window, cx);
                                    }),
                                )
                                .into_any_element(),
                            SidebarAction::NewWorkspace => row
                                .cursor_pointer()
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, window, cx| {
                                        this.open_workspace_form(
                                            crate::workspace_form::FormMode::NewWorkspace,
                                            window,
                                            cx,
                                        )
                                    }),
                                )
                                .into_any_element(),
                            SidebarAction::History => row
                                .cursor_pointer()
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, window, cx| {
                                        this.open_history(window, cx)
                                    }),
                                )
                                .into_any_element(),
                            SidebarAction::Search => row.cursor_pointer().on_mouse_down(MouseButton::Left,cx.listener(|this,_,window,cx|{cx.stop_propagation();this.open_command_center(CommandScope::All,window,cx);})).into_any_element(),
                        }
                    })),
            )
            .child({
                let project_list = div()
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
                            .text_size(px(font_sm))
                            .text_color(rgb(palette.foreground_muted))
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
                                    .text_color(rgb(palette.foreground_muted))
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
                                            .text_color(rgb(palette.foreground))
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
                                let menu_id = id.clone();
                                let selected = self.selected_workspace.as_ref() == Some(&id);
                                div()
                                    .id(gpui::SharedString::from(id.clone()))
                                    .rounded(px(radius::LG))
                                    .py_2()
                                    .pl_2()
                                    .pr_3()
                                    .mb(px(2.))
                                    .cursor_pointer()
                                    .when(selected, |row| row.bg(rgb(palette.surface_sidebar_selected)))
                                    .hover(move |row| {
                                        row.bg(rgb(if selected {
                                            palette.surface_sidebar_selected
                                        } else {
                                            palette.surface_sidebar_hover
                                        }))
                                    })
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, _, _, cx| {
                                            this.select_workspace(id.clone(), cx)
                                        }),
                                    )
                                    .on_mouse_down(MouseButton::Right, cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                                        cx.stop_propagation();
                                        this.open_workspace_menu(menu_id.clone(), true, event.position, window, cx);
                                    }))
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
                                                            .bg(rgb(dot_overlay)),
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .min_w_0()
                                                    .flex_1()
                                                    .line_height(px(20.))
                                                    .text_color(rgb(label_overlay))
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
                                                            self.selected_workspace.as_ref() == Some(&workspace.id) && self.directory.agent_target(&id).is_some_and(|target| match target { FocusedSession::Agent(id) => self.selected_agent.as_ref()==Some(&id) && self.conversations.contains_key(&id), FocusedSession::Terminal(id) => self.selected_terminal.as_ref()==Some(&id) }),
                                                            |button| {
                                                                button.bg(rgb(hover_overlay))
                                                            },
                                                        )
                                                        .hover(|button| {
                                                            button.bg(rgb(hover_overlay))
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
                    }));
                project_list
            })
            .when(self.hosts_mode, |pane| {
                pane.child(
                    div()
                        .id("hosts-list")
                        .flex_1()
                        .overflow_y_scroll()
                        .px_2()
                        .pt_2()
                        .children(self.hosts_panel.clone()),
                )
            })
            .child(
                div()
                    .h(px(41.))
                    .flex_shrink_0()
                    .border_t_1()
                    .border_color(rgb(palette.border))
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
                            .when(glyph == "server", |row| row.cursor_pointer().on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| { cx.stop_propagation(); this.hosts_mode = !this.hosts_mode; cx.notify(); })))
                    }))
                    .child(div().flex_1())
                    .children(["circle-help", "settings"].map(|glyph| {
                        div()
                            .size(px(28.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icon(glyph, 16.))
                            .when(glyph == "settings", |button| {
                                button.cursor_pointer().on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, window, cx| this.open_pane(Pane::Settings, window, cx)),
                                )
                            })
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
        let pane = match self.pane {
            Some(Pane::HostTerminals) => self
                .host_terminals
                .clone()
                .map(|view| view.into_any_element()),
            Some(Pane::Settings) => self
                .settings_pane
                .clone()
                .map(|pane| pane.into_any_element()),
            Some(Pane::Schedules) => self
                .schedules_pane
                .clone()
                .map(|pane| pane.into_any_element()),
            Some(Pane::ScheduleForm) => self
                .schedule_form_pane
                .clone()
                .map(|pane| pane.into_any_element()),
            Some(Pane::PluginLogs) => self
                .plugin_logs_pane
                .clone()
                .map(|pane| pane.into_any_element()),
            Some(Pane::Plugins) => self
                .plugins_pane
                .clone()
                .map(|pane| pane.into_any_element()),
            None => None,
        };
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
                    .pr(px(
                        if cfg!(target_os = "macos") || explorer_presented || window.is_fullscreen()
                        {
                            4.
                        } else {
                            0.
                        },
                    ))
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_b_1()
                    .border_color(rgb(palette.border))
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
                                .hover(|button| button.bg(rgb(header_hover)))
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
                            .flex_shrink(1.)
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .font_weight(gpui::FontWeight::LIGHT)
                                    .child(title.clone()),
                            )
                            .when(
                                !title.trim().eq_ignore_ascii_case(subtitle.trim()),
                                |group| {
                                    group.child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_color(rgb(palette.foreground_muted))
                                            .child(subtitle),
                                    )
                                },
                            ),
                    )
                    .when(self.selected_workspace.is_some(), |header| {
                        header.child({
                            let entity = cx.entity();
                            div()
                                .id("workspace-header-menu")
                                .relative()
                                .size(px(26.))
                                .flex_shrink_0()
                                .rounded(px(radius::MD))
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .when(
                                    self.workspace_menu.as_ref().is_some_and(|menu| menu.header),
                                    |button| button.bg(rgb(header_hover)),
                                )
                                .hover(|button| button.bg(rgb(header_hover)))
                                .child(icon("ellipsis", 16.))
                                .child(
                                    canvas(
                                        move |bounds, _, cx| {
                                            entity.update(cx, |this, _| {
                                                this.workspace_menu_bounds = bounds
                                            })
                                        },
                                        |_, (), _, _| {},
                                    )
                                    .absolute()
                                    .size_full(),
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, window, cx| {
                                        cx.stop_propagation();
                                        if let Some(id) = this.selected_workspace.clone() {
                                            let origin = point(
                                                this.workspace_menu_bounds.left(),
                                                this.workspace_menu_bounds.bottom() + px(4.),
                                            );
                                            this.open_workspace_menu(id, false, origin, window, cx);
                                        }
                                    }),
                                )
                        })
                    })
                    .child(crate::window_chrome::drag_region())
                    .when(
                        self.tools
                            .as_ref()
                            .is_some_and(|tools| !tools.read(cx).scripts().is_empty()),
                        |header| {
                            header.child({
                                let entity = cx.entity();
                                let scripts_open = self.scripts_menu.is_some();
                                div()
                                    .id("scripts-button")
                                    .relative()
                                    .size(px(26.))
                                    .rounded(px(radius::MD))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .when(self.selected_workspace.is_some(), |button| {
                                        button.cursor_pointer()
                                    })
                                    .when(scripts_open, |button| button.bg(rgb(palette.surface2)))
                                    .hover(|button| button.bg(gpui::rgba(0xffffff14)))
                                    .child(icon("play", 16.))
                                    .child(
                                        canvas(
                                            move |bounds, _, cx| {
                                                entity.update(cx, |this, _| {
                                                    this.scripts_bounds = bounds;
                                                })
                                            },
                                            |_, (), _, _| {},
                                        )
                                        .absolute()
                                        .size_full(),
                                    )
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _, window, cx| {
                                            cx.stop_propagation();
                                            if this.selected_workspace.is_some() {
                                                this.toggle_scripts_menu(window, cx);
                                            }
                                        }),
                                    )
                            })
                        },
                    )
                    .child(
                        div()
                            .id("toggle-explorer")
                            .size(px(26.))
                            .flex_shrink_0()
                            .when(
                                !cfg!(target_os = "macos")
                                    && !window.is_fullscreen()
                                    && !explorer_presented,
                                |button| button.mr(px(-4.)),
                            )
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(radius::MD))
                            .cursor_pointer()
                            .hover(|button| button.bg(rgb(header_hover)))
                            .child(icon("panel-right", 16.))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.toggle_explorer(window, cx);
                                }),
                            ),
                    )
                    .when(
                        !cfg!(target_os = "macos")
                            && !window.is_fullscreen()
                            && !explorer_presented,
                        |header| header.child(crate::window_chrome::controls(window)),
                    ),
            )
            .when_some(
                self.error.clone().or_else(|| {
                    self.terminal_commands
                        .state
                        .error_for(self.selected_workspace.as_deref())
                }),
                |view, error| {
                    view.child(
                        div()
                            .px_4()
                            .py_2()
                            .text_color(rgb(palette.status_danger))
                            .child(error),
                    )
                },
            )
            .child(if let Some(pane) = pane {
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .child(pane)
                    .into_any_element()
            } else {
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .child(self.render_split_area(window, cx))
                    .into_any_element()
            });
        // A focused terminal claims its wheel events before they reach the root,
        // so presence observes the window's scroll events instead of bubbling.
        let presence_entity = cx.entity();
        let presence = canvas(
            |_, _, _| {},
            move |_, _, window, _cx| {
                let entity = presence_entity.clone();
                let drag_entity = entity.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                    if phase.capture() {
                        let _ = drag_entity.update(cx, |this, cx| {
                            if this.explorer_drag.is_some() {
                                this.move_explorer_drag(event.position, cx);
                                cx.stop_propagation();
                            }
                        });
                    }
                });
                let drag_entity = entity.clone();
                window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                    if phase.capture() && event.button == MouseButton::Left {
                        let _ = drag_entity.update(cx, |this, cx| {
                            if this.explorer_drag.is_some() {
                                this.finish_explorer_drag(cx);
                                cx.stop_propagation();
                            }
                        });
                    }
                });
                window.on_mouse_event(move |_: &gpui::ScrollEvent, phase, _, cx| {
                    if phase.capture() {
                        let _ = entity.update(cx, |this, _| this.note_user_activity());
                    }
                });
            },
        )
        .absolute()
        .size_full();
        let profile_overlay = self
            .host_terminals
            .clone()
            .and_then(|view| view.update(cx, |view, cx| view.overlay(window, cx)));
        div()
            .size_full()
            .relative()
            .flex()
            .font_family(crate::fonts::ui_family_for(&settings.ui_font_family))
            .bg(rgb(palette.surface0))
            .text_color(rgb(palette.foreground))
            .text_size(px(font_base))
            .line_height(px(20.))
            .key_context("Workspace")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::on_split_pane_right))
            .on_action(cx.listener(Self::on_split_pane_down))
            .on_action(cx.listener(Self::on_focus_pane_left))
            .on_action(cx.listener(Self::on_focus_pane_right))
            .on_action(cx.listener(Self::on_focus_pane_up))
            .on_action(cx.listener(Self::on_focus_pane_down))
            .on_action(cx.listener(Self::on_move_tab_left))
            .on_action(cx.listener(Self::on_move_tab_right))
            .on_action(cx.listener(Self::on_move_tab_up))
            .on_action(cx.listener(Self::on_move_tab_down))
            .on_action(cx.listener(Self::on_close_focused_pane))
            .on_action(cx.listener(Self::on_new_tab))
            .on_action(cx.listener(Self::on_new_terminal))
            .on_action(cx.listener(Self::on_new_browser))
            .on_key_down(cx.listener(Self::on_launcher_key))
            .on_action(cx.listener(Self::on_new_window))
            .on_action(cx.listener(Self::on_toggle_explorer))
            .on_action(cx.listener(Self::on_toggle_explorer_terminal))
            .on_action(cx.listener(Self::on_search_files))
            .on_action(cx.listener(Self::on_toggle_command_center))
            .on_mouse_move(cx.listener(Self::on_root_mouse_move))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_root_pointer_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_root_mouse_up))
            .child(presence)
            .when(sidebar_presented, |root| root.child(sidebar))
            .child(content)
            .when(explorer_presented, |root| {
                let frame = div()
                    .id("explorer-frame")
                    .relative()
                    .w(px(explorer_geometry.width as f32))
                    .h_full()
                    .flex_shrink_0()
                    .child(self.tools.clone().unwrap())
                    .when(!explorer_geometry.overlay, |frame| {
                        frame.child(
                            div()
                                .id("explorer-resize")
                                .absolute()
                                .left(px(-4.))
                                .top_0()
                                .bottom_0()
                                .w(px(8.))
                                .cursor(CursorStyle::ResizeLeftRight)
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                        cx.stop_propagation();
                                        this.explorer_drag = Some(ExplorerDrag {
                                            start_x: event.position.x.to_f64(),
                                            start_width: explorer_geometry.width,
                                            preview_width: explorer_geometry.width,
                                        });
                                    }),
                                ),
                        )
                    });
                root.child(frame.when(explorer_geometry.overlay, |frame| {
                    frame.absolute().top_0().bottom_0().right_0()
                }))
            })
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
            .when_some(self.scripts_menu.clone(), |root, menu| {
                root.child(
                    gpui::deferred(
                        gpui::anchored()
                            .position(point(
                                self.scripts_bounds.right(),
                                self.scripts_bounds.bottom() + px(4.),
                            ))
                            .anchor(gpui::Anchor::TopRight)
                            .snap_to_window_with_margin(px(8.))
                            .child(menu),
                    )
                    .with_priority(3),
                )
            })
            .when_some(
                self.workspace_menu
                    .as_ref()
                    .map(|menu| (menu.view.clone(), menu.position)),
                |root, (menu, position)| {
                    root.child(
                        div()
                            .absolute()
                            .inset_0()
                            .occlude()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.close_workspace_menu(window, cx);
                                }),
                            )
                            .on_mouse_down(
                                MouseButton::Right,
                                cx.listener(|this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.close_workspace_menu(window, cx);
                                }),
                            )
                            .child(
                                gpui::deferred(
                                    gpui::anchored()
                                        .position(position)
                                        .anchor(gpui::Anchor::TopLeft)
                                        .snap_to_window_with_margin(px(8.))
                                        .child(menu),
                                )
                                .with_priority(4),
                            ),
                    )
                },
            )
            .when_some(profile_overlay, |root, overlay| root.child(overlay))
            .when_some(self.render_new_tab_overlay(cx), |root, overlay| {
                root.child(overlay)
            })
            .when_some(
                self.command_center
                    .clone()
                    .filter(|view| view.read(cx).is_open()),
                |root, view| root.child(gpui::deferred(view).with_priority(5)),
            )
            .children(crate::window_chrome::resize_handles(window))
    }
}

impl Paseo {
    /// Renders the workspace split tree. Measurement feeds separator drag
    /// math; the same canvas pattern the terminal view uses for its metrics.
    fn render_split_area(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        self.sync_focused_pane_from_focus(window, cx);
        self.restore_split_panes(window, cx);
        self.apply_terminal_visibility(cx);
        self.apply_browser_visibility(cx);
        let layout = self
            .selected_splits()
            .map(|layout| layout.clone())
            .unwrap_or_else(|| SplitLayout::single(MAIN_PANE_ID));
        let measure = cx.entity();
        let active_panes: BTreeSet<_> = layout.panes().into_iter().map(|pane| pane.id).collect();
        self.tab_rail.retain(&layout);
        self.terminal_commands
            .retain(self.selected_workspace.as_deref(), &layout);
        self.pane_bounds.retain(|id, _| active_panes.contains(id));
        div()
            .id("split-area")
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .child(
                canvas(
                    move |bounds, _, cx| {
                        let _ = measure.update(cx, |this, _| {
                            this.split_area_px =
                                Some((bounds.size.width.to_f64(), bounds.size.height.to_f64()));
                        });
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            )
            .child(self.render_split_node(&layout.root, window, cx))
            .into_any_element()
    }

    fn render_split_node(
        &mut self,
        node: &SplitNode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        match node {
            SplitNode::Pane { pane } => self.render_split_pane(pane, window, cx).into_any_element(),
            SplitNode::Group { group } => {
                let group = group.clone();
                let palette = crate::settings_view::active_palette(&self.shared_settings, window);
                let horizontal = group.direction == SplitDirection::Horizontal;
                let container = div().flex().flex_1().min_h_0().min_w_0().overflow_hidden();
                let container = if horizontal {
                    container.flex_row()
                } else {
                    container.flex_col()
                };
                let mut children = Vec::new();
                for (index, child) in group.children.iter().enumerate() {
                    if index > 0 {
                        children.push(
                            self.render_separator(&group.id, index - 1, horizontal, &palette, cx)
                                .into_any_element(),
                        );
                    }
                    let size = group.sizes.get(index).copied().unwrap_or(0.0).max(0.0001) as f32;
                    let child_node = child.clone();
                    children.push(
                        div()
                            .flex()
                            .flex_col()
                            .flex_grow(size)
                            .flex_basis(px(0.))
                            .min_h_0()
                            .min_w_0()
                            .child(self.render_split_node(&child_node, window, cx))
                            .into_any_element(),
                    );
                }
                container.children(children).into_any_element()
            }
        }
    }

    fn render_separator(
        &mut self,
        group_id: &str,
        index: usize,
        horizontal: bool,
        palette: &paseo_desktop::appearance::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let group_id = group_id.to_owned();
        let cursor = if horizontal {
            CursorStyle::ResizeLeftRight
        } else {
            CursorStyle::ResizeUpDown
        };
        div()
            .id(SharedString::from(format!(
                "split-separator-{group_id}-{index}"
            )))
            .cursor(cursor)
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .when(horizontal, |separator| separator.w(px(SEPARATOR_HIT)))
            .when(!horizontal, |separator| separator.h(px(SEPARATOR_HIT)))
            .child(
                div()
                    .flex_shrink_0()
                    .bg(rgb(palette.border))
                    .when(horizontal, |line| line.w(px(1.)).h_full())
                    .when(!horizontal, |line| line.h(px(1.)).w_full()),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.begin_split_drag(group_id.clone(), index, horizontal, event.position, cx);
                    cx.stop_propagation();
                }),
            )
    }

    fn render_split_pane(
        &mut self,
        pane: &SplitPane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let palette = crate::settings_view::active_palette(&self.shared_settings, window);
        let pane = pane.clone();
        let pane_id = pane.id.clone();
        let focused_tab = pane.focused_tab.clone();
        let drop_target = self.tab_drag.as_ref().is_some_and(|drag| {
            drag.moved && drag.hovered_pane.as_deref() == Some(pane_id.as_str())
        });
        let mut content: Option<gpui::AnyElement> = match focused_tab
            .as_deref()
            .and_then(|tab| self.pane_tab_kind(tab).map(|kind| (tab, kind)))
        {
            Some((tab, PaneTabKind::Terminal)) => self
                .terminals
                .iter()
                .find(|(id, _)| id.as_str() == tab)
                .map(|(_, view)| view.clone().into_any_element()),
            Some((tab, PaneTabKind::Agent)) => self
                .conversations
                .get(tab)
                .map(|conversation| conversation.clone().into_any_element()),
            Some((tab, PaneTabKind::File)) => self
                .editors
                .iter()
                .find(|(key, _)| key.tab_id() == tab)
                .map(|(_, editor)| editor.clone().into_any_element()),
            Some((tab, PaneTabKind::Browser)) => self
                .browsers
                .get(tab)
                .map(|browser| browser.clone().into_any_element()),
            _ => None,
        };

        if (focused_tab.is_none()
            && self.selected_workspace.is_some()
            && self.terminal_inventory_workspace == self.selected_workspace)
            || focused_tab
                .as_deref()
                .is_some_and(|id| id.starts_with(terminal_commands::PREFIX))
        {
            content = Some(self.render_launcher(&pane_id, focused_tab.clone(), window, cx));
        }
        let strip = self.render_workspace_tab_row(&pane, drop_target, window, cx);

        let measure = cx.entity();
        let measured_pane_id = pane_id.clone();
        let focus_pane_id = pane_id.clone();
        let hover_pane_id = pane_id.clone();
        let pane_element = div()
            .id(SharedString::from(format!("split-pane-{pane_id}")))
            .relative()
            .size_full()
            .min_h_0()
            .min_w_0()
            .overflow_hidden()
            .flex()
            .flex_col()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    this.focus_split_pane(focus_pane_id.clone(), window, cx);
                }),
            )
            .on_mouse_move(cx.listener(move |this, _, _, cx| {
                this.hover_tab_drag(&hover_pane_id, cx);
            }))
            .child(
                canvas(
                    move |bounds, _, cx| {
                        measure.update(cx, |this, cx| {
                            let resized =
                                this.pane_bounds.get(&measured_pane_id).is_none_or(|old| {
                                    (old.size.width - bounds.size.width).abs() > px(1.)
                                });
                            this.pane_bounds.insert(measured_pane_id.clone(), bounds);
                            if resized {
                                cx.notify();
                            }
                        });
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            )
            .child(strip);
        match content {
            Some(content) => pane_element.child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .child(content),
            ),
            None => pane_element.child(
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(rgb(palette.foreground_muted))
                    .child("Select a tab"),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::EditorKey;
    use std::collections::BTreeMap;

    #[test]
    fn the_same_relative_path_in_two_workspaces_is_two_tabs() {
        let mut tabs: BTreeMap<EditorKey, &str> = BTreeMap::new();
        tabs.insert(EditorKey::new("/repos/a", "README.md"), "a's buffer");
        // Clicking README.md in another workspace must open that workspace's
        // file, not reuse (and save over) the first workspace's buffer.
        assert!(!tabs.contains_key(&EditorKey::new("/repos/b", "README.md")));
        assert_eq!(tabs[&EditorKey::new("/repos/a", "README.md")], "a's buffer");
        // Pane tabs retain the directory in their identity across navigation.
        assert_ne!(
            EditorKey::new("/repos/a", "README.md").tab_id(),
            EditorKey::new("/repos/b", "README.md").tab_id()
        );
    }
}

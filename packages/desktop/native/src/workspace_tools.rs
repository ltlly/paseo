use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Range,
    sync::Arc,
    time::Duration,
};

use gpui::{
    App, Bounds, ClipboardItem, Context, ElementInputHandler, EntityInputHandler, EventEmitter,
    FocusHandle, Focusable, KeyDownEvent, MouseButton, Pixels, Point, Render, ShapedLine,
    TextAlign, TextRun, UTF16Selection, Window, canvas, div, fill, font, point, prelude::*, px,
    rgb, rgba, size,
};
use paseo_desktop::appearance::Palette;
use paseo_desktop::client::Connection;
use serde_json::{Value, json};

use crate::{
    chrome::{icon, loading_spinner},
    settings_view::{SharedSettings, active_palette},
    theme::{font_size, radius},
};
use paseo_desktop::editing::Editing;

#[path = "explorer_presentation.rs"]
mod explorer_presentation;
use explorer_presentation as files_style;
const TREE_INDENT: f32 = files_style::TREE_INDENT;
const ROW_HEIGHT: f32 = files_style::TREE_ICON + 2. * files_style::TREE_PADDING_Y;
/// The authored diff band is the palette's diff colour at 15% alpha.
const DIFF_WASH_ALPHA: u32 = 0x26;

/// Matches the original command center: a short debounce, then one host query.
const FILE_SEARCH_DEBOUNCE: Duration = Duration::from_millis(100);
const FILE_SEARCH_LIMIT: u32 = 100;
const BRANCH_SUGGESTION_LIMIT: u32 = 200;
/// A git action that never lands leaves the row pending forever otherwise.
const ACTION_TIMEOUT: Duration = Duration::from_secs(30);
/// The original app shows a completed action for one second.
const SUCCESS_DISPLAY: Duration = Duration::from_millis(1000);

/// Feature gates this surface reads once from `server_info.features`. A gated
/// feature either runs or says to update the host; there is no fallback path.
#[derive(Clone, Copy, Default)]
pub struct Features {
    pub discard_changes: bool,
    pub fs_entry_ops: bool,
    pub forge_providers: bool,
    pub forge_set_auto_merge: bool,
    pub forge_check_details: bool,
}

impl Features {
    pub fn read(features: &BTreeMap<String, Value>) -> Self {
        let enabled = |name: &str| features.get(name) == Some(&Value::Bool(true));
        Self {
            discard_changes: enabled("checkoutDiscardChanges"),
            fs_entry_ops: enabled("fsEntryOps"),
            forge_providers: enabled("forgeProviders"),
            forge_set_auto_merge: enabled("checkoutForgeSetAutoMerge"),
            forge_check_details: enabled("forgeCheckDetails"),
        }
    }
}

/// One git action row. A reply settles the row it names and nothing else.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum GitOp {
    Commit,
    Pull,
    Push,
    Refresh,
    SwitchBranch,
    StashSave,
    StashPop,
    Discard,
    CreatePr,
    MergePr,
    AutoMerge,
    CheckDetails,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum OpState {
    Pending,
    Success,
    /// The host never answered in time. The daemon may still be running the
    /// action, so the row is not clickable again until a reply or a reconnect
    /// resolves it; it must not read as "safe to resend".
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergeMethod {
    Squash,
    Merge,
    Rebase,
}

impl MergeMethod {
    fn as_str(self) -> &'static str {
        match self {
            MergeMethod::Squash => "squash",
            MergeMethod::Merge => "merge",
            MergeMethod::Rebase => "rebase",
        }
    }

    fn label(self) -> &'static str {
        match self {
            MergeMethod::Squash => "Squash and merge",
            MergeMethod::Merge => "Create a merge commit",
            MergeMethod::Rebase => "Rebase and merge",
        }
    }
}

/// A checkout mutation and the follow-up its success implies.
#[derive(Clone)]
enum RepoOp {
    Commit,
    Pull,
    Push,
    Refresh,
    SwitchBranch(String),
    StashSave,
    StashPop,
    Discard(String),
    CreatePr,
    MergePr,
    AutoMerge,
}

impl RepoOp {
    fn row(&self) -> GitOp {
        match self {
            RepoOp::Commit => GitOp::Commit,
            RepoOp::Pull => GitOp::Pull,
            RepoOp::Push => GitOp::Push,
            RepoOp::Refresh => GitOp::Refresh,
            RepoOp::SwitchBranch(_) => GitOp::SwitchBranch,
            RepoOp::StashSave => GitOp::StashSave,
            RepoOp::StashPop => GitOp::StashPop,
            RepoOp::Discard(_) => GitOp::Discard,
            RepoOp::CreatePr => GitOp::CreatePr,
            RepoOp::MergePr => GitOp::MergePr,
            RepoOp::AutoMerge => GitOp::AutoMerge,
        }
    }
}

#[derive(Clone, Default)]
struct PrCheck {
    name: String,
    status: String,
    url: Option<String>,
    check_run_id: Option<i64>,
    workflow_run_id: Option<i64>,
}

impl PrCheck {
    /// One colour per check, from the forge's own status string.
    fn tone(&self, palette: &Palette) -> u32 {
        match self.status.to_lowercase().as_str() {
            "success" | "successful" | "passed" | "neutral" | "skipped" => {
                palette.status_dot_success
            }
            "failure" | "failed" | "error" | "cancelled" | "canceled" | "timed_out"
            | "action_required" => palette.status_dot_danger,
            "pending" | "queued" | "in_progress" | "running" | "waiting" => {
                palette.status_dot_warning
            }
            _ => palette.foreground_extra_muted,
        }
    }
}

/// The change request of the current branch, as `checkout_pr_status_response`
/// reports it. Absent fields stay absent so a missing forge fact never reads as
/// permissive.
#[derive(Clone, Default)]
struct PrStatus {
    forge: String,
    number: Option<i64>,
    url: String,
    title: String,
    state: String,
    is_draft: bool,
    is_merged: bool,
    mergeable: String,
    checks: Vec<PrCheck>,
    checks_status: Option<String>,
    base_ref: String,
    repo_owner: Option<String>,
    repo_name: Option<String>,
    merge_methods: Vec<MergeMethod>,
    /// Why a direct merge is withheld. `None` only when the forge's own facts
    /// report the change request ready; a missing fact withholds the action.
    direct_merge_blocker: Option<&'static str>,
    /// Auto-merge is already scheduled on the forge.
    auto_merge_enabled: bool,
    /// Why enabling auto-merge is not offered. `None` when the forge allows it.
    auto_merge_enable_blocker: Option<&'static str>,
    can_disable_auto_merge: bool,
}

impl PrStatus {
    fn is_open(&self) -> bool {
        self.state.eq_ignore_ascii_case("open") || self.state.eq_ignore_ascii_case("opened")
    }

    /// Merge is offered only for an open, unmerged, non-draft change request the
    /// forge reports as directly mergeable, and only by a method its policy
    /// allows. A missing readiness fact is not permission.
    fn can_merge(&self) -> bool {
        self.is_open()
            && !self.is_draft
            && !self.is_merged
            && self.mergeable == "MERGEABLE"
            && self.direct_merge_blocker.is_none()
            && !self.merge_methods.is_empty()
    }

    fn checks_summary(&self, palette: &Palette) -> String {
        if let Some(status) = &self.checks_status
            && !status.is_empty()
        {
            return status.to_lowercase();
        }
        if self.checks.is_empty() {
            return "no checks".into();
        }
        let failed = self
            .checks
            .iter()
            .filter(|check| check.tone(palette) == palette.status_dot_danger)
            .count();
        if failed > 0 {
            return format!("{failed} failing");
        }
        let passing = self
            .checks
            .iter()
            .filter(|check| check.tone(palette) == palette.status_dot_success)
            .count();
        format!("{passing}/{} passing", self.checks.len())
    }

    fn checks_tone(&self, palette: &Palette) -> u32 {
        if self
            .checks
            .iter()
            .any(|c| c.tone(palette) == palette.status_dot_danger)
        {
            return palette.status_dot_danger;
        }
        if self
            .checks
            .iter()
            .all(|c| c.tone(palette) == palette.status_dot_success)
        {
            return palette.status_dot_success;
        }
        palette.status_dot_warning
    }
}

/// The one check the user asked the forge for details about.
#[derive(Clone, Default)]
struct CheckDetails {
    name: String,
    conclusion: Option<String>,
    url: Option<String>,
    failed_jobs: Vec<String>,
}

/// A file the quick-open search matched.
#[derive(Clone)]
struct SearchHit {
    path: String,
    name: String,
    directory: String,
}

#[derive(Default)]
struct Search {
    hits: Vec<SearchHit>,
    loading: bool,
    error: Option<String>,
    /// Bumped per keystroke; a reply for an older generation is dropped.
    generation: u64,
}

impl Search {
    fn clear(&mut self) {
        self.hits.clear();
        self.loading = false;
        self.error = None;
    }

    /// A new workspace clears the results but keeps the counter moving: a
    /// debounce armed in the old workspace must never match a fresh search.
    fn start_in_new_workspace(&mut self) {
        self.clear();
        self.generation += 1;
    }

    /// A reply lands only when no newer keystroke has taken over. A stale reply
    /// leaves the current search's loading state and results alone.
    fn accept_reply(&mut self, generation: u64, payload: &Value) {
        if generation != self.generation {
            return;
        }
        self.loading = false;
        if let Some(error) = payload["error"].as_str() {
            self.hits.clear();
            self.error = Some(error.to_owned());
        } else {
            self.hits = parse_search_hits(payload);
            self.error = None;
        }
    }
}

#[derive(Clone)]
struct StashEntry {
    index: i64,
    branch: Option<String>,
    message: String,
}

/// The switch that may only run from the stash reply that armed it. A late
/// reply for an older stash request must not switch branches.
struct StashSwitch {
    branch: String,
    request_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ScriptLinkKind {
    Public,
    Paseo,
    Direct,
}

/// One URL a running service can be reached at.
#[derive(Clone)]
pub struct ScriptTarget {
    pub kind: ScriptLinkKind,
    pub label: String,
    pub url: String,
}

impl ScriptTarget {
    pub fn heading(&self) -> &'static str {
        match self.kind {
            ScriptLinkKind::Public => "public",
            ScriptLinkKind::Paseo => "paseo",
            ScriptLinkKind::Direct => "direct",
        }
    }
}

/// Which anchored menu the dock is showing over the changes list.
#[derive(Clone, Copy, Debug, PartialEq)]
enum GitMenuKind {
    Branch,
    PullRequest,
    Checks,
}

#[derive(Clone)]
struct GitMenu {
    kind: GitMenuKind,
    bounds: Bounds<Pixels>,
}

#[derive(Clone)]
enum GitMenuAction {
    SwitchBranch(String),
    RestoreStash(i64),
    Merge(MergeMethod),
    AutoMerge {
        enabled: bool,
        method: Option<MergeMethod>,
    },
    OpenUrl(String),
    CheckDetails {
        name: String,
        check_run_id: Option<i64>,
        workflow_run_id: Option<i64>,
    },
}

/// A destructive action only runs after the user confirms it.
#[derive(Clone)]
struct Confirm {
    message: String,
    action: ConfirmAction,
}

#[derive(Clone)]
enum ConfirmAction {
    Discard(String),
    MergePr(MergeMethod),
    StashAndSwitch(String),
}

/// A file the explorer should open in an editor tab.
pub struct OpenFile(pub String);
/// A script terminal the user asked to view.
pub struct ViewTerminal(pub String);

#[derive(Clone, Copy, PartialEq)]
pub enum DockTab {
    Files,
    Changes,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Sort {
    Name,
    Modified,
    Size,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum EntryKind {
    File,
    Directory,
}

#[derive(Clone)]
struct Entry {
    name: String,
    path: String,
    kind: EntryKind,
    size: u64,
    modified_at: String,
}

#[derive(Clone, Default)]
struct Directory {
    entries: Vec<Entry>,
}

/// Status facts from `checkout_status_response`/`checkout_status_update`.
#[derive(Clone, Default)]
#[allow(dead_code)] // full status surface is parsed even though the dock shows a subset
pub struct CheckoutStatus {
    pub is_git: bool,
    pub repo_root: Option<String>,
    pub current_branch: Option<String>,
    pub is_dirty: Option<bool>,
    pub ahead: Option<i64>,
    pub behind: Option<i64>,
}

#[derive(Clone)]
struct DiffFile {
    path: String,
    is_new: bool,
    is_deleted: bool,
    additions: u64,
    deletions: u64,
    binary: bool,
    too_large: bool,
    hunks: Vec<Hunk>,
}

#[derive(Clone)]
struct Hunk {
    header: String,
    lines: Vec<(LineKind, String)>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum LineKind {
    Add,
    Remove,
    Context,
}

#[derive(Clone)]
pub struct Script {
    pub name: String,
    pub is_service: bool,
    pub lifecycle: String,
    pub health: Option<String>,
    pub exit_code: Option<i64>,
    pub terminal_id: Option<String>,
    /// The daemon's local proxy for this service, when it has one.
    pub local_url: Option<String>,
    /// The public (tunnel or service proxy) URL, when the host exposes one.
    pub public_url: Option<String>,
    /// The pre-split proxy URL older daemons send; classified by reachability.
    pub proxy_url: Option<String>,
    /// The port the service listens on, when the daemon reported one.
    pub port: Option<i64>,
}

impl Script {
    /// The one colour the row's icon is drawn in, matching the original tones.
    pub fn tone(&self) -> ScriptTone {
        if self.lifecycle != "running" {
            return ScriptTone::Idle;
        }
        if !self.is_service {
            return ScriptTone::Running;
        }
        match self.health.as_deref() {
            Some("healthy") => ScriptTone::Healthy,
            Some("unhealthy") => ScriptTone::Unhealthy,
            _ => ScriptTone::Running,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ScriptTone {
    Idle,
    Running,
    Healthy,
    Unhealthy,
}

impl ScriptTone {
    pub fn color(self, palette: &Palette) -> u32 {
        match self {
            ScriptTone::Idle => palette.foreground_muted,
            ScriptTone::Running => palette.status_dot_running,
            ScriptTone::Healthy => palette.status_dot_success,
            ScriptTone::Unhealthy => palette.status_dot_danger,
        }
    }
}

/// A workspace script action chosen from the header menu.
#[derive(Clone)]
pub struct ScriptAction(pub String, pub ScriptActionKind);

#[derive(Clone, Copy, PartialEq)]
pub enum ScriptActionKind {
    Start,
    Stop,
    Restart,
    ViewTerminal,
    CopyUrl,
    OpenUrl,
}

/// One pending host request, so late failures land on the surface that asked.
enum Pending {
    Directory(String),
    Create {
        parent: String,
    },
    Rename {
        parent: String,
    },
    Duplicate {
        parent: String,
    },
    Delete {
        parent: String,
    },
    CheckoutStatus,
    Diff,
    ScriptStart(String),
    ScriptStop(String),
    /// A filename search reply, tagged with the generation that asked for it.
    Search(u64),
    Branches,
    Stashes,
    PrStatus,
    CheckDetails,
    Git(RepoOp),
}

#[derive(Clone)]
enum MenuAction {
    NewFile(String),
    NewFolder(String),
    Rename(String),
    Duplicate { path: String, parent: String },
    Delete { path: String, parent: String },
    CopyPath { path: String, absolute: bool },
    Collapse(String),
}

impl MenuAction {
    fn label(&self) -> &'static str {
        match self {
            MenuAction::NewFile(_) => "New file",
            MenuAction::NewFolder(_) => "New folder",
            MenuAction::Rename(_) => "Rename…",
            MenuAction::Duplicate { .. } => "Duplicate",
            MenuAction::Delete { .. } => "Delete",
            MenuAction::CopyPath {
                absolute: false, ..
            } => "Copy path",
            MenuAction::CopyPath { absolute: true, .. } => "Copy full path",
            MenuAction::Collapse(_) => "Collapse",
        }
    }

    fn clone_action(&self) -> Self {
        match self {
            MenuAction::NewFile(path) => MenuAction::NewFile(path.clone()),
            MenuAction::NewFolder(path) => MenuAction::NewFolder(path.clone()),
            MenuAction::Rename(path) => MenuAction::Rename(path.clone()),
            MenuAction::Duplicate { path, parent } => MenuAction::Duplicate {
                path: path.clone(),
                parent: parent.clone(),
            },
            MenuAction::Delete { path, parent } => MenuAction::Delete {
                path: path.clone(),
                parent: parent.clone(),
            },
            MenuAction::CopyPath { path, absolute } => MenuAction::CopyPath {
                path: path.clone(),
                absolute: *absolute,
            },
            MenuAction::Collapse(path) => MenuAction::Collapse(path.clone()),
        }
    }
}

#[derive(Clone)]
struct Menu {
    path: String,
    actions: Vec<MenuAction>,
    bounds: Bounds<Pixels>,
}

/// What the one dock text field is doing right now.
#[derive(Clone, Copy, Debug, PartialEq)]
enum InputKind {
    Create,
    Rename,
    Search,
}

/// The single-line input for creating or renaming an entry, or for the
/// filename search. One field owns the platform input handler at a time.
struct EntryInput {
    /// The entry being renamed, or the parent directory for a new entry.
    target: String,
    kind: InputKind,
    directory: bool,
    query: Editing,
    focus: FocusHandle,
    line: Option<ShapedLine>,
    bounds: Bounds<Pixels>,
    scroll_x: Pixels,
    selecting: bool,
}

impl EntryInput {
    fn caret(&self) -> Point<Pixels> {
        self.bounds.origin
            + point(
                self.line
                    .as_ref()
                    .map_or(px(0.), |line| line.x_for_index(self.query.cursor()))
                    - self.scroll_x,
                px(4.),
            )
    }
}

/// A workspace tools surface: the explorer tree, the working changes, and the
/// workspace's scripts state, all over existing host RPCs.
pub struct WorkspaceTools {
    connection: Arc<Connection>,
    /// The window's settings. Appearance resolves once per render so the dock
    /// follows a theme change without rebuilding the surface.
    shared: SharedSettings,
    workspace_id: String,
    cwd: String,
    /// The daemon host without scheme or port; direct service URLs need it.
    host: String,
    /// Whether this connection names the machine the client runs on. Paseo's
    /// local proxy URLs only work from that machine.
    local_endpoint: bool,
    features: Features,
    pub tab: DockTab,
    owns_window_corner: bool,
    // Explorer
    directories: BTreeMap<String, Directory>,
    expanded: BTreeSet<String>,
    loading: BTreeSet<String>,
    entry_error: Option<String>,
    pub sort: Sort,
    pub show_hidden: bool,
    file_icon_size: i32,
    file_images: BTreeMap<&'static str, Arc<gpui::RenderImage>>,
    menu: Option<Menu>,
    input: Option<EntryInput>,
    search: Search,
    // Changes
    status: Option<CheckoutStatus>,
    status_error: Option<String>,
    status_pending: bool,
    diff: Option<Vec<DiffFile>>,
    diff_error: Option<String>,
    diff_too_large: bool,
    diff_loading: bool,
    selected_diff: Option<String>,
    // Git and forge actions
    ops: BTreeMap<GitOp, OpState>,
    /// The request id each action row is currently waiting on. A reply settles
    /// a row only while it is still the newest request for that row.
    active_ops: BTreeMap<GitOp, String>,
    git_error: Option<String>,
    git_menu: Option<GitMenu>,
    confirm: Option<Confirm>,
    /// A stash-and-switch armed by one stash request.
    stash_and_switch: Option<StashSwitch>,
    branches: Vec<String>,
    branches_error: Option<String>,
    branches_loading: bool,
    stashes: Vec<StashEntry>,
    pr: Option<PrStatus>,
    pr_error: Option<String>,
    pr_loading: bool,
    check_details: Option<CheckDetails>,
    check_details_error: Option<String>,
    // Scripts
    scripts: BTreeMap<String, Script>,
    /// A restart waits for the daemon to report the script stopped, then starts it.
    restart: Option<String>,
    script_pending: Option<String>,
    script_error: Option<String>,
    // Bookkeeping
    pending: BTreeMap<String, Pending>,
    events_subscribed: bool,
    connected: bool,
    /// Bumped per action so only the newest success flash clears the row.
    flash: u64,
    focus: FocusHandle,
}

impl EventEmitter<OpenFile> for WorkspaceTools {}
impl EventEmitter<ViewTerminal> for WorkspaceTools {}

impl WorkspaceTools {
    pub fn new(
        connection: Arc<Connection>,
        shared: SharedSettings,
        cx: &mut Context<Self>,
    ) -> Self {
        let host = connection.host().to_owned();
        let local_endpoint = connection.is_local_endpoint();
        let mut tools = Self {
            connection,
            shared,
            workspace_id: String::new(),
            cwd: String::new(),
            host,
            local_endpoint,
            features: Features::default(),
            tab: DockTab::Files,
            owns_window_corner: false,
            directories: BTreeMap::new(),
            expanded: BTreeSet::from([".".to_owned()]),
            loading: BTreeSet::new(),
            entry_error: None,
            sort: Sort::Name,
            show_hidden: true,
            file_icon_size: 0,
            file_images: BTreeMap::new(),
            menu: None,
            input: None,
            search: Search::default(),
            status: None,
            status_error: None,
            status_pending: false,
            diff: None,
            diff_error: None,
            diff_too_large: false,
            diff_loading: false,
            selected_diff: None,
            ops: BTreeMap::new(),
            active_ops: BTreeMap::new(),
            git_error: None,
            git_menu: None,
            confirm: None,
            stash_and_switch: None,
            branches: Vec::new(),
            branches_error: None,
            branches_loading: false,
            stashes: Vec::new(),
            pr: None,
            pr_error: None,
            pr_loading: false,
            check_details: None,
            check_details_error: None,
            scripts: BTreeMap::new(),
            restart: None,
            script_pending: None,
            script_error: None,
            pending: BTreeMap::new(),
            events_subscribed: false,
            connected: true,
            flash: 0,
            focus: cx.focus_handle(),
        };
        tools.subscribe_events();
        tools
    }

    pub fn set_window_corner(&mut self, owned: bool, cx: &mut Context<Self>) {
        if self.owns_window_corner != owned {
            self.owns_window_corner = owned;
            cx.notify();
        }
    }
    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn local_endpoint(&self) -> bool {
        self.local_endpoint
    }

    /// The host feature gates, read once when the daemon introduces itself.
    pub fn set_features(&mut self, features: &BTreeMap<String, Value>) {
        self.features = Features::read(features);
    }

    pub fn cwd(&self) -> &str {
        &self.cwd
    }

    pub fn scripts(&self) -> Vec<&Script> {
        self.scripts.values().collect()
    }

    pub fn script_pending(&self) -> Option<&str> {
        self.script_pending.as_deref()
    }

    pub fn script_error(&self) -> Option<&str> {
        self.script_error.as_deref()
    }

    /// Enter a workspace: reset the explorer and changes, adopt its scripts.
    pub fn open_workspace(
        &mut self,
        workspace_id: String,
        cwd: String,
        scripts: Vec<Script>,
        cx: &mut Context<Self>,
    ) {
        if self.workspace_id == workspace_id {
            return;
        }
        self.connection.unsubscribe("checkout-diff");
        self.workspace_id = workspace_id;
        self.cwd = cwd;
        self.tab = DockTab::Files;
        self.directories.clear();
        self.expanded = BTreeSet::from([".".to_owned()]);
        self.loading.clear();
        self.entry_error = None;
        self.menu = None;
        self.input = None;
        self.status = None;
        self.status_error = None;
        self.status_pending = false;
        self.diff = None;
        self.diff_error = None;
        self.diff_too_large = false;
        self.diff_loading = false;
        self.selected_diff = None;
        self.search.start_in_new_workspace();
        self.ops.clear();
        self.active_ops.clear();
        self.git_error = None;
        self.git_menu = None;
        self.confirm = None;
        self.stash_and_switch = None;
        self.branches.clear();
        self.branches_error = None;
        self.branches_loading = false;
        self.stashes.clear();
        self.pr = None;
        self.pr_error = None;
        self.pr_loading = false;
        self.check_details = None;
        self.check_details_error = None;
        self.scripts = scripts
            .into_iter()
            .map(|script| (script.name.clone(), script))
            .collect();
        self.restart = None;
        self.script_pending = None;
        self.script_error = None;
        self.pending.clear();
        self.refresh(cx);
        cx.notify();
    }

    pub fn disconnected(&mut self, cx: &mut Context<Self>) {
        self.connected = false;
        self.pending.clear();
        self.loading.clear();
        self.status_pending = false;
        self.diff_loading = false;
        self.restart = None;
        self.script_pending = None;
        self.input = None;
        self.menu = None;
        self.git_menu = None;
        self.confirm = None;
        self.branches_loading = false;
        self.pr_loading = false;
        // A mutation is never replayed on reconnect: the daemon may have applied
        // it before the socket dropped, and its reply is gone.
        self.ops.clear();
        self.active_ops.clear();
        if self.status.is_none() && self.status_error.is_none() {
            self.status_error = Some("Host is disconnected.".into());
        }
        cx.notify();
    }

    /// Reconnect: the host dropped every subscription.
    pub fn reconnected(&mut self, cx: &mut Context<Self>) {
        self.connected = true;
        self.events_subscribed = false;
        self.status_error = None;
        self.diff_error = None;
        self.entry_error = None;
        self.git_error = None;
        self.pr_error = None;
        self.pending.clear();
        self.ops.clear();
        self.active_ops.clear();
        self.search.start_in_new_workspace();
        self.subscribe_events();
        self.refresh(cx);
        cx.notify();
    }

    pub fn toggle_tab(&mut self, tab: DockTab, cx: &mut Context<Self>) {
        self.tab = tab;
        self.selected_diff = None;
        self.menu = None;
        self.git_menu = None;
        self.confirm = None;
        cx.notify();
    }

    /// A script action chosen in the header menu.
    pub fn script_action(
        &mut self,
        name: String,
        action: ScriptActionKind,
        cx: &mut Context<Self>,
    ) {
        match action {
            ScriptActionKind::Start => self.start_script(name, cx),
            ScriptActionKind::Stop => self.stop_script(name, cx),
            ScriptActionKind::Restart => {
                // Stop now; the start happens when the daemon reports it stopped.
                self.restart = Some(name.clone());
                self.stop_script(name, cx);
            }
            ScriptActionKind::ViewTerminal => {
                if let Some(terminal_id) =
                    self.scripts.get(&name).and_then(|s| s.terminal_id.clone())
                {
                    cx.emit(ViewTerminal(terminal_id));
                }
            }
            ScriptActionKind::CopyUrl => {
                // The scripts menu passes the exact URL it drew; copying must not
                // re-derive a target from the script name.
                if let Some(url) = action_url(&name) {
                    cx.write_to_clipboard(ClipboardItem::new_string(url.to_owned()));
                }
            }
            ScriptActionKind::OpenUrl => {
                // There is no embedded browser in the native client yet; the OS
                // opens the service URL instead. That is a migration gap, not a
                // equivalent surface.
                if let Some(url) = action_url(&name) {
                    cx.open_url(url);
                }
            }
        }
    }

    fn subscribe_events(&mut self) {
        if self.events_subscribed || !self.connected {
            return;
        }
        match self.connection.subscribe(
            "events",
            "session.events.set_subscription.request",
            json!({"events":["checkout_status_update","script_status_update"]}),
        ) {
            Ok(_) => self.events_subscribed = true,
            Err(error) => self.status_error = Some(error.to_string()),
        }
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        if !self.connected || self.cwd.is_empty() {
            return;
        }
        self.request_directory(".", cx);
        match self
            .connection
            .request("checkout_status_request", json!({"cwd":self.cwd}))
        {
            Ok(id) => {
                self.pending.insert(id, Pending::CheckoutStatus);
                self.status_pending = true;
            }
            Err(error) => self.status_error = Some(error.to_string()),
        }
        match self.connection.subscribe(
            "checkout-diff",
            "subscribe_checkout_diff_request",
            json!({"cwd":self.cwd,"compare":{"mode":"uncommitted"}}),
        ) {
            Ok(id) => {
                self.pending.insert(id, Pending::Diff);
                self.diff_loading = true;
            }
            Err(error) => self.diff_error = Some(error.to_string()),
        }
        cx.notify();
    }

    fn request_directory(&mut self, path: &str, cx: &mut Context<Self>) {
        match self.connection.request(
            "file_explorer_request",
            json!({"cwd":self.cwd,"path":path,"mode":"list"}),
        ) {
            Ok(id) => {
                self.pending.insert(id, Pending::Directory(path.to_owned()));
                self.loading.insert(path.to_owned());
            }
            Err(error) => {
                self.loading.remove(path);
                self.entry_error = Some(error.to_string());
            }
        }
        cx.notify();
    }

    /// The scripts list arrived with a workspace descriptor payload.
    pub fn adopt_workspace_scripts(&mut self, workspace_id: &str, payload: &Value) {
        if workspace_id != self.workspace_id {
            return;
        }
        if let Some(scripts) = payload["scripts"].as_array() {
            for script in scripts.iter().filter_map(parse_script) {
                self.scripts.insert(script.name.clone(), script);
            }
        }
    }

    /// Whether this tools surface answers a message kind/payload pair.
    pub fn receive(&mut self, kind: &str, payload: &Value, cx: &mut Context<Self>) -> bool {
        if !owns_message(kind, payload, &self.workspace_id, &self.cwd, &self.pending) {
            return false;
        }
        if kind == "script_status_update"
            && payload["workspaceId"].as_str() == Some(self.workspace_id.as_str())
        {
            if let Some(scripts) = payload["scripts"].as_array() {
                for script in scripts.iter().filter_map(parse_script) {
                    let name = script.name.clone();
                    let stopped = script.lifecycle != "running";
                    self.scripts.insert(name.clone(), script);
                    if stopped && self.script_pending.as_deref() == Some(name.as_str()) {
                        // The stop reply can no longer change anything.
                        self.script_pending = None;
                    }
                    // A restart waits for the daemon to report the stop.
                    if stopped
                        && should_start_restart(
                            self.restart.as_deref(),
                            &name,
                            self.script_pending.as_deref(),
                        )
                    {
                        self.restart = None;
                        self.start_script(name, cx);
                    }
                }
            }
            cx.notify();
            return true;
        }
        if kind == "checkout_status_update" && payload["cwd"].as_str() == Some(self.cwd.as_str()) {
            // Status pushes carry `prStatus` metadata, but the dock refreshes the
            // change request from a status request or an action instead of
            // asking on every push.
            self.status = Some(parse_status(&payload));
            self.status_pending = false;
            self.status_error = None;
            cx.notify();
            return true;
        }
        if kind == "checkout_diff_update" {
            self.apply_diff(&payload);
            cx.notify();
            return true;
        }
        let Some(request_id) = payload["requestId"].as_str().map(str::to_owned) else {
            return false;
        };
        let Some(pending) = self.pending.remove(&request_id) else {
            return false;
        };
        let error = payload["error"].as_str().map(str::to_owned);
        // Read before the match moves the script name out of the pending op.
        let stop_reply = matches!(&pending, Pending::ScriptStop(_));
        match pending {
            Pending::Directory(path) => {
                self.loading.remove(&path);
                if let Some(error) = error {
                    self.entry_error = Some(error);
                } else if let Some(directory) = payload["directory"].as_object() {
                    let path = directory
                        .get("path")
                        .and_then(Value::as_str)
                        .unwrap_or(&path)
                        .to_owned();
                    self.directories.insert(
                        path.clone(),
                        parse_directory(&Value::Object(directory.clone())),
                    );
                    self.loading.remove(&path);
                } else {
                    self.entry_error = Some("Directory listing unavailable.".into());
                }
            }
            Pending::Create { parent }
            | Pending::Rename { parent }
            | Pending::Duplicate { parent }
            | Pending::Delete { parent } => {
                let success = payload["success"] != Value::Bool(false) && error.is_none();
                if !success {
                    self.entry_error =
                        Some(error.unwrap_or_else(|| "The host refused the change.".into()));
                }
                self.input = None;
                self.request_directory(&parent, cx);
            }
            Pending::CheckoutStatus => {
                self.status_pending = false;
                if let Some(error) = checkout_error_message(&payload) {
                    self.status_error = Some(error);
                } else {
                    self.status = Some(parse_status(&payload));
                    self.status_error = None;
                    self.maybe_request_pr(cx);
                }
            }
            Pending::Search(generation) => self.search.accept_reply(generation, &payload),
            Pending::Branches => {
                self.branches_loading = false;
                if let Some(error) = payload["error"].as_str() {
                    self.branches_error = Some(error.to_owned());
                } else {
                    self.branches = payload["branches"]
                        .as_array()
                        .map(|branches| {
                            branches
                                .iter()
                                .filter_map(|branch| branch.as_str().map(str::to_owned))
                                .collect()
                        })
                        .unwrap_or_default();
                    self.branches_error = None;
                }
            }
            Pending::Stashes => {
                if let Some(error) = checkout_error_message(&payload) {
                    self.git_error = Some(error);
                } else {
                    self.stashes = parse_stashes(&payload);
                }
            }
            Pending::PrStatus => {
                self.pr_loading = false;
                if let Some(error) = checkout_error_message(&payload) {
                    self.pr_error = Some(error);
                } else {
                    self.pr = parse_pr_status(&payload);
                    self.pr_error = None;
                }
            }
            Pending::CheckDetails => {
                // A stale details reply must not clear a newer request's row.
                if settles_row(
                    self.active_ops
                        .get(&GitOp::CheckDetails)
                        .map(String::as_str),
                    &request_id,
                ) {
                    self.active_ops.remove(&GitOp::CheckDetails);
                    self.ops.remove(&GitOp::CheckDetails);
                    if let Some(error) = checkout_error_message(&payload) {
                        self.check_details_error = Some(error);
                    } else {
                        self.check_details = parse_check_details(&payload);
                        self.check_details_error = None;
                    }
                }
            }
            Pending::Git(op) => self.settle_git(op, &request_id, &payload, error, cx),
            Pending::Diff => {
                self.diff_loading = false;
                if let Some(error) = checkout_error_message(&payload) {
                    self.diff_error = Some(error);
                } else {
                    self.apply_diff(&payload);
                }
            }
            Pending::ScriptStart(name) | Pending::ScriptStop(name) => {
                // A reply only answers the operation it names: a late stop
                // reply for another script must not unblock a newer start.
                clear_script_pending(&mut self.script_pending, &name);
                if let Some(error) = error {
                    self.script_error = Some(error);
                } else {
                    if let Some(script) = payload.get("script").and_then(parse_script_option) {
                        self.scripts.insert(script.name.clone(), script);
                        self.script_error = None;
                    }
                    // The stop reply may have lost the race against a
                    // script_status_update; fire an armed restart here too, but
                    // only once nothing else is mid-flight, because start_script
                    // refuses then and taking the restart would drop it.
                    if stop_reply
                        && self.script_pending.is_none()
                        && let Some(armed) = self.restart.take()
                    {
                        self.start_script(armed, cx);
                    }
                }
            }
        }
        cx.notify();
        true
    }

    fn apply_diff(&mut self, payload: &Value) {
        if payload["cwd"].as_str().is_some_and(|cwd| cwd != self.cwd) {
            return;
        }
        if let Some(error) = checkout_error_message(payload) {
            self.diff_error = Some(error);
            return;
        }
        self.diff_error = None;
        self.diff_too_large = payload["diffTooLarge"] == Value::Bool(true);
        self.diff = payload["files"].as_array().map(|files| {
            files
                .iter()
                .filter_map(parse_diff_file)
                .collect::<Vec<DiffFile>>()
        });
    }

    fn visible<'a>(&self, entries: &'a [Entry]) -> Vec<&'a Entry> {
        entries
            .iter()
            .filter(|entry| self.show_hidden || !entry.name.starts_with('.'))
            .collect()
    }

    /// The flattened tree rows: visible, sorted, expanded directories inlined.
    fn rows(&self) -> Vec<(usize, &Entry)> {
        let mut rows = Vec::new();
        let Some(root) = self.directories.get(".") else {
            return rows;
        };
        let mut pending: Vec<(usize, &Entry)> = self
            .sorted(self.visible(&root.entries))
            .into_iter()
            .rev()
            .map(|entry| (0usize, entry))
            .collect();
        while let Some((depth, entry)) = pending.pop() {
            rows.push((depth, entry));
            if entry.kind == EntryKind::Directory
                && self.expanded.contains(&entry.path)
                && let Some(directory) = self.directories.get(&entry.path)
            {
                let children: Vec<(usize, &Entry)> = self
                    .sorted(self.visible(&directory.entries))
                    .into_iter()
                    .map(|entry| (depth + 1, entry))
                    .collect();
                for child in children.into_iter().rev() {
                    pending.push(child);
                }
            }
        }
        rows
    }

    fn sorted<'a>(&'a self, entries: Vec<&'a Entry>) -> Vec<&'a Entry> {
        let mut entries = entries;
        entries.sort_by(|a, b| {
            if a.kind != b.kind {
                return if a.kind == EntryKind::Directory {
                    std::cmp::Ordering::Less
                } else {
                    std::cmp::Ordering::Greater
                };
            }
            match self.sort {
                Sort::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                Sort::Modified => b.modified_at.cmp(&a.modified_at),
                Sort::Size => b.size.cmp(&a.size),
            }
        });
        entries
    }

    fn toggle_directory(&mut self, path: &str, cx: &mut Context<Self>) {
        if self.expanded.remove(path) {
            cx.notify();
            return;
        }
        self.expanded.insert(path.to_owned());
        if !self.directories.contains_key(path) && !self.loading.contains(path) {
            self.request_directory(path, cx);
        }
        cx.notify();
    }

    fn begin_create(
        &mut self,
        parent: String,
        directory: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut input = EntryInput {
            target: parent,
            kind: InputKind::Create,
            directory,
            query: Editing::default(),
            focus: cx.focus_handle(),
            line: None,
            bounds: Bounds::default(),
            scroll_x: px(0.),
            selecting: false,
        };
        input.query.select(0, false);
        self.input = Some(input);
        self.menu = None;
        if let Some(input) = &self.input {
            input.focus.clone().focus(window, cx);
        }
        cx.notify();
    }

    fn begin_rename(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self
            .directories
            .values()
            .flat_map(|directory| directory.entries.iter())
            .find(|entry| entry.path == path)
            .cloned()
        else {
            return;
        };
        let mut input = EntryInput {
            target: entry.path.clone(),
            kind: InputKind::Rename,
            directory: entry.kind == EntryKind::Directory,
            query: Editing::default(),
            focus: cx.focus_handle(),
            line: None,
            bounds: Bounds::default(),
            scroll_x: px(0.),
            selecting: false,
        };
        input.query.text = entry.name.clone();
        let end = entry.name.len();
        input.query.select(end, false);
        self.input = Some(input);
        self.menu = None;
        if let Some(input) = &self.input {
            input.focus.clone().focus(window, cx);
        }
        cx.notify();
    }

    fn commit_input(&mut self, cx: &mut Context<Self>) {
        let Some(input) = &self.input else {
            return;
        };
        let kind = input.kind;
        if kind == InputKind::Search {
            // Enter opens the first match, like the original command center.
            let hit = self.search.hits.first().map(|hit| hit.path.clone());
            self.input = None;
            self.search.generation += 1;
            self.reset_search_results();
            if let Some(path) = hit {
                cx.emit(OpenFile(path));
            }
            cx.notify();
            return;
        }
        let name = input.query.text.trim().to_owned();
        if name.is_empty() || name.contains('/') {
            self.input = None;
            cx.notify();
            return;
        }
        let (request_kind, request, pending) = if kind == InputKind::Create {
            (
                "fs.entry.create.request",
                json!({
                    "cwd":self.cwd,
                    "parentPath":input.target,
                    "name":name,
                    "kind":if input.directory { "directory" } else { "file" },
                }),
                Pending::Create {
                    parent: input.target.clone(),
                },
            )
        } else {
            (
                "fs.entry.rename.request",
                json!({
                    "cwd":self.cwd,
                    "path":input.target,
                    "name":name,
                }),
                Pending::Rename {
                    parent: parent_of(&input.target),
                },
            )
        };
        match self.connection.request(request_kind, request) {
            Ok(id) => {
                self.pending.insert(id, pending);
                self.input = None;
            }
            Err(error) => self.entry_error = Some(error.to_string()),
        }
        cx.notify();
    }

    fn duplicate(&mut self, path: String, parent: String, cx: &mut Context<Self>) {
        match self.connection.request(
            "fs.entry.duplicate.request",
            json!({"cwd":self.cwd,"path":path}),
        ) {
            Ok(id) => {
                self.pending.insert(id, Pending::Duplicate { parent });
                self.menu = None;
            }
            Err(error) => self.entry_error = Some(error.to_string()),
        }
        cx.notify();
    }

    fn delete(&mut self, path: String, parent: String, cx: &mut Context<Self>) {
        match self.connection.request(
            "fs.entry.delete.request",
            json!({"cwd":self.cwd,"path":path}),
        ) {
            Ok(id) => {
                self.pending.insert(id, Pending::Delete { parent });
                self.menu = None;
            }
            Err(error) => self.entry_error = Some(error.to_string()),
        }
        cx.notify();
    }

    fn copy_path(&mut self, path: String, absolute: bool, cx: &mut Context<Self>) {
        let full = if path == "." {
            self.cwd.clone()
        } else if absolute {
            format!("{}/{}", self.cwd.trim_end_matches('/'), path)
        } else {
            path
        };
        cx.write_to_clipboard(ClipboardItem::new_string(full));
        self.menu = None;
        cx.notify();
    }

    fn start_script(&mut self, name: String, cx: &mut Context<Self>) {
        if self.script_pending.is_some() {
            return;
        }
        match self.connection.request(
            "workspace.script.start.request",
            json!({"workspaceId":self.workspace_id,"scriptName":name}),
        ) {
            Ok(id) => {
                self.pending.insert(id, Pending::ScriptStart(name.clone()));
                self.script_pending = Some(name);
                self.script_error = None;
            }
            Err(error) => self.script_error = Some(error.to_string()),
        }
        cx.notify();
    }

    fn stop_script(&mut self, name: String, cx: &mut Context<Self>) {
        if self.script_pending.is_some() {
            return;
        }
        match self.connection.request(
            "workspace.script.stop.request",
            json!({"workspaceId":self.workspace_id,"scriptName":name}),
        ) {
            Ok(id) => {
                self.pending.insert(id, Pending::ScriptStop(name.clone()));
                self.script_pending = Some(name);
                self.script_error = None;
            }
            Err(error) => self.script_error = Some(error.to_string()),
        }
        cx.notify();
    }
}

impl WorkspaceTools {
    fn reset_search_results(&mut self) {
        self.search.clear();
    }

    /// Filename search reuses the one dock text field, so the platform input
    /// handler and IME ownership stay in one place.
    fn begin_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut input = EntryInput {
            target: String::new(),
            kind: InputKind::Search,
            directory: false,
            query: Editing::default(),
            focus: cx.focus_handle(),
            line: None,
            bounds: Bounds::default(),
            scroll_x: px(0.),
            selecting: false,
        };
        input.query.select(0, false);
        self.input = Some(input);
        self.menu = None;
        self.search.generation += 1;
        self.reset_search_results();
        if let Some(input) = &self.input {
            input.focus.clone().focus(window, cx);
        }
        cx.notify();
    }

    fn search_query(&self) -> String {
        self.input
            .as_ref()
            .filter(|input| input.kind == InputKind::Search)
            .map(|input| input.query.text.clone())
            .unwrap_or_default()
    }

    fn schedule_search(&mut self, cx: &mut Context<Self>) {
        self.search.generation += 1;
        let generation = self.search.generation;
        let query = self.search_query();
        if query.trim().is_empty() || !self.connected {
            self.reset_search_results();
            cx.notify();
            return;
        }
        self.search.loading = true;
        self.search.error = None;
        let timer = cx.background_executor().timer(FILE_SEARCH_DEBOUNCE);
        cx.spawn(async move |this, cx| {
            timer.await;
            let _ = this.update(cx, |this, cx| this.issue_search(generation, query, cx));
        })
        .detach();
        cx.notify();
    }

    fn issue_search(&mut self, generation: u64, query: String, cx: &mut Context<Self>) {
        if generation != self.search.generation || !self.connected {
            // A newer keystroke owns the results.
            return;
        }
        let cwd = self.cwd.clone();
        let request = json!({
            "cwd": cwd,
            "query": query,
            "includeFiles": true,
            "includeDirectories": false,
            "limit": FILE_SEARCH_LIMIT,
        });
        match self
            .connection
            .request("directory_suggestions_request", request)
        {
            Ok(id) => {
                self.pending.insert(id, Pending::Search(generation));
            }
            Err(error) => {
                self.search.loading = false;
                self.search.error = Some(error.to_string());
                cx.notify();
            }
        }
    }

    /// The change request of the branch, once per status reply. Gated once by
    /// the host's `forgeProviders` capability.
    fn maybe_request_pr(&mut self, cx: &mut Context<Self>) {
        if !self.features.forge_providers {
            return;
        }
        if !self.status.as_ref().is_some_and(|status| status.is_git) {
            self.pr = None;
            return;
        }
        if self.pr_loading {
            return;
        }
        self.pr_loading = true;
        let cwd = self.cwd.clone();
        match self
            .connection
            .request("checkout_pr_status_request", json!({"cwd":cwd}))
        {
            Ok(id) => {
                self.pending.insert(id, Pending::PrStatus);
            }
            Err(error) => {
                self.pr_loading = false;
                self.pr_error = Some(error.to_string());
            }
        }
        cx.notify();
    }

    fn open_git_menu(
        &mut self,
        kind: GitMenuKind,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.git_menu = Some(GitMenu {
            kind,
            bounds: Bounds::new(position, size(px(0.), px(0.))),
        });
        if kind == GitMenuKind::Branch {
            self.request_branches(cx);
            self.request_stashes(cx);
        }
        cx.notify();
    }

    fn request_branches(&mut self, cx: &mut Context<Self>) {
        if !self.connected || self.branches_loading {
            return;
        }
        self.branches_loading = true;
        self.branches_error = None;
        let cwd = self.cwd.clone();
        let request = json!({"cwd":cwd,"limit":BRANCH_SUGGESTION_LIMIT});
        match self
            .connection
            .request("branch_suggestions_request", request)
        {
            Ok(id) => {
                self.pending.insert(id, Pending::Branches);
            }
            Err(error) => {
                self.branches_loading = false;
                self.branches_error = Some(error.to_string());
            }
        }
        cx.notify();
    }

    fn request_stashes(&mut self, cx: &mut Context<Self>) {
        if !self.connected {
            return;
        }
        let cwd = self.cwd.clone();
        let request = json!({"cwd":cwd,"paseoOnly":true});
        match self.connection.request("stash_list_request", request) {
            Ok(id) => {
                self.pending.insert(id, Pending::Stashes);
            }
            Err(error) => self.git_error = Some(error.to_string()),
        }
        cx.notify();
    }

    fn git_menu_action(&mut self, action: GitMenuAction, cx: &mut Context<Self>) {
        self.git_menu = None;
        match action {
            GitMenuAction::SwitchBranch(branch) => self.run_switch_branch(branch, cx),
            GitMenuAction::RestoreStash(index) => self.run_stash_pop(index, cx),
            GitMenuAction::Merge(method) => {
                let target = self
                    .pr
                    .as_ref()
                    .map(|pr| pr.base_ref.clone())
                    .filter(|base| !base.is_empty())
                    .unwrap_or_else(|| "the base branch".to_owned());
                self.confirm = Some(Confirm {
                    message: format!("{} into {target}?", method.label()),
                    action: ConfirmAction::MergePr(method),
                });
            }
            GitMenuAction::AutoMerge { enabled, method } => {
                self.run_auto_merge(enabled, method, cx)
            }
            GitMenuAction::OpenUrl(url) => {
                if !url.is_empty() {
                    cx.open_url(&url);
                }
            }
            GitMenuAction::CheckDetails {
                name,
                check_run_id,
                workflow_run_id,
            } => self.request_check_details(name, check_run_id, workflow_run_id, cx),
        }
        cx.notify();
    }

    /// Destructive and branch-changing actions land here only after the user
    /// confirmed them.
    fn confirm_action(&mut self, action: ConfirmAction, cx: &mut Context<Self>) {
        self.confirm = None;
        match action {
            ConfirmAction::Discard(path) => self.run_discard(path, cx),
            ConfirmAction::MergePr(method) => self.run_merge_pr(method, cx),
            ConfirmAction::StashAndSwitch(branch) => {
                // The switch is issued from the stash reply, never before it, and
                // only from the stash request this confirmation armed.
                if let Some(request_id) = self.run_stash_save(cx) {
                    self.stash_and_switch = Some(StashSwitch { branch, request_id });
                }
            }
        }
        cx.notify();
    }

    fn run_repo(
        &mut self,
        op: RepoOp,
        kind: &str,
        fields: Value,
        cx: &mut Context<Self>,
    ) -> Option<String> {
        if !self.connected {
            self.git_error = Some("Host is disconnected.".into());
            cx.notify();
            return None;
        }
        let row = op.row();
        if row_blocked(self.ops.get(&row).copied()) {
            // One request per action row. An unknown result is not permission
            // to send a second mutation whose outcome also cannot be known.
            return None;
        }
        match self.connection.request(kind, fields) {
            Ok(id) => {
                self.pending.insert(id.clone(), Pending::Git(op));
                self.ops.insert(row, OpState::Pending);
                self.active_ops.insert(row, id.clone());
                self.git_error = None;
                if matches!(row, GitOp::CreatePr | GitOp::MergePr | GitOp::AutoMerge) {
                    self.pr_error = None;
                }
                self.arm_timeout(row, id.clone(), cx);
                cx.notify();
                Some(id)
            }
            Err(error) => {
                self.git_error = Some(error.to_string());
                cx.notify();
                None
            }
        }
    }

    fn run_commit(&mut self, cx: &mut Context<Self>) {
        let cwd = self.cwd.clone();
        self.run_repo(
            RepoOp::Commit,
            "checkout_commit_request",
            json!({"cwd":cwd,"addAll":true}),
            cx,
        );
    }

    fn run_pull(&mut self, cx: &mut Context<Self>) {
        let cwd = self.cwd.clone();
        self.run_repo(
            RepoOp::Pull,
            "checkout_pull_request",
            json!({"cwd":cwd}),
            cx,
        );
    }

    fn run_push(&mut self, cx: &mut Context<Self>) {
        let cwd = self.cwd.clone();
        self.run_repo(
            RepoOp::Push,
            "checkout_push_request",
            json!({"cwd":cwd}),
            cx,
        );
    }

    /// Refresh asks the host to re-read remote refs, then reloads the dock.
    fn run_refresh(&mut self, cx: &mut Context<Self>) {
        let cwd = self.cwd.clone();
        self.run_repo(
            RepoOp::Refresh,
            "checkout.refresh.request",
            json!({"cwd":cwd}),
            cx,
        );
    }

    fn run_switch_branch(&mut self, branch: String, cx: &mut Context<Self>) {
        let cwd = self.cwd.clone();
        self.run_repo(
            RepoOp::SwitchBranch(branch.clone()),
            "checkout_switch_branch_request",
            json!({"cwd":cwd,"branch":branch}),
            cx,
        );
    }

    fn run_stash_save(&mut self, cx: &mut Context<Self>) -> Option<String> {
        let cwd = self.cwd.clone();
        let branch = self.status.as_ref().and_then(|s| s.current_branch.clone());
        let request = match branch {
            Some(branch) => json!({"cwd":cwd,"branch":branch}),
            None => json!({"cwd":cwd}),
        };
        self.run_repo(RepoOp::StashSave, "stash_save_request", request, cx)
    }

    fn run_stash_pop(&mut self, index: i64, cx: &mut Context<Self>) {
        let cwd = self.cwd.clone();
        self.run_repo(
            RepoOp::StashPop,
            "stash_pop_request",
            json!({"cwd":cwd,"stashIndex":index}),
            cx,
        );
    }

    /// Discard is destructive: it only runs from a confirmation.
    fn request_discard(&mut self, path: String, cx: &mut Context<Self>) {
        if !self.features.discard_changes {
            self.git_error = Some("Update the host to discard changes.".into());
            cx.notify();
            return;
        }
        self.confirm = Some(Confirm {
            message: format!("Discard changes to {path}? This cannot be undone."),
            action: ConfirmAction::Discard(path),
        });
        cx.notify();
    }

    /// Discard is destructive: it only runs from a confirmation.
    fn run_discard(&mut self, path: String, cx: &mut Context<Self>) {
        if !self.features.discard_changes {
            self.git_error = Some("Update the host to discard changes.".into());
            cx.notify();
            return;
        }
        let cwd = self.cwd.clone();
        self.run_repo(
            RepoOp::Discard(path.clone()),
            "checkout.discard_changes.request",
            json!({"cwd":cwd,"paths":[path]}),
            cx,
        );
    }

    fn run_create_pr(&mut self, cx: &mut Context<Self>) {
        if !self.features.forge_providers {
            self.pr_error = Some("Update the host to create a change request.".into());
            cx.notify();
            return;
        }
        let cwd = self.cwd.clone();
        self.run_repo(
            RepoOp::CreatePr,
            "checkout_pr_create_request",
            json!({"cwd":cwd}),
            cx,
        );
    }

    fn run_merge_pr(&mut self, method: MergeMethod, cx: &mut Context<Self>) {
        if !self.features.forge_providers {
            self.pr_error = Some("Update the host to merge a change request.".into());
            cx.notify();
            return;
        }
        let cwd = self.cwd.clone();
        self.run_repo(
            RepoOp::MergePr,
            "checkout_pr_merge_request",
            json!({"cwd":cwd,"mergeMethod":method.as_str()}),
            cx,
        );
    }

    fn run_auto_merge(
        &mut self,
        enabled: bool,
        method: Option<MergeMethod>,
        cx: &mut Context<Self>,
    ) {
        if !self.features.forge_set_auto_merge {
            self.pr_error = Some("Update the host to use auto-merge.".into());
            cx.notify();
            return;
        }
        let cwd = self.cwd.clone();
        let mut request = json!({"cwd":cwd,"enabled":enabled});
        if let Some(method) = method {
            request["mergeMethod"] = json!(method.as_str());
        }
        self.run_repo(
            RepoOp::AutoMerge,
            "checkout.forge.set_auto_merge.request",
            request,
            cx,
        );
    }

    fn request_check_details(
        &mut self,
        name: String,
        check_run_id: Option<i64>,
        workflow_run_id: Option<i64>,
        cx: &mut Context<Self>,
    ) {
        if !self.features.forge_check_details {
            self.git_error = Some("Update the host to show check details.".into());
            cx.notify();
            return;
        }
        if check_run_id.is_none() && workflow_run_id.is_none() {
            self.git_error = Some("This check has no run id the host can fetch.".into());
            cx.notify();
            return;
        }
        let cwd = self.cwd.clone();
        let mut fields = json!({"cwd":cwd});
        if let Some(id) = check_run_id {
            fields["checkRunId"] = json!(id);
        }
        if let Some(id) = workflow_run_id {
            fields["workflowRunId"] = json!(id);
        }
        if let Some(pr) = &self.pr {
            if let Some(owner) = pr.repo_owner.as_deref().filter(|owner| !owner.is_empty()) {
                fields["repoOwner"] = json!(owner);
            }
            if let Some(repo) = pr.repo_name.as_deref().filter(|repo| !repo.is_empty()) {
                fields["repoName"] = json!(repo);
            }
            if let Some(number) = pr.number {
                fields["changeRequestNumber"] = json!(number);
            }
        }
        match self
            .connection
            .request("checkout.forge.get_check_details.request", fields)
        {
            Ok(id) => {
                self.pending.insert(id.clone(), Pending::CheckDetails);
                self.ops.insert(GitOp::CheckDetails, OpState::Pending);
                self.active_ops.insert(GitOp::CheckDetails, id.clone());
                self.check_details_error = None;
                self.check_details = Some(CheckDetails {
                    name,
                    ..Default::default()
                });
                self.arm_timeout(GitOp::CheckDetails, id, cx);
            }
            Err(error) => self.check_details_error = Some(error.to_string()),
        }
        cx.notify();
    }

    /// A reply settles only the row it names, and only while that row is still
    /// waiting on this request. A late reply for an older request leaves a
    /// newer request's row and its stash continuation alone.
    fn settle_git(
        &mut self,
        op: RepoOp,
        request_id: &str,
        payload: &Value,
        error: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let row = op.row();
        if !settles_row(self.active_ops.get(&row).map(String::as_str), request_id) {
            return;
        }
        self.active_ops.remove(&row);
        let failure = error
            .or_else(|| checkout_error_message(payload))
            .or_else(|| {
                (payload["success"] == Value::Bool(false))
                    .then(|| "The host refused the git action.".to_owned())
            });
        if let Some(message) = failure {
            self.ops.remove(&row);
            if matches!(op, RepoOp::StashSave)
                && self
                    .stash_and_switch
                    .as_ref()
                    .is_some_and(|armed| armed.request_id == request_id)
            {
                // The stash this confirmation armed failed: never switch.
                self.stash_and_switch = None;
            }
            let switching = match &op {
                RepoOp::SwitchBranch(branch) => Some(branch.clone()),
                _ => None,
            };
            if let (GitOp::SwitchBranch, Some(branch)) = (row, switching)
                && should_offer_stash_switch(&message)
            {
                // Instead of a dead end, offer the original app's stash and switch.
                self.confirm = Some(Confirm {
                    message: format!("Switch to {branch} and stash your changes?"),
                    action: ConfirmAction::StashAndSwitch(branch),
                });
            } else {
                self.git_error = Some(message);
            }
            cx.notify();
            return;
        }
        self.ops.insert(row, OpState::Success);
        self.git_error = None;
        self.flash_success(row, cx);
        match op {
            RepoOp::StashSave => {
                if let Some(armed) = self.stash_and_switch.take()
                    && armed.request_id == request_id
                {
                    self.run_switch_branch(armed.branch, cx);
                    return;
                }
            }
            RepoOp::SwitchBranch(_) => {
                self.git_menu = None;
                self.branches.clear();
                self.stashes.clear();
            }
            RepoOp::Discard(path) => {
                if self.selected_diff.as_deref() == Some(path.as_str()) {
                    self.selected_diff = None;
                }
            }
            _ => {}
        }
        self.refresh(cx);
    }

    fn flash_success(&mut self, op: GitOp, cx: &mut Context<Self>) {
        self.flash += 1;
        let generation = self.flash;
        let timer = cx.background_executor().timer(SUCCESS_DISPLAY);
        cx.spawn(async move |this, cx| {
            timer.await;
            let _ = this.update(cx, |this, cx| {
                if this.flash == generation && this.ops.get(&op) == Some(&OpState::Success) {
                    this.ops.remove(&op);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// An unanswered action would keep its row pending forever; time out into an
    /// unknown result instead. Nothing is re-sent, and the row stays blocked
    /// until the host's reply or a reconnect resolves it.
    fn arm_timeout(&mut self, row: GitOp, request_id: String, cx: &mut Context<Self>) {
        let timer = cx.background_executor().timer(ACTION_TIMEOUT);
        cx.spawn(async move |this, cx| {
            timer.await;
            let _ = this.update(cx, |this, cx| {
                if !settles_row(
                    this.active_ops.get(&row).map(String::as_str),
                    &request_id,
                ) {
                    // Answered, or superseded by a newer request on this row.
                    return;
                }
                this.ops.insert(row, OpState::Unknown);
                if row == GitOp::StashSave
                    && this
                        .stash_and_switch
                        .as_ref()
                        .is_some_and(|armed| armed.request_id == request_id)
                {
                    // An unresolved stash must not trigger its switch later.
                    this.stash_and_switch = None;
                }
                this.git_error = Some(
                    "The host did not answer the action; its result is unknown. It will settle when the host replies or reconnects."
                        .into(),
                );
                cx.notify();
            });
        })
        .detach();
    }
}

fn parent_of(path: &str) -> String {
    match path.rfind('/') {
        Some(index) => path[..index].to_owned(),
        None => ".".to_owned(),
    }
}

/// A reply settles a row only while that row is still waiting on this request.
/// A late reply for an older request must not clear or complete a newer one.
fn settles_row(active: Option<&str>, request_id: &str) -> bool {
    active == Some(request_id)
}

/// An in-flight or unresolved action blocks another request on the same row.
/// An unknown result is explicitly included: the daemon may have applied it.
fn row_blocked(state: Option<OpState>) -> bool {
    matches!(state, Some(OpState::Pending | OpState::Unknown))
}

/// Whether this tools surface answers a host message: one of its own pushes, or
/// a reply whose request id it is still pending on. Ownership is the pending
/// map, not a list of kinds, so a new RPC needs no dispatch edit.
fn owns_message(
    kind: &str,
    payload: &Value,
    workspace_id: &str,
    cwd: &str,
    pending: &BTreeMap<String, Pending>,
) -> bool {
    match kind {
        "script_status_update" => payload["workspaceId"].as_str() == Some(workspace_id),
        "checkout_status_update" => payload["cwd"].as_str() == Some(cwd),
        "checkout_diff_update" => true,
        _ => payload["requestId"]
            .as_str()
            .is_some_and(|id| pending.contains_key(id)),
    }
}

/// The host part of a URL, without scheme, userinfo or port.
fn url_host(url: &str) -> Option<String> {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    if authority.is_empty() {
        return None;
    }
    let host = if let Some(rest) = authority.strip_prefix('[') {
        rest.split(']').next().unwrap_or(rest)
    } else {
        authority.split(':').next().unwrap_or(authority)
    };
    (!host.is_empty()).then(|| host.to_owned())
}

fn is_loopback_host(host: &str) -> bool {
    matches!(
        host.trim().to_lowercase().as_str(),
        "localhost" | "127.0.0.1" | "::1"
    )
}

fn is_local_only_url(url: &str) -> bool {
    url_host(url).is_none_or(|host| {
        let host = host.to_lowercase();
        is_loopback_host(&host) || host.ends_with(".localhost")
    })
}

fn strip_protocol(url: &str) -> String {
    url.split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(url)
        .trim_end_matches('/')
        .to_owned()
}

/// The URL a service is reachable at straight from this daemon host.
fn direct_service_url(host: &str, port: Option<i64>) -> Option<String> {
    let port = port?;
    let host = if is_loopback_host(host) {
        "localhost".to_owned()
    } else if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    Some(format!("http://{host}:{port}"))
}

/// The service URLs in the original app's order: public, Paseo, then direct.
/// Only a running service has a URL; a stopped one has nothing to open.
///
/// Paseo's proxy URLs resolve on the daemon's machine through the `Host` header,
/// so a connection to another machine must not offer them: the browser would
/// ask the client's own loopback instead. Public and direct URLs stay.
pub fn script_targets(script: &Script, host: &str, local_endpoint: bool) -> Vec<ScriptTarget> {
    if !script.is_service || script.lifecycle != "running" {
        return Vec::new();
    }
    // COMPAT(workspaceScriptSplitUrls): daemons predating the split only send
    // proxyUrl, so classify it by reachability instead of assuming either side.
    let paseo = script.local_url.clone().or_else(|| {
        script
            .proxy_url
            .clone()
            .filter(|url| is_local_only_url(url))
    });
    let public = script.public_url.clone().or_else(|| {
        script
            .proxy_url
            .clone()
            .filter(|url| !is_local_only_url(url))
    });
    let mut targets: Vec<ScriptTarget> = Vec::new();
    let mut add = |kind: ScriptLinkKind, url: Option<String>| {
        let Some(url) = url else { return };
        if targets.iter().any(|target| target.url == url) {
            return;
        }
        targets.push(ScriptTarget {
            kind,
            label: strip_protocol(&url),
            url,
        });
    };
    add(ScriptLinkKind::Public, public);
    add(ScriptLinkKind::Paseo, paseo.filter(|_| local_endpoint));
    add(
        ScriptLinkKind::Direct,
        direct_service_url(host, script.port),
    );
    targets
}

/// The exact URL a menu action carries. Copying and opening take it verbatim;
/// neither re-derives a target from a script name.
fn action_url(payload: &str) -> Option<&str> {
    (!payload.is_empty()).then_some(payload)
}

/// `entries` carries the directory suggestions; `directories` is the legacy list.
fn parse_search_hits(payload: &Value) -> Vec<SearchHit> {
    if let Some(entries) = payload["entries"].as_array()
        && !entries.is_empty()
    {
        return entries
            .iter()
            .filter_map(|entry| entry["path"].as_str().map(describe_path))
            .collect();
    }
    payload["directories"]
        .as_array()
        .map(|paths| {
            paths
                .iter()
                .filter_map(|path| path.as_str().map(describe_path))
                .collect()
        })
        .unwrap_or_default()
}

fn describe_path(path: &str) -> SearchHit {
    let normalized = path.replace('\\', "/");
    let (directory, name) = match normalized.rsplit_once('/') {
        Some((directory, name)) => (directory.to_owned(), name.to_owned()),
        None => (String::new(), normalized.clone()),
    };
    SearchHit {
        path: normalized,
        name,
        directory,
    }
}

fn parse_stashes(payload: &Value) -> Vec<StashEntry> {
    payload["entries"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| {
                    Some(StashEntry {
                        index: entry["index"].as_i64()?,
                        branch: entry["branch"].as_str().map(str::to_owned),
                        message: entry["message"].as_str().unwrap_or_default().to_owned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn parse_checks(status: &Value) -> Vec<PrCheck> {
    status["checks"]
        .as_array()
        .map(|checks| {
            checks
                .iter()
                .filter_map(|check| {
                    Some(PrCheck {
                        name: check["name"].as_str()?.to_owned(),
                        status: check["status"].as_str().unwrap_or_default().to_owned(),
                        url: check["url"].as_str().map(str::to_owned),
                        check_run_id: check["checkRunId"].as_i64(),
                        workflow_run_id: check["workflowRunId"].as_i64(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Which actions a check row offers. Details needs the host capability and a run
/// id; the browser stays a separate action so a check with both keeps both.
#[derive(Clone, Copy, Debug, PartialEq)]
enum CheckActions {
    Details,
    OpenUrl,
    Both,
    Neither,
}

fn check_actions(check: &PrCheck, details_enabled: bool) -> CheckActions {
    let has_run_id = check.check_run_id.is_some() || check.workflow_run_id.is_some();
    let has_url = check.url.as_deref().is_some_and(|url| !url.is_empty());
    match (has_run_id && details_enabled, has_url) {
        (true, true) => CheckActions::Both,
        (true, false) => CheckActions::Details,
        (false, true) => CheckActions::OpenUrl,
        (false, false) => CheckActions::Neither,
    }
}

/// The forge's own readiness facts, in the family that produced them. The
/// GitHub `status.github` object is a published mirror, not a universal one:
/// it is read only when the facts envelope is absent.
struct ForgeFacts {
    direct_merge_blocker: Option<&'static str>,
    merge_methods: Vec<MergeMethod>,
    auto_merge_enabled: bool,
    auto_merge_enable_blocker: Option<&'static str>,
    can_disable_auto_merge: bool,
}

impl ForgeFacts {
    /// No usable facts: withhold every forge action instead of assuming one.
    fn withheld(reason: &'static str) -> Self {
        Self {
            direct_merge_blocker: Some(reason),
            merge_methods: Vec::new(),
            auto_merge_enabled: false,
            auto_merge_enable_blocker: Some(reason),
            can_disable_auto_merge: false,
        }
    }
}

/// The forge's repository policy decides which merge methods exist. When the
/// host supplies no policy facts at all, fall back to the git state with every
/// method instead of inventing a narrower set.
fn merge_methods_for(github: &Value) -> Vec<MergeMethod> {
    let repository = &github["repository"];
    if repository.is_null() {
        return vec![MergeMethod::Squash, MergeMethod::Merge, MergeMethod::Rebase];
    }
    let allowed = |name: &str| repository[name] == Value::Bool(true);
    let mut methods = Vec::new();
    if allowed("squashMergeAllowed") {
        methods.push(MergeMethod::Squash);
    }
    if allowed("mergeCommitAllowed") {
        methods.push(MergeMethod::Merge);
    }
    if allowed("rebaseMergeAllowed") {
        methods.push(MergeMethod::Rebase);
    }
    methods
}

fn forge_facts(status: &Value) -> ForgeFacts {
    let envelope = &status["forgeSpecific"];
    if let Some(family) = envelope["forge"].as_str() {
        return match family {
            "github" => github_facts(envelope, status),
            "gitlab" => gitlab_facts(envelope),
            "gitea" => gitea_facts(envelope),
            // A family this client has no rules for: offer nothing rather than
            // borrow another forge's readiness answers.
            _ => ForgeFacts::withheld(
                "This client does not understand this forge's readiness facts.",
            ),
        };
    }
    if !status["github"].is_null() {
        return github_facts(&status["github"], status);
    }
    ForgeFacts::withheld("The forge has not reported readiness for this change request.")
}

/// Mirrors `assertDirectPullRequestMergeReady` and the auto-merge guards in the
/// daemon's GitHub adapter: merge queue, a scheduled auto-merge, and a merge
/// state outside its direct-merge allowlist all withhold a direct merge.
fn github_facts(github: &Value, status: &Value) -> ForgeFacts {
    let repository = &github["repository"];
    let merge_state = github["mergeStateStatus"].as_str().unwrap_or_default();
    let merge_queue = github["isMergeQueueEnabled"] == Value::Bool(true)
        || github["isInMergeQueue"] == Value::Bool(true);
    let auto_merge_enabled = !github["autoMergeRequest"].is_null();
    let conflicts = status["mergeable"].as_str() == Some("CONFLICTING");
    let direct_ready = matches!(merge_state, "CLEAN" | "HAS_HOOKS");
    let direct_merge_blocker = if merge_queue {
        Some("This repository merges through a merge queue.")
    } else if auto_merge_enabled {
        Some("Auto-merge is already enabled for this change request.")
    } else if conflicts {
        Some("The change request has conflicts.")
    } else if !direct_ready {
        Some("The forge does not report this change request as ready for a direct merge.")
    } else {
        None
    };
    let can_enable_auto_merge = !auto_merge_enabled
        && !merge_queue
        && !conflicts
        && merge_state == "BLOCKED"
        && github["viewerCanEnableAutoMerge"] == Value::Bool(true)
        && repository["autoMergeAllowed"] == Value::Bool(true);
    let auto_merge_enable_blocker = if auto_merge_enabled || can_enable_auto_merge {
        None
    } else if merge_queue {
        Some("Auto-merge is not available while the repository uses a merge queue.")
    } else if conflicts {
        Some("Auto-merge is not available because the change request has conflicts.")
    } else if merge_state != "BLOCKED" {
        Some("Auto-merge is offered once the forge reports the change request as blocked.")
    } else if github["viewerCanEnableAutoMerge"] != Value::Bool(true) {
        Some("This account cannot enable auto-merge.")
    } else {
        Some("Auto-merge is disabled for this repository.")
    };
    ForgeFacts {
        direct_merge_blocker,
        merge_methods: merge_methods_for(github),
        auto_merge_enabled,
        auto_merge_enable_blocker,
        can_disable_auto_merge: auto_merge_enabled
            && !merge_queue
            && github["viewerCanDisableAutoMerge"] == Value::Bool(true),
    }
}

/// Mirrors the GitLab adapter's `assertGitLabDirectMergeReady` and
/// `assertGitLabAutoMergeEnableReady`. Every merge method is accepted as a flag.
fn gitlab_facts(facts: &Value) -> ForgeFacts {
    let scheduled = facts["mergeWhenPipelineSucceeds"] == Value::Bool(true);
    let conflicts = facts["hasConflicts"] == Value::Bool(true);
    let ready = match facts["detailedMergeStatus"].as_str() {
        Some(status) => status == "mergeable",
        // Older self-managed GitLab only reports the legacy merge status.
        None => facts["mergeStatus"].as_str() == Some("can_be_merged") && !conflicts,
    };
    let pipeline_active = facts["pipelineStatus"].as_str().is_some_and(|status| {
        matches!(
            status,
            "created"
                | "waiting_for_resource"
                | "preparing"
                | "pending"
                | "running"
                | "canceling"
                | "scheduled"
        )
    });
    let direct_merge_blocker = if scheduled {
        Some("Auto-merge is already scheduled for this change request.")
    } else if !ready {
        Some("The forge does not report this change request as ready for a direct merge.")
    } else {
        None
    };
    let auto_merge_enable_blocker = if scheduled || pipeline_active {
        None
    } else {
        Some("Auto-merge needs an in-progress pipeline; without one GitLab merges immediately.")
    };
    ForgeFacts {
        direct_merge_blocker,
        merge_methods: vec![MergeMethod::Squash, MergeMethod::Merge, MergeMethod::Rebase],
        auto_merge_enabled: scheduled,
        auto_merge_enable_blocker,
        can_disable_auto_merge: scheduled,
    }
}

/// Gitea reports one mergeable fact and has no auto-merge at all; the daemon's
/// `enablePullRequestAutoMerge` is not supported for it.
fn gitea_facts(facts: &Value) -> ForgeFacts {
    let ready = facts["mergeable"] == Value::Bool(true) && facts["hasMerged"] != Value::Bool(true);
    ForgeFacts {
        direct_merge_blocker: (!ready).then_some(
            "The forge does not report this change request as ready for a direct merge.",
        ),
        merge_methods: vec![MergeMethod::Squash, MergeMethod::Merge, MergeMethod::Rebase],
        auto_merge_enabled: false,
        auto_merge_enable_blocker: Some("This forge does not support auto-merge."),
        can_disable_auto_merge: false,
    }
}

fn parse_pr_status(payload: &Value) -> Option<PrStatus> {
    let status = &payload["status"];
    if status.is_null() {
        return None;
    }
    let facts = forge_facts(status);
    Some(PrStatus {
        forge: status["forge"]
            .as_str()
            .or_else(|| payload["forge"].as_str())
            .unwrap_or("github")
            .to_owned(),
        number: status["number"].as_i64(),
        url: status["url"].as_str().unwrap_or_default().to_owned(),
        title: status["title"].as_str().unwrap_or_default().to_owned(),
        state: status["state"].as_str().unwrap_or_default().to_owned(),
        is_draft: status["isDraft"] == Value::Bool(true),
        is_merged: status["isMerged"] == Value::Bool(true),
        mergeable: status["mergeable"].as_str().unwrap_or("UNKNOWN").to_owned(),
        checks: parse_checks(status),
        checks_status: status["checksStatus"].as_str().map(str::to_owned),
        base_ref: status["baseRefName"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        repo_owner: status["repoOwner"].as_str().map(str::to_owned),
        repo_name: status["repoName"].as_str().map(str::to_owned),
        merge_methods: facts.merge_methods,
        direct_merge_blocker: facts.direct_merge_blocker,
        auto_merge_enabled: facts.auto_merge_enabled,
        auto_merge_enable_blocker: facts.auto_merge_enable_blocker,
        can_disable_auto_merge: facts.can_disable_auto_merge,
    })
}

fn parse_check_details(payload: &Value) -> Option<CheckDetails> {
    let details = &payload["details"];
    if details.is_null() {
        return None;
    }
    let failed_jobs = details["failedJobs"]
        .as_array()
        .map(|jobs| {
            jobs.iter()
                .filter_map(|job| job["name"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    Some(CheckDetails {
        name: details["name"].as_str().unwrap_or_default().to_owned(),
        conclusion: details["conclusion"].as_str().map(str::to_owned),
        url: details["detailsUrl"]
            .as_str()
            .or_else(|| details["url"].as_str())
            .map(str::to_owned),
        failed_jobs,
    })
}

/// The original branch switcher offers "stash and switch" when the host refuses
/// a checkout for uncommitted changes.
fn should_offer_stash_switch(message: &str) -> bool {
    message.to_lowercase().contains("uncommitted")
}

fn parse_script_option(script: &Value) -> Option<Script> {
    parse_script(script)
}

/// Checkout errors travel as `{code,message}` objects; plain surfaces use strings.
pub(crate) fn checkout_error_message(payload: &Value) -> Option<String> {
    let error = &payload["error"];
    if error.is_null() {
        return None;
    }
    if let Some(message) = error["message"].as_str() {
        return Some(message.to_owned());
    }
    error.as_str().map(str::to_owned)
}

/// A restart may only fire once the stop is acknowledged, either by the stop
/// reply or by the daemon reporting the script stopped while no stop reply is
/// still outstanding for it.
fn should_start_restart(restart: Option<&str>, name: &str, pending: Option<&str>) -> bool {
    restart == Some(name) && pending.is_none_or(|pending| pending != name)
}

/// A script reply only clears the marker of the operation it answers. A late
/// reply for another script leaves a newer operation's marker in place, so the
/// next action still sees that something is in flight.
fn clear_script_pending(pending: &mut Option<String>, answered: &str) {
    if pending.as_deref() == Some(answered) {
        *pending = None;
    }
}

/// Scripts from a raw workspace descriptor payload.
pub fn parse_scripts(payload: &Value) -> Vec<Script> {
    payload["scripts"]
        .as_array()
        .map(|scripts| scripts.iter().filter_map(parse_script).collect())
        .unwrap_or_default()
}

fn parse_script(script: &Value) -> Option<Script> {
    Some(Script {
        name: script["scriptName"].as_str()?.to_owned(),
        is_service: script["type"].as_str().unwrap_or("service") == "service",
        lifecycle: script["lifecycle"].as_str().unwrap_or("stopped").to_owned(),
        health: script["health"].as_str().map(str::to_owned),
        exit_code: script["exitCode"].as_i64(),
        terminal_id: script["terminalId"].as_str().map(str::to_owned),
        local_url: script["localProxyUrl"].as_str().map(str::to_owned),
        public_url: script["publicProxyUrl"].as_str().map(str::to_owned),
        proxy_url: script["proxyUrl"].as_str().map(str::to_owned),
        port: script["port"].as_i64(),
    })
}

fn parse_status(payload: &Value) -> CheckoutStatus {
    CheckoutStatus {
        is_git: payload["isGit"] == Value::Bool(true),
        repo_root: payload["repoRoot"].as_str().map(str::to_owned),
        current_branch: payload["currentBranch"].as_str().map(str::to_owned),
        is_dirty: payload["isDirty"].as_bool(),
        ahead: payload["aheadOfOrigin"].as_i64(),
        behind: payload["behindOfOrigin"].as_i64(),
    }
}

fn parse_directory(directory: &Value) -> Directory {
    Directory {
        entries: directory["entries"]
            .as_array()
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|entry| {
                        Some(Entry {
                            name: entry["name"].as_str()?.to_owned(),
                            path: entry["path"].as_str()?.to_owned(),
                            kind: match entry["kind"].as_str() {
                                Some("directory") => EntryKind::Directory,
                                _ => EntryKind::File,
                            },
                            size: entry["size"].as_u64().unwrap_or(0),
                            modified_at: entry["modifiedAt"]
                                .as_str()
                                .unwrap_or_default()
                                .to_owned(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}

fn parse_diff_file(file: &Value) -> Option<DiffFile> {
    Some(DiffFile {
        path: file["path"].as_str()?.to_owned(),
        is_new: file["isNew"] == Value::Bool(true),
        is_deleted: file["isDeleted"] == Value::Bool(true),
        additions: file["additions"].as_u64().unwrap_or(0),
        deletions: file["deletions"].as_u64().unwrap_or(0),
        binary: file["status"].as_str() == Some("binary"),
        too_large: file["status"].as_str() == Some("too_large"),
        hunks: file["hunks"]
            .as_array()
            .map(|hunks| {
                hunks
                    .iter()
                    .map(|hunk| Hunk {
                        header: format!(
                            "@@ -{},{} +{},{} @@",
                            hunk["oldStart"].as_i64().unwrap_or(0),
                            hunk["oldCount"].as_i64().unwrap_or(0),
                            hunk["newStart"].as_i64().unwrap_or(0),
                            hunk["newCount"].as_i64().unwrap_or(0),
                        ),
                        lines: hunk["lines"]
                            .as_array()
                            .map(|lines| {
                                lines
                                    .iter()
                                    .filter_map(|line| {
                                        let kind = match line["type"].as_str()? {
                                            "add" => LineKind::Add,
                                            "remove" => LineKind::Remove,
                                            "header" => return None,
                                            _ => LineKind::Context,
                                        };
                                        Some((kind, line["content"].as_str()?.to_owned()))
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                    })
                    .collect()
            })
            .unwrap_or_default(),
    })
}

fn badge(file: &DiffFile) -> &'static str {
    if file.is_new {
        "A"
    } else if file.is_deleted {
        "D"
    } else {
        "M"
    }
}

impl Focusable for WorkspaceTools {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.input
            .as_ref()
            .map(|input| input.focus.clone())
            .unwrap_or_else(|| self.focus.clone())
    }
}

impl WorkspaceTools {
    fn replace_input(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        marked: bool,
        selected: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(input) = &mut self.input {
            input
                .query
                .replace(range, &text.replace(['\r', '\n'], " "), marked, selected);
            window.notify_text_input_changed(&input.focus);
        }
        // Search only once the text is committed, never mid-composition.
        if !marked
            && self
                .input
                .as_ref()
                .is_some_and(|input| input.kind == InputKind::Search)
        {
            self.schedule_search(cx);
        }
        cx.notify();
    }
}

impl EntityInputHandler for WorkspaceTools {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let input = self.input.as_ref()?;
        let range = input.query.byte_range(range);
        *actual = Some(input.query.utf16_range(range.clone()));
        input.query.text.get(range).map(str::to_owned)
    }
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let input = self.input.as_ref()?;
        Some(UTF16Selection {
            range: input.query.utf16_range(input.query.selection.clone()),
            reversed: input.query.reversed,
        })
    }
    fn set_selected_text_range(
        &mut self,
        range: Range<usize>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
        if let Some(input) = &mut self.input {
            input.query.selection = input.query.byte_range(range);
            input.query.reversed = false;
        }
    }
    fn text_length_utf16(&mut self, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        Some(self.input.as_ref()?.query.text.encode_utf16().count())
    }
    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        let input = self.input.as_ref()?;
        input
            .query
            .marked
            .clone()
            .map(|r| input.query.utf16_range(r))
    }
    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        if let Some(input) = &mut self.input {
            input.query.marked = None;
        }
    }
    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.replace_input(range, text, false, None, window, cx);
    }
    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.replace_input(range, text, true, selected, window, cx);
    }
    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let input = self.input.as_ref()?;
        let byte = input.query.byte_range(range).start;
        Some(Bounds::new(
            input.bounds.origin
                + point(
                    input
                        .line
                        .as_ref()
                        .map_or(px(0.), |line| line.x_for_index(byte))
                        - input.scroll_x,
                    px(0.),
                ),
            size(px(1.), px(22.)),
        ))
    }
    fn character_index_for_point(
        &mut self,
        position: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let input = self.input.as_ref()?;
        Some(
            input.query.utf16_offset(
                input
                    .line
                    .as_ref()?
                    .closest_index_for_x(position.x - input.bounds.left() + input.scroll_x),
            ),
        )
    }
}

impl Render for WorkspaceTools {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = active_palette(&self.shared, window);
        let content = match self.tab {
            DockTab::Files => self.render_files(&palette, window, cx),
            DockTab::Changes => self.render_changes(&palette, cx),
        };
        fn dock_tab(
            label: &'static str,
            tab: DockTab,
            selected: bool,
            palette: &Palette,
            cx: &mut Context<WorkspaceTools>,
        ) -> gpui::Stateful<gpui::Div> {
            let this_tab = tab;
            div()
                .id(gpui::SharedString::from(format!("dock-tab-{label}")))
                .h(px(28.))
                .px_2()
                .rounded(px(radius::MD))
                .flex()
                .items_center()
                .cursor_pointer()
                .text_size(px(font_size::BASE))
                .min_w_0()
                .flex_shrink(1.)
                .gap_1()
                .text_color(rgb(if selected {
                    palette.foreground
                } else {
                    palette.foreground_muted
                }))
                .when(selected, |tab| tab.bg(rgb(palette.surface2)))
                .hover(|tab| tab.bg(rgb(palette.surface2)))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.focus.focus(window, cx);
                        this.toggle_tab(this_tab, cx);
                    }),
                )
                .child(
                    icon(
                        if this_tab == DockTab::Files {
                            "files"
                        } else {
                            "file-diff"
                        },
                        14.,
                    )
                    .flex_shrink_0(),
                )
                .child(div().min_w_0().truncate().child(label))
        }
        let files_toolbar = self.render_files_toolbar(&palette, cx);
        div()
            .id("workspace-tools")
            .track_focus(&self.focus)
            .w_full()
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .bg(rgb(palette.surface_sidebar))
            .text_color(rgb(palette.foreground))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _, _, cx| {
                    if this.menu.take().is_some() {
                        cx.notify();
                    }
                }),
            )
            .child(
                div()
                    .h(px(36.))
                    .flex_shrink_0()
                    .pl_2()
                    .pr(px(
                        if self.owns_window_corner
                            && !cfg!(target_os = "macos")
                            && !window.is_fullscreen()
                        {
                            0.
                        } else {
                            8.
                        },
                    ))
                    .flex()
                    .items_center()
                    .gap_1()
                    .border_b_1()
                    .border_color(rgb(palette.border))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(dock_tab(
                                "Files",
                                DockTab::Files,
                                self.tab == DockTab::Files,
                                &palette,
                                cx,
                            ))
                            .child(dock_tab(
                                "Changes",
                                DockTab::Changes,
                                self.tab == DockTab::Changes,
                                &palette,
                                cx,
                            )),
                    )
                    .when(
                        self.owns_window_corner
                            && !cfg!(target_os = "macos")
                            && !window.is_fullscreen(),
                        |rail| rail.child(crate::window_chrome::controls(window)),
                    ),
            )
            .when(self.tab == DockTab::Files, |dock| dock.child(files_toolbar))
            .child(content)
            .when_some(self.confirm.clone(), |dock, confirm| {
                dock.child(
                    div()
                        .flex_shrink_0()
                        .px_2()
                        .py_1()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .border_t_1()
                        .border_color(rgb(palette.border))
                        .bg(rgb(palette.surface1))
                        .text_size(px(font_size::SM))
                        .child(
                            div()
                                .text_color(rgb(palette.foreground))
                                .child(confirm.message.clone()),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_end()
                                .gap_2()
                                .child(
                                    div()
                                        .id("confirm-cancel")
                                        .h(px(24.))
                                        .px_2()
                                        .rounded(px(radius::SM))
                                        .flex()
                                        .items_center()
                                        .cursor_pointer()
                                        .text_color(rgb(palette.foreground_muted))
                                        .hover(|button| button.bg(rgb(palette.surface3)))
                                        .child("Cancel")
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.confirm = None;
                                                cx.notify();
                                            }),
                                        ),
                                )
                                .child(
                                    div()
                                        .id("confirm-accept")
                                        .h(px(24.))
                                        .px_2()
                                        .rounded(px(radius::SM))
                                        .flex()
                                        .items_center()
                                        .cursor_pointer()
                                        .bg(rgb(palette.destructive))
                                        .text_color(rgb(palette.destructive_foreground))
                                        .child("Confirm")
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _, _, cx| {
                                                cx.stop_propagation();
                                                if let Some(confirm) = this.confirm.clone() {
                                                    this.confirm_action(confirm.action, cx);
                                                }
                                            }),
                                        ),
                                ),
                        ),
                )
            })
            .when_some(self.entry_error.clone(), |dock, error| {
                dock.child(
                    div()
                        .flex_shrink_0()
                        .px_2()
                        .py_1()
                        .flex()
                        .items_center()
                        .gap_2()
                        .border_t_1()
                        .border_color(rgb(palette.border))
                        .text_size(px(font_size::SM))
                        .text_color(rgb(palette.status_danger))
                        .child(div().flex_1().truncate().child(error))
                        .child(
                            div()
                                .id("dismiss-entry-error")
                                .flex_shrink_0()
                                .cursor_pointer()
                                .text_color(rgb(palette.foreground_muted))
                                .child(icon("x", 12.).text_color(rgb(palette.foreground_muted)))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.entry_error = None;
                                        cx.notify();
                                    }),
                                ),
                        ),
                )
            })
            .when_some(self.git_menu.clone(), |dock, menu| {
                dock.child(
                    gpui::deferred(
                        gpui::anchored()
                            .position(point(menu.bounds.left(), menu.bounds.bottom() + px(4.)))
                            .anchor(gpui::Anchor::TopLeft)
                            .snap_to_window_with_margin(px(8.))
                            .child(self.render_git_menu(&menu, &palette, cx)),
                    )
                    .with_priority(2),
                )
            })
            .when_some(self.menu.clone(), |dock, menu| {
                dock.child(
                    gpui::deferred(
                        gpui::anchored()
                            .position(point(menu.bounds.left(), menu.bounds.bottom() + px(2.)))
                            .anchor(gpui::Anchor::TopLeft)
                            .snap_to_window_with_margin(px(8.))
                            .child(
                                div()
                                    .id("explorer-context-menu")
                                    .w(px(180.))
                                    .rounded(px(radius::LG))
                                    .border_1()
                                    .border_color(rgb(palette.border))
                                    .bg(rgb(palette.popover))
                                    .py_1()
                                    .flex()
                                    .flex_col()
                                    .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                                        if this.menu.take().is_some() {
                                            cx.notify();
                                        }
                                    }))
                                    .children(menu.actions.iter().map(|action| {
                                        let label = action.label();
                                        let action = action.clone_action();
                                        let danger = matches!(action, MenuAction::Delete { .. });
                                        div()
                                            .id(gpui::SharedString::from(format!("menu-{label}")))
                                            .min_h(px(28.))
                                            .px_3()
                                            .py_1()
                                            .flex()
                                            .items_center()
                                            .cursor_pointer()
                                            .text_size(px(font_size::SM))
                                            .text_color(rgb(if danger {
                                                palette.status_danger
                                            } else {
                                                palette.popover_foreground
                                            }))
                                            .hover(|item| item.bg(rgb(palette.surface2)))
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(move |this, _, window, cx| {
                                                    cx.stop_propagation();
                                                    this.menu = None;
                                                    match &action {
                                                        MenuAction::NewFile(parent) => {
                                                            this.begin_create(
                                                                parent.clone(),
                                                                false,
                                                                window,
                                                                cx,
                                                            );
                                                        }
                                                        MenuAction::NewFolder(parent) => {
                                                            this.begin_create(
                                                                parent.clone(),
                                                                true,
                                                                window,
                                                                cx,
                                                            );
                                                        }
                                                        MenuAction::Rename(path) => {
                                                            this.begin_rename(
                                                                path.clone(),
                                                                window,
                                                                cx,
                                                            );
                                                        }
                                                        MenuAction::Duplicate { path, parent } => {
                                                            this.duplicate(
                                                                path.clone(),
                                                                parent.clone(),
                                                                cx,
                                                            );
                                                        }
                                                        MenuAction::Delete { path, parent } => {
                                                            this.delete(
                                                                path.clone(),
                                                                parent.clone(),
                                                                cx,
                                                            );
                                                        }
                                                        MenuAction::CopyPath { path, absolute } => {
                                                            this.copy_path(
                                                                path.clone(),
                                                                *absolute,
                                                                cx,
                                                            );
                                                        }
                                                        MenuAction::Collapse(path) => {
                                                            this.expanded.remove(path);
                                                            cx.notify();
                                                        }
                                                    }
                                                    cx.notify();
                                                }),
                                            )
                                            .child(label)
                                    })),
                            ),
                    )
                    .with_priority(2),
                )
            })
    }
}

impl WorkspaceTools {
    fn render_files_toolbar(&mut self, palette: &Palette, cx: &mut Context<Self>) -> gpui::Div {
        #[derive(Clone, Copy)]
        enum Action {
            NewFile,
            NewFolder,
            Search,
            Hidden,
            Refresh,
        }
        let mut controls = div()
            .flex()
            .items_center()
            .gap(px(files_style::CONTROL_GAP));
        for (id, glyph, label, action) in [
            ("files-new-file", "file-plus", "New file", Action::NewFile),
            (
                "files-new-folder",
                "folder-plus",
                "New folder",
                Action::NewFolder,
            ),
            // Retain filename search until the Command Center owns that entrypoint.
            ("files-search", "search", "Search files", Action::Search),
            (
                "files-hidden",
                if self.show_hidden { "eye" } else { "eye-off" },
                if self.show_hidden {
                    "Hide hidden files"
                } else {
                    "Show hidden files"
                },
                Action::Hidden,
            ),
            ("files-refresh", "refresh-cw", "Refresh", Action::Refresh),
        ] {
            if matches!(action, Action::NewFile | Action::NewFolder) && !self.features.fs_entry_ops
            {
                continue;
            }
            let refreshing = matches!(action, Action::Refresh) && self.loading.contains(".");
            controls = controls.child(
                div()
                    .id(id)
                    .size(px(files_style::CONTROL_SIZE))
                    .flex_shrink_0()
                    .rounded(px(radius::MD))
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(!refreshing, |button| {
                        button
                            .cursor_pointer()
                            .hover(|button| button.bg(rgb(palette.surface_sidebar_hover)))
                    })
                    .when(
                        matches!(action, Action::Hidden) && !self.show_hidden,
                        |button| button.bg(rgb(palette.surface_sidebar_hover)),
                    )
                    .tooltip(move |_, cx| cx.new(|_| crate::chrome::Tooltip(label.into())).into())
                    .child(if refreshing {
                        loading_spinner(files_style::GLYPH_SIZE, palette.foreground_extra_muted)
                            .into_any_element()
                    } else {
                        icon(glyph, files_style::GLYPH_SIZE)
                            .text_color(rgb(palette.foreground_extra_muted))
                            .into_any_element()
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            match action {
                                Action::NewFile | Action::NewFolder => this.begin_create(
                                    ".".into(),
                                    matches!(action, Action::NewFolder),
                                    window,
                                    cx,
                                ),
                                Action::Search => this.begin_search(window, cx),
                                Action::Hidden => {
                                    this.show_hidden = !this.show_hidden;
                                    cx.notify();
                                }
                                Action::Refresh if !refreshing => this.refresh(cx),
                                Action::Refresh => {}
                            }
                        }),
                    ),
            );
        }
        let label = match self.sort {
            Sort::Name => "Name",
            Sort::Modified => "Modified",
            Sort::Size => "Size",
        };
        div()
            .h(px(files_style::TOOLBAR_HEIGHT))
            .flex_shrink_0()
            .pr(px(files_style::TRAILING_PADDING))
            .flex()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface_sidebar))
            .child(
                div()
                    .id("files-sort")
                    .h(px(files_style::SORT_HEIGHT))
                    .ml(px(files_style::SORT_MARGIN_LEFT))
                    .px(px(files_style::SORT_PADDING_X))
                    .rounded(px(files_style::SORT_RADIUS))
                    .flex()
                    .items_center()
                    .gap(px(files_style::SORT_GAP))
                    .text_size(px(files_style::SORT_FONT))
                    .text_color(rgb(palette.foreground_muted))
                    .cursor_pointer()
                    .hover(|button| button.bg(rgb(palette.surface_sidebar_hover)))
                    .child(label)
                    .child(icon("chevron-down", 12.).text_color(rgb(palette.foreground_muted)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.sort = match this.sort {
                                Sort::Name => Sort::Modified,
                                Sort::Modified => Sort::Size,
                                Sort::Size => Sort::Name,
                            };
                            cx.notify();
                        }),
                    ),
            )
            .child(controls)
    }

    fn render_file_icon(&mut self, file_name: &str, window: &Window, cx: &App) -> gpui::AnyElement {
        let pixels = (files_style::TREE_ICON * window.scale_factor())
            .ceil()
            .clamp(1., 128.) as i32;
        if self.file_icon_size != pixels {
            self.file_images.clear();
            self.file_icon_size = pixels;
        }
        let (name, svg) = paseo_desktop::file_icons::lookup(file_name);
        if !self.file_images.contains_key(name) {
            let renderer = cx.svg_renderer();
            let image = renderer.parse_svg(svg.as_bytes()).and_then(|svg| {
                renderer.render_parsed(
                    &svg,
                    gpui::SvgSize::ExactSize(size(
                        gpui::DevicePixels(pixels),
                        gpui::DevicePixels(pixels),
                    )),
                )
            });
            if let Ok(image) = image {
                self.file_images.insert(name, image);
            }
        }
        if let Some(image) = self.file_images.get(name) {
            gpui::img(image.clone())
                .size(px(files_style::TREE_ICON))
                .into_any_element()
        } else {
            icon("file-text", files_style::TREE_ICON).into_any_element()
        }
    }

    fn render_files(
        &mut self,
        palette: &Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        // Search replaces the tree while it is open: the field answers a query,
        // and its hits are the only rows that can be opened.
        if self
            .input
            .as_ref()
            .is_some_and(|input| input.kind == InputKind::Search)
        {
            return self.render_search(palette, cx);
        }
        let mut list = div()
            .id("explorer-tree")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .pt(px(files_style::TREE_TOP))
            .pb(px(files_style::TREE_BOTTOM))
            .flex()
            .flex_col();
        if !self.directories.contains_key(".") {
            list = list.child(
                div()
                    .p_3()
                    .text_size(px(font_size::SM))
                    .text_color(rgb(palette.foreground_muted))
                    .child("Loading…"),
            );
        }
        // The creation input leads the tree so new entries are born where they land.
        let creating = self
            .input
            .as_ref()
            .is_some_and(|input| input.kind == InputKind::Create);
        let create_depth = self.input.as_ref().map(|input| {
            if input.target == "." {
                0
            } else {
                input.target.split('/').count() as i32
            }
        });
        if creating {
            let depth = create_depth.unwrap_or(0);
            list = list.child(
                div()
                    .h(px(ROW_HEIGHT))
                    .flex_shrink_0()
                    .pl(px(files_style::TREE_LEFT + depth as f32 * TREE_INDENT))
                    .pr(px(files_style::TREE_RIGHT))
                    .flex()
                    .items_center()
                    .gap(px(files_style::TREE_GAP))
                    .child(self.render_input_row(palette, cx)),
            );
        }
        let rows: Vec<(usize, Entry)> = self
            .rows()
            .into_iter()
            .map(|(depth, entry)| (depth, entry.clone()))
            .collect();
        for (depth, entry) in rows {
            let renaming = self
                .input
                .as_ref()
                .is_some_and(|input| input.kind == InputKind::Rename && input.target == entry.path);
            if renaming {
                list = list.child(
                    div()
                        .h(px(ROW_HEIGHT))
                        .flex_shrink_0()
                        .pl(px(files_style::TREE_LEFT + depth as f32 * TREE_INDENT))
                        .pr(px(files_style::TREE_RIGHT))
                        .flex()
                        .items_center()
                        .gap(px(files_style::TREE_GAP))
                        .child(self.render_input_row(palette, cx)),
                );
                continue;
            }
            let path = entry.path.clone();
            let parent = if entry.path == "." {
                ".".to_owned()
            } else {
                parent_of(&entry.path)
            };
            let directory = entry.kind == EntryKind::Directory;
            let expanded = directory && self.expanded.contains(&path);
            let loading = directory && self.loading.contains(&path);
            let open_path = path.clone();
            let mut row = div()
                .id(gpui::SharedString::from(format!("entry-{path}")))
                .h(px(ROW_HEIGHT))
                .flex_shrink_0()
                .pl(px(files_style::TREE_LEFT + depth as f32 * TREE_INDENT))
                .pr(px(files_style::TREE_RIGHT))
                .flex()
                .items_center()
                .gap(px(files_style::TREE_GAP))
                .text_color(rgb(crate::chrome::control_color(
                    palette.foreground,
                    palette.surface_sidebar,
                    0.76,
                )))
                .cursor_pointer()
                .hover(|row| {
                    row.bg(rgb(palette.surface_sidebar_hover))
                        .text_color(rgb(palette.foreground))
                })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.menu = None;
                        if directory {
                            this.toggle_directory(&open_path, cx);
                        } else {
                            cx.emit(OpenFile(open_path.clone()));
                        }
                    }),
                );
            let file_icon = if directory {
                if loading {
                    loading_spinner(14., palette.foreground_extra_muted).into_any_element()
                } else {
                    icon(
                        if expanded {
                            "chevron-down"
                        } else {
                            "chevron-right"
                        },
                        files_style::TREE_ICON,
                    )
                    .text_color(rgb(palette.foreground_extra_muted))
                    .into_any_element()
                }
            } else {
                self.render_file_icon(&entry.name, window, cx)
            };
            row = row
                .child(
                    div()
                        .size(px(files_style::TREE_ICON))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(file_icon),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .truncate()
                        .line_height(px(files_style::TREE_ICON))
                        .text_size(px(font_size::BASE))
                        .child(entry.name.clone()),
                );
            // The row itself reports its geometry so the context menu anchors to it.
            let entity = cx.entity();
            let menu_path = path.clone();
            let mut row = row.child(
                canvas(
                    move |bounds, _, cx| {
                        entity.update(cx, |this, _| {
                            if this
                                .menu
                                .as_ref()
                                .is_some_and(|menu| menu.path == menu_path)
                            {
                                let bounds = bounds;
                                if let Some(menu) = &mut this.menu {
                                    menu.bounds = bounds;
                                }
                            }
                        })
                    },
                    |_, (), _, _| {},
                )
                .absolute()
                .size_full(),
            );
            let _ = &mut row;
            list = list.child(row.on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    let mut actions = Vec::new();
                    if directory && expanded {
                        actions.push(MenuAction::Collapse(path.clone()));
                    } else if directory {
                        actions.push(MenuAction::NewFile(path.clone()));
                        actions.push(MenuAction::NewFolder(path.clone()));
                    } else {
                        actions.push(MenuAction::CopyPath {
                            path: path.clone(),
                            absolute: false,
                        });
                        actions.push(MenuAction::CopyPath {
                            path: path.clone(),
                            absolute: true,
                        });
                    }
                    actions.push(MenuAction::Rename(path.clone()));
                    actions.push(MenuAction::Duplicate {
                        path: path.clone(),
                        parent: parent.clone(),
                    });
                    actions.push(MenuAction::Delete {
                        path: path.clone(),
                        parent: parent.clone(),
                    });
                    let position = event.position;
                    this.menu = Some(Menu {
                        path: path.clone(),
                        actions,
                        bounds: Bounds::new(position, size(px(0.), px(0.))),
                    });
                    cx.notify();
                }),
            ));
        }
        div()
            .flex_1()
            .min_h_0()
            .relative()
            .flex()
            .flex_col()
            .child(list)
            .into_any_element()
    }

    fn render_input_row(&mut self, palette: &Palette, cx: &mut Context<Self>) -> gpui::AnyElement {
        let palette = *palette;
        let measure = cx.entity();
        let paint = cx.entity();
        let kind = self.input.as_ref().map(|i| i.kind);
        let directory = self.input.as_ref().is_some_and(|i| i.directory);
        let searching = kind == Some(InputKind::Search);
        let creating = kind == Some(InputKind::Create);
        div()
            .flex_1()
            .h(px(22.))
            .when_some(
                self.input.as_ref().map(|input| input.focus.clone()),
                |row, focus| row.track_focus(&focus),
            )
            .px_2()
            .rounded(px(radius::SM))
            .bg(rgb(palette.input))
            .border_1()
            .border_color(rgb(palette.border_accent))
            .cursor_text()
            .flex()
            .items_center()
            .gap_1()
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if this
                    .input
                    .as_ref()
                    .is_none_or(|input| input.query.marked.is_some())
                {
                    return;
                }
                let key = event.keystroke.key.clone();
                match key.as_str() {
                    "escape" => {
                        this.input = None;
                        this.focus.focus(window, cx);
                        cx.notify();
                    }
                    "enter" => this.commit_input(cx),
                    "backspace" => {
                        if let Some(input) = &mut this.input {
                            input.query.delete(false);
                            window.notify_text_input_changed(&input.focus);
                        }
                        cx.notify();
                    }
                    "left" | "right" => {
                        let forward = key == "right";
                        let extend = event.keystroke.modifiers.shift;
                        if let Some(input) = &mut this.input {
                            input.query.horizontal(forward, extend);
                            window.notify_text_input_changed(&input.focus);
                        }
                        cx.notify();
                    }
                    _ => return,
                }
                window.prevent_default();
                cx.stop_propagation();
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    if let Some(input) = &mut this.input {
                        input.focus.focus(window, cx);
                        input.selecting = true;
                        let index = input.line.as_ref().map_or(0, |line| {
                            line.closest_index_for_x(
                                event.position.x - input.bounds.left() + input.scroll_x,
                            )
                        });
                        input.query.select(index, event.modifiers.shift);
                        window.notify_text_input_changed(&input.focus);
                    }
                    cx.notify();
                }),
            )
            .on_mouse_move(
                cx.listener(|this, event: &gpui::MouseMoveEvent, window, cx| {
                    if let Some(input) = &mut this.input
                        && input.selecting
                    {
                        let index = input.line.as_ref().map_or(0, |line| {
                            line.closest_index_for_x(
                                event.position.x - input.bounds.left() + input.scroll_x,
                            )
                        });
                        input.query.select(index, true);
                        window.notify_text_input_changed(&input.focus);
                        cx.notify();
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| {
                    if let Some(input) = &mut this.input {
                        input.selecting = false;
                    }
                }),
            )
            .child(
                icon(
                    if searching {
                        "search"
                    } else if directory {
                        "folder"
                    } else {
                        "file-text"
                    },
                    12.,
                )
                .text_color(rgb(palette.foreground_muted)),
            )
            .child(
                canvas(
                    move |bounds, window, cx| {
                        measure.update(cx, |this, _| {
                            if let Some(input) = &mut this.input {
                                input.bounds = bounds;
                                let empty = input.query.text.is_empty();
                                let text = if empty {
                                    if searching {
                                        "Search files…".to_owned()
                                    } else if directory {
                                        "Folder name".to_owned()
                                    } else {
                                        "File name".to_owned()
                                    }
                                } else {
                                    input.query.text.clone()
                                };
                                let text_len = text.len();
                                let line = window.text_system().shape_line(
                                    text.into(),
                                    px(font_size::SM),
                                    &[TextRun {
                                        len: text_len,
                                        font: font(crate::fonts::ui_family()),
                                        color: rgb(if empty {
                                            palette.foreground_extra_muted
                                        } else {
                                            palette.foreground
                                        })
                                        .into(),
                                        background_color: None,
                                        underline: None,
                                        strikethrough: None,
                                    }],
                                    None,
                                );
                                let x = line.x_for_index(input.query.cursor());
                                input.scroll_x = input
                                    .scroll_x
                                    .max(x - bounds.size.width + px(1.))
                                    .min(x)
                                    .max(px(0.));
                                input.line = Some(line);
                            }
                        });
                    },
                    move |bounds, (), window, cx| {
                        paint.update(cx, |this, cx| {
                            let Some(input) = &mut this.input else {
                                return;
                            };
                            window.handle_input(
                                &input.focus,
                                ElementInputHandler::new(bounds, paint.clone()),
                                cx,
                            );
                            window.with_content_mask(
                                Some(gpui::ContentMask { bounds }),
                                |window| {
                                    if let Some(line) = &input.line {
                                        let _ = line.paint(
                                            bounds.origin - point(input.scroll_x, px(0.)),
                                            px(22.),
                                            TextAlign::Left,
                                            None,
                                            window,
                                            cx,
                                        );
                                        if input.focus.is_focused(window) {
                                            let caret = input.caret() - bounds.origin;
                                            window.paint_quad(fill(
                                                Bounds::new(
                                                    bounds.origin + point(caret.x, px(3.)),
                                                    size(px(1.), px(16.)),
                                                ),
                                                rgb(palette.foreground),
                                            ));
                                        }
                                    }
                                },
                            );
                        });
                    },
                )
                .flex_1()
                .h_full(),
            )
            .when(!searching, |row| {
                row.child(
                    div()
                        .id("entry-input-commit")
                        .flex_shrink_0()
                        .px_1()
                        .rounded(px(radius::SM))
                        .cursor_pointer()
                        .hover(|v| v.bg(rgb(palette.surface3)))
                        .text_color(rgb(palette.accent_bright))
                        .text_size(px(font_size::SM))
                        .child(if creating { "Create" } else { "Rename" })
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.commit_input(cx);
                            }),
                        ),
                )
            })
            .child(
                div()
                    .id("entry-input-cancel")
                    .flex_shrink_0()
                    .px_1()
                    .rounded(px(radius::SM))
                    .cursor_pointer()
                    .hover(|v| v.bg(rgb(palette.surface3)))
                    .text_color(rgb(palette.foreground_muted))
                    .child(icon("x", 12.).text_color(rgb(palette.foreground_muted)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.input = None;
                            this.search.generation += 1;
                            this.reset_search_results();
                            cx.notify();
                        }),
                    ),
            )
            .into_any_element()
    }

    /// The filename search field and its hits, over
    /// `directory_suggestions_request` with files only.
    fn render_search(&mut self, palette: &Palette, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mut list = div()
            .id("file-search")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_1()
            .py_1()
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(28.))
                    .flex_shrink_0()
                    .mb(px(2.))
                    .flex()
                    .items_center()
                    .child(self.render_input_row(palette, cx)),
            );
        if let Some(error) = self.search.error.clone() {
            list = list.child(
                div()
                    .px_2()
                    .py_1()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(px(font_size::SM))
                    .text_color(rgb(palette.status_danger))
                    .child(div().min_w_0().flex_1().truncate().child(error)),
            );
        }
        if self.search.loading {
            list = list.child(
                div()
                    .px_2()
                    .py_1()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(px(font_size::SM))
                    .text_color(rgb(palette.foreground_muted))
                    .child(loading_spinner(12., palette.foreground_muted))
                    .child("Searching…"),
            );
        } else if self.search.hits.is_empty() && self.search.error.is_none() {
            list = list.child(
                div()
                    .px_2()
                    .py_1()
                    .text_size(px(font_size::SM))
                    .text_color(rgb(palette.foreground_muted))
                    .child("No files match."),
            );
        }
        for hit in self.search.hits.clone() {
            let path = hit.path.clone();
            list = list.child(
                div()
                    .id(gpui::SharedString::from(format!("search-{path}")))
                    .h(px(ROW_HEIGHT))
                    .flex_shrink_0()
                    .mb(px(1.))
                    .px_2()
                    .rounded(px(radius::SM))
                    .flex()
                    .items_center()
                    .gap_2()
                    .cursor_pointer()
                    .hover(|row| row.bg(rgb(palette.surface_sidebar_hover)))
                    .child(icon("file-text", 14.).text_color(rgb(palette.foreground_muted)))
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(font_size::SM))
                            .child(hit.name.clone()),
                    )
                    .when(!hit.directory.is_empty(), |row| {
                        row.child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .truncate()
                                .text_size(px(font_size::SM))
                                .text_color(rgb(palette.foreground_extra_muted))
                                .child(hit.directory.clone()),
                        )
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.input = None;
                            this.search.generation += 1;
                            this.reset_search_results();
                            cx.emit(OpenFile(path.clone()));
                            cx.notify();
                        }),
                    ),
            );
        }
        div()
            .flex_1()
            .min_h_0()
            .relative()
            .flex()
            .flex_col()
            .child(list)
            .into_any_element()
    }

    fn render_changes(&mut self, palette: &Palette, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mut content = div()
            .id("changes-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_1()
            .py_1()
            .flex()
            .flex_col();
        let Some(status) = self.status.clone() else {
            content = content
                .child(
                    div()
                        .p_3()
                        .text_size(px(font_size::SM))
                        .text_color(rgb(palette.foreground_muted))
                        .child(if self.status_pending {
                            "Loading…"
                        } else {
                            "Git status is unavailable."
                        }),
                )
                .when_some(self.status_error.clone(), |content, error| {
                    content.child(
                        div()
                            .px_2()
                            .pb_2()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_size(px(font_size::SM))
                            .text_color(rgb(palette.status_danger))
                            .child(div().flex_1().truncate().child(error))
                            .child(
                                div()
                                    .id("status-retry")
                                    .flex_shrink_0()
                                    .cursor_pointer()
                                    .text_color(rgb(palette.foreground_muted))
                                    .child("Retry")
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.status_error = None;
                                            this.refresh(cx);
                                        }),
                                    ),
                            ),
                    )
                });
            return content.into_any_element();
        };
        if !status.is_git {
            return div()
                .flex_1()
                .min_h_0()
                .p_3()
                .text_size(px(font_size::SM))
                .text_color(rgb(palette.foreground_muted))
                .child("This workspace is not a git repository.")
                .into_any_element();
        }
        let branch = status
            .current_branch
            .clone()
            .unwrap_or_else(|| "detached".into());
        content = content.child(
            div()
                .id("git-branch-row")
                .h(px(28.))
                .flex_shrink_0()
                .px_1()
                .rounded(px(radius::SM))
                .flex()
                .items_center()
                .gap_2()
                .cursor_pointer()
                .hover(|row| row.bg(rgb(palette.surface_sidebar_hover)))
                .text_size(px(font_size::SM))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.open_git_menu(GitMenuKind::Branch, event.position, cx);
                    }),
                )
                .child(icon("git-branch", 14.).text_color(rgb(palette.foreground_muted)))
                .child(div().min_w_0().truncate().child(branch))
                .child(icon("chevron-down", 10.).text_color(rgb(palette.foreground_muted)))
                .when_some(status.ahead.filter(|ahead| *ahead > 0), |row, ahead| {
                    row.child(
                        div()
                            .flex()
                            .items_center()
                            .text_color(rgb(palette.status_dot_success))
                            .child(icon("arrow-up", 10.).text_color(rgb(palette.foreground_muted)))
                            .child(ahead.to_string()),
                    )
                })
                .when_some(status.behind.filter(|behind| *behind > 0), |row, behind| {
                    row.child(
                        div()
                            .flex()
                            .items_center()
                            .text_color(rgb(palette.status_dot_warning))
                            .child(
                                icon("arrow-down", 10.).text_color(rgb(palette.foreground_muted)),
                            )
                            .child(behind.to_string()),
                    )
                }),
        );
        content = content.child(self.render_git_actions(palette, cx));
        content = content.child(self.render_pr(palette, cx));
        if let Some(error) = self.git_error.clone() {
            content = content.child(
                div()
                    .px_2()
                    .py_1()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(px(font_size::SM))
                    .text_color(rgb(palette.status_danger))
                    .child(div().min_w_0().flex_1().truncate().child(error))
                    .child(
                        div()
                            .id("git-error-dismiss")
                            .flex_shrink_0()
                            .cursor_pointer()
                            .text_color(rgb(palette.foreground_muted))
                            .child(icon("x", 12.).text_color(rgb(palette.foreground_muted)))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.git_error = None;
                                    cx.notify();
                                }),
                            ),
                    ),
            );
        }
        if let Some(error) = self.diff_error.clone() {
            content = content.child(
                div()
                    .px_2()
                    .py_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(px(font_size::SM))
                    .text_color(rgb(palette.status_danger))
                    .child(div().flex_1().truncate().child(error))
                    .child(
                        div()
                            .id("changes-retry")
                            .flex_shrink_0()
                            .cursor_pointer()
                            .text_color(rgb(palette.foreground_muted))
                            .child("Retry")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.diff_error = None;
                                    this.refresh(cx);
                                }),
                            ),
                    ),
            );
        }
        let diff = self.diff.clone().unwrap_or_default();
        if diff.is_empty() && self.diff_error.is_none() {
            content = content.child(
                div()
                    .p_3()
                    .text_size(px(font_size::SM))
                    .text_color(rgb(palette.foreground_muted))
                    .child(if self.diff_loading {
                        "Loading…"
                    } else if self.diff_too_large {
                        "The working diff is too large to display."
                    } else {
                        "No changes."
                    }),
            );
        }
        let selected = self.selected_diff.clone();
        for file in &diff {
            let path = file.path.clone();
            let discard_path = path.clone();
            let open = selected.as_deref() == Some(path.as_str());
            let name = path.rsplit('/').next().unwrap_or(&path).to_owned();
            let directory = if name == path {
                String::new()
            } else {
                format!("{}/", &path[..path.len() - name.len() - 1])
            };
            content = content.child(
                div()
                    .flex_shrink_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .id(gpui::SharedString::from(format!("change-{path}")))
                            .h(px(ROW_HEIGHT))
                            .mb(px(1.))
                            .pl_2()
                            .pr_1()
                            .rounded(px(radius::SM))
                            .flex()
                            .items_center()
                            .gap_2()
                            .cursor_pointer()
                            .when(open, |row| row.bg(rgb(palette.surface_sidebar_hover)))
                            .hover(|row| row.bg(rgb(palette.surface_sidebar_hover)))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.selected_diff =
                                        if this.selected_diff.as_deref() == Some(path.as_str()) {
                                            None
                                        } else {
                                            Some(path.clone())
                                        };
                                    cx.notify();
                                }),
                            )
                            .child(div().text_size(px(font_size::SM)).truncate().child(name))
                            .when(!directory.is_empty(), |row| {
                                row.child(
                                    div()
                                        .min_w_0()
                                        .flex_1()
                                        .truncate()
                                        .text_size(px(font_size::SM))
                                        .text_color(rgb(palette.foreground_extra_muted))
                                        .child(directory.clone()),
                                )
                            })
                            .when(directory.is_empty(), |row| row.child(div().flex_1()))
                            .child(
                                div()
                                    .text_size(px(font_size::SM))
                                    .text_color(rgb(palette.diff_addition))
                                    .child(format!("+{}", file.additions)),
                            )
                            .child(
                                div()
                                    .text_size(px(font_size::SM))
                                    .text_color(rgb(palette.diff_deletion))
                                    .child(format!("-{}", file.deletions)),
                            )
                            .child(
                                div()
                                    .w(px(14.))
                                    .text_size(px(font_size::SM))
                                    .text_color(rgb(palette.foreground_muted))
                                    .child(badge(file)),
                            )
                            .when(self.features.discard_changes, |row| {
                                row.child(
                                    div()
                                        .id(gpui::SharedString::from(format!(
                                            "discard-{discard_path}"
                                        )))
                                        .flex_shrink_0()
                                        .size(px(18.))
                                        .rounded(px(radius::SM))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .cursor_pointer()
                                        .text_color(rgb(palette.foreground_muted))
                                        .hover(|button| button.bg(rgb(palette.surface3)))
                                        .child(
                                            icon("x", 12.)
                                                .text_color(rgb(palette.foreground_muted)),
                                        )
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(move |this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.request_discard(discard_path.clone(), cx);
                                            }),
                                        ),
                                )
                            }),
                    )
                    .when(open && !file.binary && !file.too_large, |row| {
                        row.child(
                            div()
                                .flex_shrink_0()
                                .pb_2()
                                .pl(px(12.))
                                .flex()
                                .flex_col()
                                .children(file.hunks.iter().map(|hunk| {
                                    div()
                                        .flex()
                                        .flex_col()
                                        .child(
                                            div()
                                                .h(px(20.))
                                                .px_2()
                                                .font_family(crate::fonts::code_family())
                                                .text_size(px(font_size::CODE))
                                                .text_color(rgb(palette.foreground_muted))
                                                .child(hunk.header.clone()),
                                        )
                                        .children(hunk.lines.iter().map(|(kind, line)| {
                                            let (bg, color) = match kind {
                                                LineKind::Add => (
                                                    rgba(
                                                        palette.diff_addition << 8
                                                            | DIFF_WASH_ALPHA,
                                                    ),
                                                    palette.diff_addition,
                                                ),
                                                LineKind::Remove => (
                                                    rgba(
                                                        palette.diff_deletion << 8
                                                            | DIFF_WASH_ALPHA,
                                                    ),
                                                    palette.diff_deletion,
                                                ),
                                                LineKind::Context => {
                                                    (rgba(0x00000000), palette.foreground)
                                                }
                                            };
                                            div()
                                                .min_h(px(18.))
                                                .px_2()
                                                .bg(bg)
                                                .font_family(crate::fonts::code_family())
                                                .text_size(px(font_size::CODE))
                                                .text_color(rgb(color))
                                                .child(if line.is_empty() {
                                                    " ".to_owned()
                                                } else {
                                                    line.clone()
                                                })
                                        }))
                                })),
                        )
                    })
                    .when(open && file.binary, |row| {
                        row.child(
                            div()
                                .px_2()
                                .py_1()
                                .text_size(px(font_size::SM))
                                .text_color(rgb(palette.foreground_muted))
                                .child("Binary file."),
                        )
                    })
                    .when(open && file.too_large, |row| {
                        row.child(
                            div()
                                .px_2()
                                .py_1()
                                .text_size(px(font_size::SM))
                                .text_color(rgb(palette.foreground_muted))
                                .child("This file's diff is too large to display."),
                        )
                    }),
            );
        }
        content.into_any_element()
    }

    /// Commit, pull, push and refresh, the four actions the changes header owns.
    fn render_git_actions(
        &mut self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let ops = self.ops.clone();
        let state = |op: GitOp| ops.get(&op).copied();
        div()
            .flex_shrink_0()
            .px_1()
            .pb_1()
            .flex()
            .items_center()
            .gap_1()
            .child(git_button(
                "git-commit",
                "Commit",
                "check",
                state(GitOp::Commit),
                palette,
                cx,
                |this, cx| this.run_commit(cx),
            ))
            .child(git_button(
                "git-pull",
                "Pull",
                "arrow-down",
                state(GitOp::Pull),
                palette,
                cx,
                |this, cx| this.run_pull(cx),
            ))
            .child(git_button(
                "git-push",
                "Push",
                "arrow-up",
                state(GitOp::Push),
                palette,
                cx,
                |this, cx| this.run_push(cx),
            ))
            .child(git_button(
                "git-refresh",
                "Refresh",
                "refresh-cw",
                state(GitOp::Refresh),
                palette,
                cx,
                |this, cx| this.run_refresh(cx),
            ))
            .into_any_element()
    }

    /// The change request row: state, checks, open, create and merge. The host
    /// advertises `forgeProviders`; without it the row does not exist.
    fn render_pr(&mut self, palette: &Palette, cx: &mut Context<Self>) -> gpui::AnyElement {
        if !self.features.forge_providers {
            return div().into_any_element();
        }
        let mut block = div().flex_shrink_0().px_1().flex().flex_col();
        if let Some(error) = self.pr_error.clone() {
            block = block.child(
                div()
                    .py_1()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(px(font_size::SM))
                    .text_color(rgb(palette.status_danger))
                    .child(div().min_w_0().flex_1().truncate().child(error))
                    .child(
                        div()
                            .id("pr-retry")
                            .flex_shrink_0()
                            .cursor_pointer()
                            .text_color(rgb(palette.foreground_muted))
                            .child("Retry")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.pr_error = None;
                                    this.blocked_pr_retry(cx);
                                }),
                            ),
                    ),
            );
        }
        if let Some(pr) = self.pr.clone() {
            let url = pr.url.clone();
            let mut row = div()
                .id("pr-row")
                .h(px(28.))
                .flex()
                .items_center()
                .gap_2()
                .text_size(px(font_size::SM))
                .child(
                    div()
                        .size(px(6.))
                        .rounded_full()
                        .bg(rgb(pr.checks_tone(palette))),
                )
                .child(div().min_w_0().truncate().child(match pr.number {
                    Some(number) => format!("#{number} {}", pr.title),
                    None => pr.title.clone(),
                }))
                .when(pr.forge != "github", |row| {
                    // GitLab and other forges keep their own vocabulary here.
                    row.child(
                        div()
                            .flex_shrink_0()
                            .text_color(rgb(palette.foreground_extra_muted))
                            .child(pr.forge.clone()),
                    )
                })
                .child(
                    div()
                        .id("pr-checks")
                        .flex_shrink_0()
                        .cursor_pointer()
                        .text_color(rgb(palette.foreground_muted))
                        .child(pr.checks_summary(palette))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| {
                                cx.stop_propagation();
                                this.open_git_menu(GitMenuKind::Checks, event.position, cx);
                            }),
                        ),
                );
            if !url.is_empty() {
                row = row.child(git_text_button(
                    "pr-open",
                    "Open",
                    palette.accent_bright,
                    palette,
                    cx,
                    move |this, _, cx| this.open_external(url.clone(), cx),
                ));
            }
            // The actions menu is not gated by direct-merge readiness: cancelling
            // a scheduled auto-merge must stay reachable while the merge state is
            // blocked or unknown.
            let auto_merge_available = self.features.forge_set_auto_merge
                && (pr.auto_merge_enabled || pr.auto_merge_enable_blocker.is_none());
            let merge_ready = pr.can_merge();
            if merge_ready || auto_merge_available {
                row = row.child(git_text_button(
                    "pr-merge",
                    if merge_ready { "Merge" } else { "Auto-merge" },
                    palette.foreground,
                    palette,
                    cx,
                    |this, position, cx| this.open_git_menu(GitMenuKind::PullRequest, position, cx),
                ));
            }
            block = block.child(row);
            if pr.is_draft || pr.is_merged || !pr.is_open() {
                block = block.child(
                    div()
                        .pb_1()
                        .text_size(px(font_size::SM))
                        .text_color(rgb(palette.foreground_muted))
                        .child(if pr.is_merged {
                            "Merged".to_owned()
                        } else if pr.is_draft {
                            "Draft".to_owned()
                        } else {
                            pr.state.to_lowercase()
                        }),
                );
            }
        } else if !self.pr_loading {
            let state = self.ops.get(&GitOp::CreatePr).copied();
            block = block.child(div().h(px(28.)).flex().items_center().child(git_button(
                "pr-create",
                "Create change request",
                "git-branch",
                state,
                palette,
                cx,
                |this, cx| this.run_create_pr(cx),
            )));
        }
        if let Some(details) = self.check_details.clone() {
            let mut detail = div()
                .pb_1()
                .flex()
                .flex_col()
                .gap_1()
                .text_size(px(font_size::SM))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .truncate()
                                .child(details.name.clone()),
                        )
                        .when_some(details.conclusion.clone(), |row, conclusion| {
                            row.child(
                                div()
                                    .flex_shrink_0()
                                    .text_color(rgb(palette.foreground_muted))
                                    .child(conclusion),
                            )
                        }),
                );
            if !details.failed_jobs.is_empty() {
                detail = detail.child(
                    div()
                        .text_color(rgb(palette.status_danger))
                        .child(format!("Failed jobs: {}", details.failed_jobs.join(", "))),
                );
            }
            if let Some(url) = details.url.clone() {
                detail = detail.child(git_text_button(
                    "check-open",
                    "Open run",
                    palette.accent_bright,
                    palette,
                    cx,
                    move |this, _, cx| this.open_external(url.clone(), cx),
                ));
            }
            block = block.child(detail);
        }
        if let Some(error) = self.check_details_error.clone() {
            block = block.child(
                div()
                    .pb_1()
                    .text_size(px(font_size::SM))
                    .text_color(rgb(palette.status_danger))
                    .child(error),
            );
        }
        block.into_any_element()
    }

    fn open_external(&mut self, url: String, cx: &mut Context<Self>) {
        // No embedded browser exists in the native client yet; the OS opens it.
        cx.open_url(&url);
    }

    /// Re-ask for the change request after a failure without waiting for the
    /// next status push.
    fn blocked_pr_retry(&mut self, cx: &mut Context<Self>) {
        self.pr_loading = false;
        self.maybe_request_pr(cx);
    }

    fn render_git_menu(
        &mut self,
        menu: &GitMenu,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let kind = menu.kind;
        let rows: Vec<gpui::AnyElement> = match kind {
            GitMenuKind::Branch => self.branch_menu_rows(palette, cx),
            GitMenuKind::PullRequest => self.pr_menu_rows(palette, cx),
            GitMenuKind::Checks => self.check_menu_rows(palette, cx),
        };
        div()
            .id("git-menu")
            .w(px(240.))
            .max_h(px(320.))
            .overflow_y_scroll()
            .rounded(px(radius::LG))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.popover))
            .text_size(px(font_size::SM))
            .text_color(rgb(palette.popover_foreground))
            .py_1()
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                if this.git_menu.take().is_some() {
                    cx.notify();
                }
            }))
            .children(rows)
            .into_any_element()
    }

    fn branch_menu_rows(
        &mut self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        let current = self
            .status
            .as_ref()
            .and_then(|status| status.current_branch.clone());
        let mut rows = vec![menu_heading("Branch", palette)];
        if let Some(error) = self.branches_error.clone() {
            rows.push(menu_note(error, palette));
        } else if self.branches_loading && self.branches.is_empty() {
            rows.push(menu_note("Loading branches…".into(), palette));
        }
        if self.branches.is_empty() && !self.branches_loading && self.branches_error.is_none() {
            rows.push(menu_note("No branches.".into(), palette));
        }
        for branch in self.branches.clone() {
            let selected = current.as_deref() == Some(branch.as_str());
            rows.push(menu_row(
                branch.clone(),
                None,
                palette.foreground_muted,
                selected,
                palette,
                cx,
                GitMenuAction::SwitchBranch(branch),
            ));
        }
        rows.push(menu_heading("Stashes", palette));
        if self.stashes.is_empty() {
            rows.push(menu_note("No stashes on this branch.".into(), palette));
        }
        for stash in self.stashes.clone() {
            rows.push(menu_row(
                format!("@{}{}", stash.index, stash.message),
                stash.branch.clone(),
                palette.foreground_muted,
                false,
                palette,
                cx,
                GitMenuAction::RestoreStash(stash.index),
            ));
        }
        rows
    }

    fn pr_menu_rows(&mut self, palette: &Palette, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let Some(pr) = self.pr.clone() else {
            return vec![menu_note("No change request yet.".into(), palette)];
        };
        let mut rows = Vec::new();
        if self.features.forge_set_auto_merge {
            if pr.auto_merge_enabled {
                if pr.can_disable_auto_merge {
                    rows.push(menu_row(
                        "Disable auto-merge".into(),
                        None,
                        palette.foreground_muted,
                        false,
                        palette,
                        cx,
                        GitMenuAction::AutoMerge {
                            enabled: false,
                            method: None,
                        },
                    ));
                } else {
                    rows.push(menu_note(
                        "Auto-merge is on, but it cannot be cancelled from here.".into(),
                        palette,
                    ));
                }
            } else {
                rows.push(menu_heading("Auto-merge when ready", palette));
                match pr.auto_merge_enable_blocker {
                    // Each capability is gated by its own fact: direct-merge
                    // readiness says nothing about scheduling an auto-merge.
                    None => {
                        for method in pr.merge_methods.clone() {
                            rows.push(menu_row(
                                method.label().to_owned(),
                                None,
                                palette.foreground_muted,
                                false,
                                palette,
                                cx,
                                GitMenuAction::AutoMerge {
                                    enabled: true,
                                    method: Some(method),
                                },
                            ));
                        }
                    }
                    Some(blocker) => rows.push(menu_note(blocker.into(), palette)),
                }
            }
        }
        rows.push(menu_heading("Merge", palette));
        if pr.can_merge() {
            for method in pr.merge_methods.clone() {
                rows.push(menu_row(
                    method.label().to_owned(),
                    None,
                    palette.foreground_muted,
                    false,
                    palette,
                    cx,
                    GitMenuAction::Merge(method),
                ));
            }
        } else {
            rows.push(menu_note(pr_merge_blocker(&pr).into(), palette));
        }
        rows
    }

    fn check_menu_rows(
        &mut self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        let Some(pr) = self.pr.clone() else {
            return vec![menu_note("No change request yet.".into(), palette)];
        };
        if pr.checks.is_empty() {
            return vec![menu_note("No checks reported.".into(), palette)];
        }
        let details_enabled = self.features.forge_check_details;
        let mut rows = Vec::new();
        for check in pr.checks {
            let details = || GitMenuAction::CheckDetails {
                name: check.name.clone(),
                check_run_id: check.check_run_id,
                workflow_run_id: check.workflow_run_id,
            };
            let detail = Some(check.status.clone());
            match check_actions(&check, details_enabled) {
                CheckActions::Details => rows.push(menu_row(
                    check.name.clone(),
                    detail,
                    check.tone(palette),
                    false,
                    palette,
                    cx,
                    details(),
                )),
                CheckActions::OpenUrl => {
                    let url = check.url.clone().unwrap_or_default();
                    rows.push(menu_row(
                        check.name.clone(),
                        detail,
                        check.tone(palette),
                        false,
                        palette,
                        cx,
                        GitMenuAction::OpenUrl(url),
                    ));
                }
                CheckActions::Both => {
                    let url = check.url.clone().unwrap_or_default();
                    rows.push(menu_row(
                        check.name.clone(),
                        detail.clone(),
                        check.tone(palette),
                        false,
                        palette,
                        cx,
                        details(),
                    ));
                    rows.push(menu_row(
                        format!("Open {} run", check.name),
                        None,
                        palette.foreground_muted,
                        false,
                        palette,
                        cx,
                        GitMenuAction::OpenUrl(url),
                    ));
                }
                CheckActions::Neither => rows.push(menu_status_row(
                    check.name.clone(),
                    detail,
                    check.tone(palette),
                )),
            }
        }
        rows
    }
}

/// A git action button: a spinner while pending, the success tone once it lands.
fn git_button(
    id: &'static str,
    label: &'static str,
    glyph: &'static str,
    state: Option<OpState>,
    palette: &Palette,
    cx: &mut Context<WorkspaceTools>,
    action: impl Fn(&mut WorkspaceTools, &mut Context<WorkspaceTools>) + 'static,
) -> gpui::AnyElement {
    let pending = state == Some(OpState::Pending);
    let success = state == Some(OpState::Success);
    let unknown = state == Some(OpState::Unknown);
    let blocked = row_blocked(state);
    div()
        .id(id)
        .h(px(24.))
        .px_2()
        .rounded(px(radius::SM))
        .flex()
        .items_center()
        .gap_1()
        .text_size(px(font_size::SM))
        .text_color(rgb(if success {
            palette.status_dot_success
        } else if unknown {
            palette.status_warning
        } else {
            palette.foreground
        }))
        .when(pending, |button| button.opacity(0.7))
        .when(!blocked, |button| {
            button
                .cursor_pointer()
                .hover(|button| button.bg(rgb(palette.surface3)))
        })
        .child(if pending {
            loading_spinner(12., palette.foreground_muted).into_any_element()
        } else {
            icon(glyph, 12.)
                .text_color(rgb(palette.foreground_muted))
                .into_any_element()
        })
        .child(label)
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                if !blocked {
                    action(this, cx);
                }
            }),
        )
        .into_any_element()
}

/// A compact text action inside the change request row.
fn git_text_button(
    id: &'static str,
    label: &'static str,
    tone: u32,
    palette: &Palette,
    cx: &mut Context<WorkspaceTools>,
    action: impl Fn(&mut WorkspaceTools, Point<Pixels>, &mut Context<WorkspaceTools>) + 'static,
) -> gpui::AnyElement {
    div()
        .id(id)
        .flex_shrink_0()
        .h(px(22.))
        .px_1()
        .rounded(px(radius::SM))
        .flex()
        .items_center()
        .text_size(px(font_size::SM))
        .text_color(rgb(tone))
        .cursor_pointer()
        .hover(|button| button.bg(rgb(palette.surface3)))
        .child(label)
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                cx.stop_propagation();
                action(this, event.position, cx);
            }),
        )
        .into_any_element()
}

fn menu_heading(label: &'static str, palette: &Palette) -> gpui::AnyElement {
    div()
        .px_3()
        .pt_1()
        .pb(px(2.))
        .text_size(px(font_size::SM))
        .text_color(rgb(palette.foreground_extra_muted))
        .child(label)
        .into_any_element()
}

fn menu_note(label: String, palette: &Palette) -> gpui::AnyElement {
    div()
        .px_3()
        .py_1()
        .text_size(px(font_size::SM))
        .text_color(rgb(palette.foreground_muted))
        .child(label)
        .into_any_element()
}

fn menu_row(
    label: String,
    detail: Option<String>,
    detail_tone: u32,
    selected: bool,
    palette: &Palette,
    cx: &mut Context<WorkspaceTools>,
    action: GitMenuAction,
) -> gpui::AnyElement {
    div()
        .id(gpui::SharedString::from(format!("git-menu-{label}")))
        .min_h(px(28.))
        .px_3()
        .py_1()
        .flex()
        .items_center()
        .gap_2()
        .cursor_pointer()
        .hover(|row| row.bg(rgb(palette.surface2)))
        .child(div().min_w_0().flex_1().truncate().child(label))
        .when_some(detail, |row, detail| {
            row.child(
                div()
                    .flex_shrink_0()
                    .text_color(rgb(detail_tone))
                    .child(detail),
            )
        })
        .when(selected, |row| {
            row.child(icon("check", 12.).text_color(rgb(palette.status_dot_success)))
        })
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.git_menu_action(action.clone(), cx);
            }),
        )
        .into_any_element()
}

/// A check row with no action the host can run: still shows its status.
fn menu_status_row(label: String, detail: Option<String>, detail_tone: u32) -> gpui::AnyElement {
    div()
        .min_h(px(28.))
        .px_3()
        .py_1()
        .flex()
        .items_center()
        .gap_2()
        .child(div().min_w_0().flex_1().truncate().child(label))
        .when_some(detail, |row, detail| {
            row.child(
                div()
                    .flex_shrink_0()
                    .text_color(rgb(detail_tone))
                    .child(detail),
            )
        })
        .into_any_element()
}

/// Why merge is not on offer, in the original app's terms.
fn pr_merge_blocker(pr: &PrStatus) -> &'static str {
    if pr.is_merged {
        "The change request is already merged."
    } else if pr.is_draft {
        "The change request is still a draft."
    } else if !pr.is_open() {
        "The change request is closed."
    } else if pr.mergeable == "CONFLICTING" {
        "The change request has conflicts."
    } else if let Some(blocker) = pr.direct_merge_blocker {
        blocker
    } else if pr.merge_methods.is_empty() {
        "This repository allows no merge method for this account."
    } else {
        "The forge has not reported the change request as mergeable yet."
    }
}

/// The workspace scripts menu: run, stop, restart, copy a service URL, or open
/// a script's terminal. One row per script, in workspace order.
pub struct ScriptsMenu {
    rows: Vec<Script>,
    host: String,
    local_endpoint: bool,
    pending: Option<String>,
    error: Option<String>,
    /// The window's settings, so the menu follows the active theme.
    shared: SharedSettings,
    focus: FocusHandle,
}

pub struct ScriptsPicked(pub Option<ScriptAction>);

impl EventEmitter<ScriptsPicked> for ScriptsMenu {}

impl ScriptsMenu {
    pub fn new(
        rows: Vec<Script>,
        host: String,
        local_endpoint: bool,
        pending: Option<String>,
        error: Option<String>,
        shared: SharedSettings,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            rows,
            host,
            local_endpoint,
            pending,
            error,
            shared,
            focus: cx.focus_handle(),
        }
    }

    fn emit(&mut self, action: ScriptAction, cx: &mut Context<Self>) {
        cx.emit(ScriptsPicked(Some(action)));
    }
}

impl Focusable for ScriptsMenu {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ScriptsMenu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = active_palette(&self.shared, window);
        let rows = self.rows.clone();
        let pending = self.pending.clone();
        let host = self.host.clone();
        div()
            .id("scripts-menu")
            .track_focus(&self.focus)
            .w(px(280.))
            .max_h(px(360.))
            .overflow_y_scroll()
            .rounded(px(radius::LG))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.popover))
            .text_size(px(font_size::SM))
            .text_color(rgb(palette.popover_foreground))
            .py_1()
            .on_key_down(cx.listener(|_, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    cx.emit(ScriptsPicked(None));
                    cx.stop_propagation();
                }
            }))
            .on_mouse_down_out(cx.listener(|_, _, _, cx| {
                cx.emit(ScriptsPicked(None));
            }))
            .when_some(self.error.clone(), |menu, error| {
                menu.child(
                    div()
                        .px_3()
                        .py_1()
                        .text_color(rgb(palette.status_danger))
                        .child(error),
                )
            })
            .children(rows.into_iter().map(|script| {
                let tone = script.tone();
                let name = script.name.clone();
                let can_start = pending.as_ref() != Some(&script.name);
                let running = script.lifecycle == "running";
                // Public first, then the Paseo proxy, then the direct host URL.
                let targets = script_targets(&script, &host, self.local_endpoint);
                let script_id = script.name.clone();
                div()
                    .id(gpui::SharedString::from(format!("script-{}", script.name)))
                    .min_h(px(32.))
                    .px_3()
                    .py_1()
                    .flex()
                    .items_center()
                    .gap_2()
                    .hover(|row| row.bg(rgb(palette.surface2)))
                    .child(icon("play", 14.).text_color(rgb(tone.color(&palette))))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .child(div().truncate().child(script.name.clone()))
                            .child(div().text_color(rgb(palette.foreground_muted)).child(
                                if running {
                                    if script.is_service {
                                        script.health.clone().unwrap_or_else(|| "running".into())
                                    } else {
                                        "running".into()
                                    }
                                } else if let Some(code) = script.exit_code {
                                    format!("exit {code}")
                                } else {
                                    script.lifecycle.clone()
                                },
                            )),
                    )
                    .when(running, |row| {
                        let row = row.child(action_button(
                            "script-restart",
                            "refresh-cw",
                            can_start,
                            &palette,
                            {
                                let name = name.clone();
                                cx.listener(move |this, _, _, cx| {
                                    this.emit(
                                        ScriptAction(name.clone(), ScriptActionKind::Restart),
                                        cx,
                                    );
                                })
                            },
                        ));
                        let row = row.child(action_button(
                            "script-stop",
                            "square",
                            can_start,
                            &palette,
                            {
                                let name = name.clone();
                                cx.listener(move |this, _, _, cx| {
                                    this.emit(
                                        ScriptAction(name.clone(), ScriptActionKind::Stop),
                                        cx,
                                    );
                                })
                            },
                        ));
                        if let Some(terminal_id) = script.terminal_id.clone() {
                            row.child(action_button(
                                "script-terminal",
                                "square-terminal",
                                true,
                                &palette,
                                cx.listener(move |this, _, _, cx| {
                                    this.emit(
                                        ScriptAction(
                                            terminal_id.clone(),
                                            ScriptActionKind::ViewTerminal,
                                        ),
                                        cx,
                                    );
                                }),
                            ))
                        } else {
                            row
                        }
                    })
                    .when(!running, |row| {
                        row.child(action_button(
                            "script-start",
                            "play",
                            can_start,
                            &palette,
                            cx.listener(move |this, _, _, cx| {
                                this.emit(ScriptAction(name.clone(), ScriptActionKind::Start), cx);
                            }),
                        ))
                    })
                    .children(targets.into_iter().map(|target| {
                        let url = target.url.clone();
                        let copy_url = target.url.clone();
                        let open_id = format!("script-open-{}:{}", script_id, target.label);
                        let copy_id = format!("script-copy-{}:{}", script_id, target.label);
                        div()
                            .id(gpui::SharedString::from(format!(
                                "script-target-{}:{}",
                                script_id, target.label
                            )))
                            .pl(px(26.))
                            .pr_3()
                            .pb_1()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .truncate()
                                    .text_color(rgb(palette.foreground_muted))
                                    .child(format!("{} · {}", target.heading(), target.label)),
                            )
                            .child(
                                div()
                                    .id(gpui::SharedString::from(open_id))
                                    .flex_shrink_0()
                                    .cursor_pointer()
                                    .text_color(rgb(palette.accent_bright))
                                    .child("Open")
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.emit(
                                                ScriptAction(
                                                    url.clone(),
                                                    ScriptActionKind::OpenUrl,
                                                ),
                                                cx,
                                            );
                                        }),
                                    ),
                            )
                            .child(
                                div()
                                    .id(gpui::SharedString::from(copy_id))
                                    .flex_shrink_0()
                                    .cursor_pointer()
                                    .text_color(rgb(palette.foreground_muted))
                                    .child("Copy")
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.emit(
                                                ScriptAction(
                                                    copy_url.clone(),
                                                    ScriptActionKind::CopyUrl,
                                                ),
                                                cx,
                                            );
                                        }),
                                    ),
                            )
                    }))
            }))
            .when(self.rows.is_empty() && self.error.is_none(), |menu| {
                menu.child(
                    div()
                        .px_3()
                        .py_2()
                        .text_color(rgb(palette.foreground_muted))
                        .child("No scripts are configured for this workspace."),
                )
            })
    }
}

fn action_button(
    id: &'static str,
    glyph: &'static str,
    enabled: bool,
    palette: &Palette,
    listener: impl Fn(&gpui::MouseDownEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> gpui::AnyElement {
    div()
        .id(id)
        .size(px(24.))
        .rounded(px(radius::SM))
        .flex()
        .items_center()
        .justify_center()
        .when(enabled, |button| {
            button
                .cursor_pointer()
                .hover(|button| button.bg(rgb(palette.surface3)))
        })
        .when(!enabled, |button| button.opacity(0.5))
        .child(icon(glyph, 14.).text_color(rgb(palette.foreground_muted)))
        .on_mouse_down(MouseButton::Left, move |event, window, cx| {
            cx.stop_propagation();
            if enabled {
                listener(event, window, cx);
            }
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn checkout_errors_read_the_message_field_of_the_error_object() {
        // messages.ts: CheckoutErrorSchema = {code, message}; as_str() saw none
        // and every host refusal showed as "No changes." before.
        let payload = json!({"error":{"code":"MERGE_CONFLICT","message":"Rebase in progress"}});
        assert_eq!(
            checkout_error_message(&payload).as_deref(),
            Some("Rebase in progress")
        );
        let plain = json!({"error":"legacy string"});
        assert_eq!(
            checkout_error_message(&plain).as_deref(),
            Some("legacy string")
        );
        assert_eq!(checkout_error_message(&json!({"error":null})), None);
    }

    #[test]
    fn diff_files_flag_per_file_too_large_status() {
        let file = parse_diff_file(&json!({
            "path":"big.rs","isNew":false,"isDeleted":false,
            "additions":0,"deletions":0,"status":"too_large","hunks":[]
        }))
        .unwrap();
        assert!(file.too_large);
        assert!(!file.binary);
    }

    #[test]
    fn restarts_fire_once_the_stop_is_actually_acknowledged() {
        // status update arrives while the stop reply is still outstanding:
        // the start must wait, or script_pending swallows it silently.
        assert!(!should_start_restart(Some("web"), "web", Some("web")));
        // stop reply cleared the pending marker first: fire.
        assert!(should_start_restart(Some("web"), "web", None));
        // a different script's start in flight does not block this restart.
        assert!(should_start_restart(Some("web"), "web", Some("db")));
        // nothing armed: never start.
        assert!(!should_start_restart(None, "web", None));
    }

    #[test]
    fn a_late_script_reply_only_clears_its_own_marker() {
        // Stop B was sent, then a restart of A fired its own start: B's late
        // reply must not clear A's marker and open a second concurrent op.
        let mut pending = Some("a".to_owned());
        clear_script_pending(&mut pending, "b");
        assert_eq!(pending.as_deref(), Some("a"));
        clear_script_pending(&mut pending, "a");
        assert_eq!(pending, None);
        clear_script_pending(&mut pending, "a");
        assert_eq!(pending, None);
    }

    #[test]
    fn parent_of_roots_the_top_level() {
        assert_eq!(parent_of("src"), ".");
        assert_eq!(parent_of("src/lib.rs"), "src");
        assert_eq!(parent_of("a/b/c.txt"), "a/b");
    }

    #[test]
    fn directory_parsing_keeps_entry_facts() {
        let directory = parse_directory(&json!({
            "path": "src",
            "entries": [
                {"name":"lib.rs","path":"src/lib.rs","kind":"file","size":12,"modifiedAt":"t1"},
                {"name":"bin","path":"src/bin","kind":"directory","size":0,"modifiedAt":"t2"}
            ]
        }));
        assert_eq!(directory.entries.len(), 2);
        assert_eq!(directory.entries[0].path, "src/lib.rs");
        assert_eq!(directory.entries[1].kind, EntryKind::Directory);
    }

    #[test]
    fn diff_parsing_maps_lines_and_flags() {
        let file = parse_diff_file(&json!({
            "path": "a.rs",
            "isNew": true,
            "isDeleted": false,
            "additions": 2,
            "deletions": 1,
            "hunks": [{
                "oldStart": 1, "oldCount": 1, "newStart": 1, "newCount": 2,
                "lines": [
                    {"type":"header","content":"..."},
                    {"type":"remove","content":"old"},
                    {"type":"add","content":"new"},
                    {"type":"context","content":"keep"}
                ]
            }]
        }))
        .unwrap();
        assert!(file.is_new);
        assert_eq!(badge(&file), "A");
        assert_eq!(file.hunks[0].lines.len(), 3);
        assert_eq!(file.hunks[0].lines[0], (LineKind::Remove, "old".to_owned()));
        assert_eq!(file.hunks[0].lines[1], (LineKind::Add, "new".to_owned()));
    }

    #[test]
    fn deleted_files_badge_differently_from_modifications() {
        let deleted = parse_diff_file(&json!({
            "path":"b.rs","isNew":false,"isDeleted":true,
            "additions":0,"deletions":9,"hunks":[]
        }))
        .unwrap();
        assert_eq!(badge(&deleted), "D");
        let modified = parse_diff_file(&json!({
            "path":"c.rs","isNew":false,"isDeleted":false,
            "additions":1,"deletions":1,"hunks":[]
        }))
        .unwrap();
        assert_eq!(badge(&modified), "M");
    }

    #[test]
    fn status_parsing_reads_branch_and_ahead_behind() {
        let status = parse_status(&json!({
            "isGit": true,
            "repoRoot": "/repo",
            "currentBranch": "main",
            "isDirty": true,
            "aheadOfOrigin": 2,
            "behindOfOrigin": 0
        }));
        assert!(status.is_git);
        assert_eq!(status.current_branch.as_deref(), Some("main"));
        assert_eq!(status.ahead, Some(2));
        assert_eq!(status.behind, Some(0));
    }

    #[test]
    fn script_parsing_and_tones_match_the_original_semantics() {
        let running_service = parse_script(&json!({
            "scriptName":"web","type":"service","lifecycle":"running","health":"healthy"
        }))
        .unwrap();
        assert_eq!(running_service.tone(), ScriptTone::Healthy);
        let failing = parse_script(&json!({
            "scriptName":"web","type":"service","lifecycle":"running","health":"unhealthy"
        }))
        .unwrap();
        assert_eq!(failing.tone(), ScriptTone::Unhealthy);
        let plain = parse_script(&json!({
            "scriptName":"watch","type":"script","lifecycle":"running"
        }))
        .unwrap();
        assert_eq!(plain.tone(), ScriptTone::Running);
        let stopped = parse_script(&json!({
            "scriptName":"done","lifecycle":"exited","exitCode":3
        }))
        .unwrap();
        assert_eq!(stopped.tone(), ScriptTone::Idle);
        assert_eq!(stopped.exit_code, Some(3));
        let dark = paseo_desktop::appearance::resolve(
            paseo_desktop::settings::ThemePreference::Dark,
            true,
        );
        let light = paseo_desktop::appearance::resolve(
            paseo_desktop::settings::ThemePreference::Light,
            false,
        );
        assert_eq!(ScriptTone::Healthy.color(&dark), dark.status_dot_success);
        assert_eq!(ScriptTone::Unhealthy.color(&light), light.status_dot_danger);
    }

    #[test]
    fn workspace_scripts_extract_from_a_descriptor_payload() {
        let scripts = parse_scripts(&json!({"id":"w1","scripts":[
            {"scriptName":"web","type":"service","lifecycle":"running","localProxyUrl":"http://x"}
        ]}));
        assert_eq!(scripts.len(), 1);
        assert_eq!(scripts[0].name, "web");
        assert_eq!(scripts[0].local_url.as_deref(), Some("http://x"));
    }

    #[test]
    fn service_targets_are_public_paseo_then_direct() {
        let script = parse_script(&json!({
            "scriptName":"web","type":"service","lifecycle":"running",
            "publicProxyUrl":"https://web.example.com/",
            "localProxyUrl":"http://127.0.0.1:4000",
            "port":3000
        }))
        .unwrap();
        let targets = script_targets(&script, "127.0.0.1", true);
        assert_eq!(targets.len(), 3);
        assert_eq!(targets[0].kind, ScriptLinkKind::Public);
        assert_eq!(targets[0].label, "web.example.com");
        assert_eq!(targets[1].kind, ScriptLinkKind::Paseo);
        assert_eq!(targets[2].kind, ScriptLinkKind::Direct);
        // A loopback daemon host is named localhost, like the original app.
        assert_eq!(targets[2].url, "http://localhost:3000");
    }

    #[test]
    fn legacy_proxy_url_is_classified_by_reachability_and_locality() {
        let local = parse_script(&json!({
            "scriptName":"app","type":"service","lifecycle":"running",
            "proxyUrl":"http://localhost:8080"
        }))
        .unwrap();
        let targets = script_targets(&local, "127.0.0.1", true);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].kind, ScriptLinkKind::Paseo);

        let public = parse_script(&json!({
            "scriptName":"app","type":"service","lifecycle":"running",
            "proxyUrl":"https://app.tunnel.dev"
        }))
        .unwrap();
        let targets = script_targets(&public, "10.0.0.5", false);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].kind, ScriptLinkKind::Public);
    }

    #[test]
    fn a_remote_connection_never_offers_paseo_loopback_urls() {
        let script = parse_script(&json!({
            "scriptName":"web","type":"service","lifecycle":"running",
            "publicProxyUrl":"https://web.example.com/",
            "localProxyUrl":"http://web--project.localhost:7000",
            "port":3000
        }))
        .unwrap();
        // The daemon on 10.0.0.5 generates loopback proxy hostnames that only
        // resolve there; opening them on this machine hits the wrong host.
        let targets = script_targets(&script, "10.0.0.5", false);
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].kind, ScriptLinkKind::Public);
        assert_eq!(targets[1].kind, ScriptLinkKind::Direct);
        assert_eq!(targets[1].url, "http://10.0.0.5:3000");
        // The same daemon reached locally keeps its Paseo target.
        let local = script_targets(&script, "127.0.0.1", true);
        assert!(
            local
                .iter()
                .any(|target| target.kind == ScriptLinkKind::Paseo)
        );
    }

    #[test]
    fn a_stopped_service_has_no_target_and_direct_url_uses_the_daemon_host() {
        let stopped = parse_script(&json!({
            "scriptName":"web","type":"service","lifecycle":"stopped","port":3000
        }))
        .unwrap();
        assert!(script_targets(&stopped, "localhost", true).is_empty());

        let running = parse_script(&json!({
            "scriptName":"web","type":"service","lifecycle":"running","port":3000
        }))
        .unwrap();
        let targets = script_targets(&running, "dev.example.net", false);
        assert_eq!(targets[0].url, "http://dev.example.net:3000");
    }

    #[test]
    fn search_hits_split_name_from_directory() {
        let hits = parse_search_hits(&json!({"entries":[
            {"path":"src/app/main.rs","kind":"file"},
            {"path":"README.md","kind":"file"}
        ]}));
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].name, "main.rs");
        assert_eq!(hits[0].directory, "src/app");
        assert_eq!(hits[1].name, "README.md");
        assert!(hits[1].directory.is_empty());
    }

    #[test]
    fn pr_status_reads_checks_and_repository_merge_policy() {
        let dark = paseo_desktop::appearance::resolve(
            paseo_desktop::settings::ThemePreference::Dark,
            true,
        );
        let pr = parse_pr_status(&json!({
            "forge":"github",
            "status":{
                "number":12,"url":"https://x/12","title":"Ship it","state":"OPEN",
                "isDraft":false,"isMerged":false,"mergeable":"MERGEABLE",
                "baseRefName":"main","repoOwner":"o","repoName":"r",
                "checksStatus":"SUCCESS",
                "checks":[
                    {"name":"ci","status":"success","url":"https://x/1","checkRunId":7},
                    {"name":"lint","status":"failure","url":null}
                ],
                "github":{
                    "mergeStateStatus":"CLEAN",
                    "repository":{"squashMergeAllowed":true,"mergeCommitAllowed":false,"rebaseMergeAllowed":false}
                }
            }
        }))
        .unwrap();
        assert_eq!(pr.number, Some(12));
        assert_eq!(pr.base_ref, "main");
        assert_eq!(pr.checks.len(), 2);
        assert_eq!(pr.checks[0].check_run_id, Some(7));
        assert_eq!(pr.checks[1].tone(&dark), dark.status_dot_danger);
        // Status colours are identity as well as paint: every comparison uses
        // the same palette the row is drawn with.
        let light = paseo_desktop::appearance::resolve(
            paseo_desktop::settings::ThemePreference::Light,
            false,
        );
        assert_eq!(pr.checks[0].tone(&light), light.status_dot_success);
        assert_eq!(pr.checks_tone(&light), light.status_dot_danger);
        assert_eq!(pr.checks_tone(&dark), dark.status_dot_danger);
        assert_ne!(light.status_dot_danger, dark.status_dot_danger);
        // Only the policy-allowed method is offered.
        assert_eq!(pr.merge_methods, vec![MergeMethod::Squash]);
        assert!(pr.can_merge());
        assert_eq!(pr.checks_summary(&dark), "success");
    }

    #[test]
    fn merge_is_withheld_for_drafts_conflicts_and_merged_requests() {
        let base = |extra: Value| {
            let mut status = json!({
                "state":"OPEN","isDraft":false,"isMerged":false,
                "mergeable":"MERGEABLE",
                "github":{
                    "mergeStateStatus":"CLEAN",
                    "repository":{"squashMergeAllowed":true}
                }
            });
            for (key, value) in extra.as_object().unwrap() {
                status[key] = value.clone();
            }
            parse_pr_status(&json!({"status":status})).unwrap()
        };
        assert!(base(json!({})).can_merge());
        assert!(!base(json!({"isDraft":true})).can_merge());
        assert!(!base(json!({"isMerged":true})).can_merge());
        assert!(!base(json!({"mergeable":"CONFLICTING"})).can_merge());
        assert!(!base(json!({"state":"CLOSED"})).can_merge());
        // A repository that allows no method cannot be merged from here.
        assert!(!base(json!({"github":{"mergeStateStatus":"CLEAN","repository":{"squashMergeAllowed":false}}})).can_merge());
        // No policy facts at all falls back to git state with every method.
        let unfactored = base(json!({"github":{"mergeStateStatus":"CLEAN"}}));
        assert_eq!(unfactored.merge_methods.len(), 3);
        assert!(unfactored.can_merge());
        assert_eq!(
            pr_merge_blocker(&base(json!({"isDraft":true}))),
            "The change request is still a draft."
        );
    }

    #[test]
    fn github_facts_withhold_merge_until_the_forge_reports_it_ready() {
        let facts = |github: Value| github_facts(&github, &json!({"mergeable":"MERGEABLE"}));
        let ready = facts(json!({
            "mergeStateStatus":"CLEAN",
            "repository":{"squashMergeAllowed":true}
        }));
        assert!(ready.direct_merge_blocker.is_none());

        // MERGEABLE alone is not permission: the daemon refuses BLOCKED states.
        for blocked in ["BLOCKED", "DIRTY", "UNKNOWN"] {
            let withheld = facts(json!({
                "mergeStateStatus":blocked,
                "repository":{"squashMergeAllowed":true}
            }));
            assert!(withheld.direct_merge_blocker.is_some(), "{blocked}");
        }
        // A merge queue or a scheduled auto-merge blocks a direct merge.
        let queue = facts(json!({
            "mergeStateStatus":"CLEAN","isMergeQueueEnabled":true,
            "repository":{"squashMergeAllowed":true}
        }));
        assert!(queue.direct_merge_blocker.is_some());
        let scheduled = facts(json!({
            "mergeStateStatus":"CLEAN","autoMergeRequest":{"enabledAt":"t","mergeMethod":"SQUASH","enabledBy":"o"},
            "viewerCanDisableAutoMerge":true,
            "repository":{"squashMergeAllowed":true}
        }));
        assert!(scheduled.direct_merge_blocker.is_some());
        assert!(scheduled.auto_merge_enabled);
        assert!(scheduled.can_disable_auto_merge);
    }

    #[test]
    fn github_auto_merge_enable_checks_its_own_capability_facts() {
        let facts = |github: Value| github_facts(&github, &json!({"mergeable":"MERGEABLE"}));
        let allowed = facts(json!({
            "mergeStateStatus":"BLOCKED","viewerCanEnableAutoMerge":true,
            "repository":{"squashMergeAllowed":true,"autoMergeAllowed":true}
        }));
        assert!(allowed.auto_merge_enable_blocker.is_none());
        // Each fact independently withholds the action.
        assert!(
            facts(json!({
                "mergeStateStatus":"BLOCKED","viewerCanEnableAutoMerge":false,
                "repository":{"squashMergeAllowed":true,"autoMergeAllowed":true}
            }))
            .auto_merge_enable_blocker
            .is_some()
        );
        assert!(
            facts(json!({
                "mergeStateStatus":"BLOCKED","viewerCanEnableAutoMerge":true,
                "repository":{"squashMergeAllowed":true,"autoMergeAllowed":false}
            }))
            .auto_merge_enable_blocker
            .is_some()
        );
        assert!(
            facts(json!({
                "mergeStateStatus":"CLEAN","viewerCanEnableAutoMerge":true,
                "repository":{"squashMergeAllowed":true,"autoMergeAllowed":true}
            }))
            .auto_merge_enable_blocker
            .is_some()
        );
    }

    #[test]
    fn gitlab_facts_drive_direct_merge_and_scheduled_auto_merge() {
        let facts = |facts: Value| gitlab_facts(&facts);
        let ready = facts(json!({
            "forge":"gitlab","detailedMergeStatus":"mergeable","hasConflicts":false,
            "mergeWhenPipelineSucceeds":false,"pipelineStatus":"running"
        }));
        assert!(ready.direct_merge_blocker.is_none());
        assert!(ready.auto_merge_enable_blocker.is_none());
        assert!(!ready.auto_merge_enabled);

        // A scheduled merge-when-pipeline-succeeds is GitLab's auto-merge and
        // must be cancellable, while direct merge is withheld.
        let scheduled = facts(json!({
            "forge":"gitlab","detailedMergeStatus":"mergeable","hasConflicts":false,
            "mergeWhenPipelineSucceeds":true,"pipelineStatus":"running"
        }));
        assert!(scheduled.auto_merge_enabled);
        assert!(scheduled.can_disable_auto_merge);
        assert!(scheduled.direct_merge_blocker.is_some());

        // Enabling needs an in-progress pipeline or glab merges immediately.
        let no_pipeline = facts(json!({
            "forge":"gitlab","detailedMergeStatus":"mergeable","hasConflicts":false,
            "mergeWhenPipelineSucceeds":false,"pipelineStatus":"success"
        }));
        assert!(no_pipeline.auto_merge_enable_blocker.is_some());

        // Legacy self-managed GitLab only reports merge_status.
        let legacy = facts(json!({
            "forge":"gitlab","detailedMergeStatus":null,"mergeStatus":"can_be_merged",
            "hasConflicts":false,"mergeWhenPipelineSucceeds":false
        }));
        assert!(legacy.direct_merge_blocker.is_none());
    }

    #[test]
    fn pr_status_reads_the_facts_envelope_end_to_end() {
        let gitlab = parse_pr_status(&json!({
            "forge":"gitlab",
            "status":{
                "state":"OPEN","isDraft":false,"isMerged":false,"mergeable":"MERGEABLE",
                "url":"https://gitlab/x","title":"MR","baseRefName":"main",
                "forgeSpecific":{
                    "forge":"gitlab","detailedMergeStatus":"mergeable","hasConflicts":false,
                    "mergeWhenPipelineSucceeds":true,"pipelineStatus":"running"
                }
            }
        }))
        .unwrap();
        // The scheduled auto-merge is the one cancellable action; a direct merge
        // would be refused by the daemon while it is armed.
        assert!(!gitlab.can_merge());
        assert!(gitlab.auto_merge_enabled);
        assert!(gitlab.can_disable_auto_merge);
        assert_eq!(
            pr_merge_blocker(&gitlab),
            "Auto-merge is already scheduled for this change request."
        );

        let github = parse_pr_status(&json!({
            "status":{
                "state":"OPEN","isDraft":false,"isMerged":false,"mergeable":"MERGEABLE",
                "url":"https://github/x","title":"PR","baseRefName":"main",
                "forgeSpecific":{
                    "forge":"github","mergeStateStatus":"BLOCKED","autoMergeRequest":null,
                    "viewerCanEnableAutoMerge":true,
                    "repository":{"squashMergeAllowed":true,"autoMergeAllowed":true}
                }
            }
        }))
        .unwrap();
        assert!(!github.can_merge());
        assert!(github.auto_merge_enable_blocker.is_none());
    }

    #[test]
    fn gitea_and_unknown_families_do_not_borrow_github_answers() {
        let gitea = gitea_facts(&json!({"forge":"gitea","mergeable":true,"hasMerged":false}));
        assert!(gitea.direct_merge_blocker.is_none());
        // The daemon's Gitea adapter has no auto-merge at all.
        assert!(gitea.auto_merge_enable_blocker.is_some());
        assert!(!gitea.auto_merge_enabled);

        let unknown = forge_facts(&json!({
            "forgeSpecific":{"forge":"bitbucket","ready":true},
            // Even a populated GitHub mirror must not answer for another family.
            "github":{"mergeStateStatus":"CLEAN","repository":{"squashMergeAllowed":true}}
        }));
        assert!(unknown.direct_merge_blocker.is_some());
        assert!(unknown.merge_methods.is_empty());
        assert!(unknown.auto_merge_enable_blocker.is_some());
    }

    #[test]
    fn the_github_mirror_is_read_only_without_a_facts_envelope() {
        let legacy = forge_facts(&json!({
            "github":{"mergeStateStatus":"CLEAN","repository":{"squashMergeAllowed":true}}
        }));
        assert!(legacy.direct_merge_blocker.is_none());
        assert_eq!(legacy.merge_methods, vec![MergeMethod::Squash]);

        let missing = forge_facts(&json!({}));
        assert!(missing.direct_merge_blocker.is_some());
        assert!(missing.merge_methods.is_empty());
    }

    #[test]
    fn check_actions_keep_details_and_the_browser_independent() {
        let check = |url: Option<&str>, check_run_id: Option<i64>| PrCheck {
            name: "ci".into(),
            status: "success".into(),
            url: url.map(str::to_owned),
            check_run_id,
            workflow_run_id: None,
        };
        // A run id no longer hides behind the URL.
        assert_eq!(
            check_actions(&check(Some("https://x/1"), Some(7)), true),
            CheckActions::Both
        );
        assert_eq!(
            check_actions(&check(Some("https://x/1"), Some(7)), false),
            CheckActions::OpenUrl
        );
        assert_eq!(
            check_actions(&check(None, Some(7)), true),
            CheckActions::Details
        );
        assert_eq!(
            check_actions(&check(Some("https://x/1"), None), true),
            CheckActions::OpenUrl
        );
        assert_eq!(
            check_actions(&check(None, None), true),
            CheckActions::Neither
        );
    }

    #[test]
    fn dispatch_is_owned_by_the_pending_request_not_by_a_kind_list() {
        let mut pending = BTreeMap::new();
        pending.insert("req-1".to_owned(), Pending::Branches);
        let owned =
            |kind: &str, payload: Value| owns_message(kind, &payload, "w1", "/repo", &pending);
        // A response kind this module sends today, and one it does not send yet:
        // both reach receive because the request id is ours.
        assert!(owned(
            "branch_suggestions_response",
            json!({"requestId":"req-1"})
        ));
        assert!(owned("some.new.response", json!({"requestId":"req-1"})));
        assert!(!owned(
            "branch_suggestions_response",
            json!({"requestId":"other"})
        ));
        assert!(!owned("some.new.response", json!({})));
        // Pushes belong to the surface only for its own workspace and cwd.
        assert!(owned("script_status_update", json!({"workspaceId":"w1"})));
        assert!(!owned("script_status_update", json!({"workspaceId":"w2"})));
        assert!(owned("checkout_status_update", json!({"cwd":"/repo"})));
        assert!(!owned("checkout_status_update", json!({"cwd":"/other"})));
    }

    #[test]
    fn search_replies_are_matched_by_generation() {
        let mut search = Search::default();
        search.generation = 2;
        search.loading = true;
        // The debounce armed in the previous workspace reaches a newer search.
        search.accept_reply(1, &json!({"entries":[{"path":"old.rs"}]}));
        assert!(search.loading);
        assert!(search.hits.is_empty());
        search.accept_reply(2, &json!({"entries":[{"path":"new.rs"}]}));
        assert!(!search.loading);
        assert_eq!(search.hits.len(), 1);
    }

    #[test]
    fn a_new_workspace_never_reuses_a_search_generation() {
        let mut search = Search::default();
        search.generation = 4;
        search.hits.push(SearchHit {
            path: "a.rs".into(),
            name: "a.rs".into(),
            directory: String::new(),
        });
        search.start_in_new_workspace();
        assert!(search.hits.is_empty());
        assert_eq!(search.generation, 5);
    }

    #[test]
    fn an_unanswered_action_is_not_retryable_and_only_its_own_reply_settles_it() {
        // A timeout leaves the row unknown, not idle: a second mutation whose
        // result also cannot be observed must not be sent.
        assert!(row_blocked(Some(OpState::Pending)));
        assert!(row_blocked(Some(OpState::Unknown)));
        assert!(!row_blocked(Some(OpState::Success)));
        assert!(!row_blocked(None));
        // A late reply for an older request cannot settle the newer request's row.
        assert!(settles_row(Some("new"), "new"));
        assert!(!settles_row(Some("new"), "old"));
        assert!(!settles_row(None, "old"));
    }

    #[test]
    fn service_url_actions_use_the_exact_url_the_menu_drew() {
        // Copy must not look the payload up as a script name and re-pick a target.
        assert_eq!(
            action_url("https://web.example.com/x?y=1"),
            Some("https://web.example.com/x?y=1")
        );
        assert_eq!(action_url(""), None);
    }

    #[test]
    fn stash_entries_and_check_details_parse() {
        let stashes = parse_stashes(&json!({"entries":[
            {"index":0,"message":" wip","branch":"main","isPaseo":true}
        ]}));
        assert_eq!(stashes.len(), 1);
        assert_eq!(stashes[0].index, 0);
        assert_eq!(stashes[0].branch.as_deref(), Some("main"));

        let details = parse_check_details(&json!({"details":{
            "name":"ci","conclusion":"failure","detailsUrl":"https://x/1",
            "failedJobs":[{"jobId":1,"name":"build"}]
        }}))
        .unwrap();
        assert_eq!(details.conclusion.as_deref(), Some("failure"));
        assert_eq!(details.failed_jobs, vec!["build".to_owned()]);
        assert_eq!(details.url.as_deref(), Some("https://x/1"));
        assert!(parse_check_details(&json!({"details":null})).is_none());
    }

    #[test]
    fn only_uncommitted_errors_offer_stash_and_switch() {
        assert!(should_offer_stash_switch(
            "Cannot switch branches with uncommitted changes"
        ));
        assert!(!should_offer_stash_switch("branch not found"));
    }

    #[test]
    fn feature_gates_are_read_once_from_the_hello_payload() {
        let features = BTreeMap::from([
            ("checkoutDiscardChanges".to_owned(), json!(true)),
            ("forgeProviders".to_owned(), json!(true)),
            ("forgeCheckDetails".to_owned(), json!(false)),
        ]);
        let gates = Features::read(&features);
        assert!(gates.discard_changes);
        assert!(gates.forge_providers);
        assert!(!gates.forge_check_details);
        assert!(!gates.forge_set_auto_merge);
    }

    #[test]
    fn visible_filter_hides_dotfiles_only_until_revealed() {
        let entries = vec![
            Entry {
                name: ".env".into(),
                path: ".env".into(),
                kind: EntryKind::File,
                size: 0,
                modified_at: String::new(),
            },
            Entry {
                name: "src".into(),
                path: "src".into(),
                kind: EntryKind::Directory,
                size: 0,
                modified_at: String::new(),
            },
        ];
        let tools_files = entries.iter().filter(|e| !e.name.starts_with('.')).count();
        assert_eq!(tools_files, 1);
    }
}

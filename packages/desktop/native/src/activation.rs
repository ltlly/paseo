//! Deep links, OS attention notifications and their activation decisions.
//!
//! Pure model only: parsing, suppression gates, pending-admission steps,
//! activation window routing and presence. The window client applies the steps
//! through GPUI's URL, input and notification APIs. Semantics follow the app's
//! `notification-open` model and the daemon's attention broadcast
//! (`agent_attention_required` / `terminal_attention_required`).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::{AgentExecution, Directory, Workspace};

/// A `paseo://h/<serverId>/agent/<agentId>` target.
///
/// Mirrors `@getpaseo/protocol/agent-deep-link` parse rules: exact scheme,
/// host `h`, no userinfo, port, query or fragment, and exactly three
/// non-empty path segments with `agent` in the middle. A link never names a
/// connection endpoint, so activating one can never connect to an arbitrary
/// host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentDeepLinkTarget {
    pub server_id: String,
    pub agent_id: String,
}

fn percent_decode(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' => {
                let hex = bytes.get(index + 1..index + 3)?;
                let high = (hex[0] as char).to_digit(16)?;
                let low = (hex[1] as char).to_digit(16)?;
                out.push(((high << 4) | low) as u8);
                index += 3;
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

fn deep_link_segment(raw: &str) -> Option<String> {
    let decoded = percent_decode(raw)?;
    let trimmed = decoded.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

pub fn parse_agent_deep_link(input: &str) -> Option<AgentDeepLinkTarget> {
    let rest = input.strip_prefix("paseo://h/")?;
    if rest.contains(['?', '#', '@']) {
        return None;
    }
    let segments: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
    if segments.len() != 3 || segments[1] != "agent" {
        return None;
    }
    let server_id = deep_link_segment(segments[0])?;
    let agent_id = deep_link_segment(segments[2])?;
    Some(AgentDeepLinkTarget {
        server_id,
        agent_id,
    })
}

/// First parseable deep link in launch arguments; binary paths and flags do
/// not parse as `paseo://` URLs.
pub fn parse_agent_deep_link_from_args(args: &[String]) -> Option<AgentDeepLinkTarget> {
    args.iter().find_map(|arg| parse_agent_deep_link(arg))
}

/// What an OS notification click or deep link should open. Terminal targets
/// keep the workspace hint from the payload; the pending-admission step resolves
/// the workspace from that hint, or from the directory when the payload omits
/// one, and never opens a terminal whose workspace is unavailable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ActivationTarget {
    Agent {
        server_id: String,
        agent_id: String,
    },
    Terminal {
        server_id: String,
        workspace_id: Option<String>,
        terminal_id: String,
    },
    Host {
        server_id: String,
    },
}

impl ActivationTarget {
    pub fn server_id(&self) -> &str {
        match self {
            Self::Agent { server_id, .. }
            | Self::Terminal { server_id, .. }
            | Self::Host { server_id } => server_id,
        }
    }
}

fn trimmed_field<'a>(data: &'a Value, key: &str) -> Option<&'a str> {
    data.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

/// Mirrors `parseNotificationOpenTarget`: terminal when both workspace and
/// terminal ids are present, agent on agent id, host as the remaining focus.
/// The server id is required here: a target with no host cannot be routed and
/// is not an activation at all.
pub fn parse_notification_target(data: &Value) -> Option<ActivationTarget> {
    if !data.is_object() {
        return None;
    }
    let server_id = trimmed_field(data, "serverId")?.to_owned();
    if let Some(target) = terminal_target(data, server_id.clone()) {
        return Some(target);
    }
    if let Some(agent_id) = trimmed_field(data, "agentId") {
        return Some(ActivationTarget::Agent {
            server_id,
            agent_id: agent_id.to_owned(),
        });
    }
    Some(ActivationTarget::Host { server_id })
}

/// A terminal target needs both ids; the wire contract makes `workspaceId`
/// optional, so a payload without one is not a terminal target.
fn terminal_target(data: &Value, server_id: String) -> Option<ActivationTarget> {
    let workspace_id = trimmed_field(data, "workspaceId")?;
    let terminal_id = trimmed_field(data, "terminalId")?;
    Some(ActivationTarget::Terminal {
        server_id,
        workspace_id: Some(workspace_id.to_owned()),
        terminal_id: terminal_id.to_owned(),
    })
}

/// Version of the notification tag envelope. Bump it when the payload shape
/// changes; a tag from another version is not trusted back into an activation.
const NOTIFICATION_TAG_VERSION: u32 = 1;

/// A notification tag as the OS returns it: the target to open, plus the window
/// that posted it so a click can return to the host that produced it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotificationTag {
    pub target: ActivationTarget,
    pub sender: Option<u64>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NotificationTagEnvelope {
    v: u32,
    target: ActivationTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sender: Option<u64>,
}

/// Notification tags carry the target through the OS: responses only echo the
/// tag back. A repeated tag also replaces the earlier notification where the
/// platform supports it, so the tag stays a pure function of its inputs.
pub fn notification_tag(target: &ActivationTarget, sender: Option<u64>) -> Option<String> {
    if !target_is_addressable(target) {
        return None;
    }
    let envelope = NotificationTagEnvelope {
        v: NOTIFICATION_TAG_VERSION,
        target: target.clone(),
        sender,
    };
    serde_json::to_string(&envelope).ok()
}

/// Parses a tag the OS echoed back. Tags are plain JSON, so this validates the
/// envelope version and the ids before an activation is built from one.
pub fn parse_notification_tag(tag: &str) -> Option<NotificationTag> {
    let envelope: NotificationTagEnvelope = serde_json::from_str(tag).ok()?;
    if envelope.v != NOTIFICATION_TAG_VERSION || !target_is_addressable(&envelope.target) {
        return None;
    }
    Some(NotificationTag {
        target: envelope.target,
        sender: envelope.sender,
    })
}

/// A target whose ids cannot address anything can never be admitted, so it is
/// rejected at both ends of the OS round trip instead of queued forever.
fn target_is_addressable(target: &ActivationTarget) -> bool {
    let present = |value: &str| !value.trim().is_empty();
    match target {
        ActivationTarget::Agent {
            server_id,
            agent_id,
        } => present(server_id) && present(agent_id),
        ActivationTarget::Terminal {
            server_id,
            terminal_id,
            ..
        } => present(server_id) && present(terminal_id),
        ActivationTarget::Host { server_id } => present(server_id),
    }
}

/// An OS notification ready to post.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttentionNotification {
    pub tag: String,
    pub title: String,
    pub body: String,
    /// Payload timestamp to record for this agent's dedup window; `None` when
    /// the daemon sent none.
    pub timestamp: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttentionDecision {
    Show(AttentionNotification),
    Suppress,
}

fn attention_title(reason: &str) -> &'static str {
    if reason == "permission" {
        "Agent needs permission"
    } else if reason == "error" {
        "Agent needs attention"
    } else {
        "Agent finished"
    }
}

fn attention_fallback_body(reason: &str) -> &'static str {
    if reason == "permission" {
        "Permission requested."
    } else if reason == "error" {
        "Encountered an error."
    } else {
        "Finished working."
    }
}

/// Gates a daemon `agent_attention_required` payload the way the app's
/// session context does: daemon `shouldNotify` first, then error suppression,
/// then focus suppression, then the per-agent timestamp dedup.
///
/// `focused_agent_id` is the session actually on screen, and `app_visible` is a
/// window the user can be looking at; the caller decides both. `admitted_server_id`
/// is the fallback host for payloads whose `notification.data` is absent.
pub fn agent_attention(
    payload: &Value,
    focused_agent_id: Option<&str>,
    app_visible: bool,
    last_notified_at: Option<&str>,
    admitted_server_id: &str,
    sender_window: u64,
) -> AttentionDecision {
    let Some(agent_id) = trimmed_field(payload, "agentId") else {
        return AttentionDecision::Suppress;
    };
    if payload.get("shouldNotify").and_then(Value::as_bool) != Some(true) {
        return AttentionDecision::Suppress;
    }
    let reason = payload
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or_default();
    // The app suppresses error reasons; errors surface in the sidebar.
    if reason == "error" {
        return AttentionDecision::Suppress;
    }
    if app_visible && focused_agent_id == Some(agent_id) {
        return AttentionDecision::Suppress;
    }
    let timestamp = trimmed_field(payload, "timestamp").map(str::to_owned);
    // Daemon-generated timestamps share one format, so string order matches
    // the app's epoch-milliseconds comparison.
    if let (Some(last), Some(current)) = (last_notified_at, timestamp.as_deref())
        && last >= current
    {
        return AttentionDecision::Suppress;
    }
    let notification = &payload["notification"];
    let title = trimmed_field(notification, "title")
        .map(str::to_owned)
        .unwrap_or_else(|| attention_title(reason).to_owned());
    let body = trimmed_field(notification, "body")
        .map(str::to_owned)
        .unwrap_or_else(|| attention_fallback_body(reason).to_owned());
    let Some(tag) = notification_tag(
        &ActivationTarget::Agent {
            server_id: trimmed_field(payload, "serverId")
                .or_else(|| {
                    payload
                        .pointer("/notification/data/serverId")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                })
                .unwrap_or(admitted_server_id)
                .to_owned(),
            agent_id: agent_id.to_owned(),
        },
        Some(sender_window),
    ) else {
        return AttentionDecision::Suppress;
    };
    AttentionDecision::Show(AttentionNotification {
        tag,
        title,
        body,
        timestamp,
    })
}

/// Gates a daemon `terminal_attention_required` payload. The daemon's
/// single-recipient plan already decided `shouldNotify`; the terminal routes
/// through its workspace, which the payload may omit.
///
/// `fallback_server_id` is the host this client has already admitted. The wire
/// contract makes `serverId` optional, and a notification is never a reason to
/// dial: without a host identity the notification is suppressed.
pub fn terminal_attention(
    payload: &Value,
    fallback_server_id: &str,
    sender_window: u64,
) -> AttentionDecision {
    if payload.get("shouldNotify").and_then(Value::as_bool) != Some(true) {
        return AttentionDecision::Suppress;
    }
    let Some(terminal_id) = trimmed_field(payload, "terminalId") else {
        return AttentionDecision::Suppress;
    };
    let server_id = trimmed_field(payload, "serverId").unwrap_or(fallback_server_id);
    if server_id.is_empty() {
        return AttentionDecision::Suppress;
    }
    let target = ActivationTarget::Terminal {
        server_id: server_id.to_owned(),
        workspace_id: trimmed_field(payload, "workspaceId").map(str::to_owned),
        terminal_id: terminal_id.to_owned(),
    };
    let Some(tag) = notification_tag(&target, Some(sender_window)) else {
        return AttentionDecision::Suppress;
    };
    AttentionDecision::Show(AttentionNotification {
        tag,
        title: trimmed_field(payload, "title")
            .unwrap_or("Terminal needs attention")
            .to_owned(),
        body: trimmed_field(payload, "body")
            .unwrap_or_default()
            .to_owned(),
        timestamp: None,
    })
}

/// Everything the pending-activation step needs to decide.
pub struct ActivationContext<'a> {
    /// Server id of the connected, ready host.
    pub connected_server_id: Option<&'a str>,
    /// Server id of an in-flight connect started for a named target.
    pub connecting_server_id: Option<&'a str>,
    /// A connection exists but has not named its host yet. A cold launch starts
    /// on the local daemon: the link must not be judged against an identity the
    /// handshake has not produced.
    pub identity_pending: bool,
    /// The connected host's directory pages are complete.
    pub directory_complete: bool,
    /// The workspace whose terminal inventory has arrived, if any.
    pub terminal_inventory_workspace: Option<&'a str>,
    /// The workspace this window shows.
    pub selected_workspace: Option<&'a str>,
    /// Saved host ids this client may switch to.
    pub saved_hosts: &'a [String],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivationStage {
    Pending,
    Connecting,
}

/// One activation waiting to be admitted. A target never mutates the wrong
/// host: routing happens only while the named host is the connected one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingActivation {
    target: ActivationTarget,
    stage: ActivationStage,
}

#[derive(Debug)]
pub enum ActivationStep {
    /// Open the built-in agent conversation (resolves to its current
    /// workspace); terminal-owned agents never take this step.
    OpenAgent {
        agent_id: String,
    },
    /// Switch workspace and keep the activation pending: a terminal is only
    /// admitted once its workspace's inventory can confirm it exists.
    SelectWorkspace {
        workspace_id: String,
    },
    OpenTerminal {
        workspace_id: String,
        terminal_id: String,
    },
    /// Focus without changing the workspace selection.
    FocusHost,
    ConnectHost {
        server_id: String,
    },
    Unavailable(String),
    Wait,
}

fn workspace_available(directory: &Directory, workspace_id: &str) -> bool {
    directory
        .workspaces
        .get(workspace_id)
        .is_some_and(Workspace::available)
}

impl PendingActivation {
    pub fn from_target(target: ActivationTarget) -> Self {
        Self {
            target,
            stage: ActivationStage::Pending,
        }
    }

    pub fn target(&self) -> &ActivationTarget {
        &self.target
    }

    pub fn begin_connecting(&mut self) {
        self.stage = ActivationStage::Connecting;
    }

    pub fn advance(&self, cx: ActivationContext, directory: &Directory) -> ActivationStep {
        let server_id = self.target.server_id();
        if cx.connected_server_id == Some(server_id) {
            return self.route(&cx, directory);
        }
        // An in-flight connect: for this target or started on its behalf.
        if self.stage == ActivationStage::Connecting || cx.connecting_server_id == Some(server_id) {
            return ActivationStep::Wait;
        }
        // The window is already connecting somewhere; which host it is stays
        // unknown until the handshake answers. Waiting is the only admission
        // that neither guesses nor dials a host the user did not choose.
        if cx.identity_pending {
            return ActivationStep::Wait;
        }
        if cx.saved_hosts.iter().any(|saved| saved == server_id) {
            return ActivationStep::ConnectHost {
                server_id: server_id.to_owned(),
            };
        }
        ActivationStep::Unavailable(
            "This link names a host this client has not saved. Save the host first.".into(),
        )
    }

    fn route(&self, cx: &ActivationContext, directory: &Directory) -> ActivationStep {
        match &self.target {
            ActivationTarget::Host { .. } => ActivationStep::FocusHost,
            ActivationTarget::Agent { agent_id, .. } => {
                let Some(agent) = directory.agents.get(agent_id) else {
                    return wait_or_unavailable(
                        cx.directory_complete,
                        "This agent is no longer available.",
                    );
                };
                if agent.archived_at.is_some() {
                    return ActivationStep::Unavailable(
                        "This agent is no longer available.".into(),
                    );
                }
                let Some(workspace_id) = agent
                    .workspace_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|id| !id.is_empty())
                else {
                    return ActivationStep::Unavailable("This agent has no workspace.".into());
                };
                if !workspace_available(directory, workspace_id) {
                    return ActivationStep::Unavailable(
                        "This agent's workspace is unavailable.".into(),
                    );
                }
                match &agent.execution {
                    // A built-in agent opens its conversation; a terminal-owned
                    // agent must open its terminal, never the conversation.
                    None => ActivationStep::OpenAgent {
                        agent_id: agent_id.clone(),
                    },
                    Some(AgentExecution::Terminal { terminal_id, .. }) => {
                        if terminal_id.trim().is_empty() {
                            return ActivationStep::Unavailable(
                                "Update this client to open this agent execution type.".into(),
                            );
                        }
                        ActivationStep::OpenTerminal {
                            workspace_id: workspace_id.to_owned(),
                            terminal_id: terminal_id.clone(),
                        }
                    }
                    Some(AgentExecution::Unknown) => ActivationStep::Unavailable(
                        "Update this client to open this agent execution type.".into(),
                    ),
                }
            }
            ActivationTarget::Terminal {
                workspace_id,
                terminal_id,
                ..
            } => {
                // The hint seeds the workspace; either id resolves against the
                // directory, whose terminal entries also cover terminal-owned
                // agents that have no shell inventory of their own.
                let resolved = workspace_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|id| !id.is_empty())
                    .map(str::to_owned)
                    .or_else(|| directory.terminal_workspace(terminal_id).map(str::to_owned));
                let Some(workspace_id) = resolved else {
                    return wait_or_unavailable(
                        cx.directory_complete,
                        "This terminal is no longer available.",
                    );
                };
                if !workspace_available(directory, &workspace_id) {
                    return ActivationStep::Unavailable(
                        "This terminal's workspace is unavailable.".into(),
                    );
                }
                if directory.terminal_workspace(terminal_id) == Some(workspace_id.as_str()) {
                    return ActivationStep::OpenTerminal {
                        workspace_id,
                        terminal_id: terminal_id.clone(),
                    };
                }
                // The terminal is not in the loaded inventory yet. Switching
                // workspace replaces the inventory, so the target waits for its
                // own snapshot instead of failing against the current one.
                if cx.selected_workspace != Some(workspace_id.as_str()) {
                    return ActivationStep::SelectWorkspace { workspace_id };
                }
                if cx.terminal_inventory_workspace != Some(workspace_id.as_str()) {
                    return ActivationStep::Wait;
                }
                ActivationStep::Unavailable("This terminal is no longer available.".into())
            }
        }
    }
}

fn wait_or_unavailable(directory_complete: bool, message: &str) -> ActivationStep {
    if directory_complete {
        ActivationStep::Unavailable(message.to_owned())
    } else {
        ActivationStep::Wait
    }
}

/// A dial that failed can never produce the identity a connecting target
/// waits for, so that activation ends now with an actionable message instead
/// of waiting forever. A target that never started dialing keeps waiting: its
/// admission is still decidable on this window.
pub fn connect_failure_outcome(pending: Option<&PendingActivation>) -> Option<&'static str> {
    pending
        .filter(|pending| pending.stage == ActivationStage::Connecting)
        .map(
            |_| "Couldn't reach the host this link points to. Reopen the link once the host is up.",
        )
}

/// One window's contribution to where an activation lands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActivationWindow {
    pub window_id: u64,
    pub focused: bool,
    pub visible: bool,
    /// Host this window has admitted, once its handshake completed.
    pub connected_server_id: Option<String>,
    /// This window may switch to the activation's host without saving it first.
    pub host_saved: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowDecision {
    Focus(usize),
    OpenPrimary,
}

/// Picks the window an activation lands in. A notification click goes back to
/// the window that posted it, because that window owns the host the target is
/// on. A link without a sender goes to a window already on the target's host,
/// then to one that may connect to it, then to focus and visibility. No branch
/// dials a host the window has not saved: that admission belongs to the target,
/// which reports the host as unavailable instead.
pub fn decide_activation_window(
    windows: &[ActivationWindow],
    target: &ActivationTarget,
    sender: Option<u64>,
) -> WindowDecision {
    let server_id = target.server_id();
    let pick = sender
        .and_then(|sender| windows.iter().position(|window| window.window_id == sender))
        .or_else(|| {
            windows
                .iter()
                .position(|window| window.connected_server_id.as_deref() == Some(server_id))
        })
        .or_else(|| windows.iter().position(|window| window.host_saved))
        .or_else(|| windows.iter().position(|window| window.focused))
        .or_else(|| windows.iter().position(|window| window.visible))
        .or_else(|| (!windows.is_empty()).then_some(0));
    pick.map_or(WindowDecision::OpenPrimary, WindowDecision::Focus)
}

/// A window is a place the user can be looking at only while it is both drawn
/// and focused. GPUI's visibility flag means the frame is on screen, which a
/// background window also satisfies.
pub fn window_watched(visible: bool, active: bool) -> bool {
    visible && active
}

/// The session actually on screen. A product pane or an editor tab covers the
/// session surface, so the selection behind it is not something the user is
/// watching and must not suppress that session's notification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DisplayedSession<'a> {
    pub agent_id: Option<&'a str>,
    pub terminal_id: Option<&'a str>,
}

pub fn displayed_session<'a>(
    product_pane_open: bool,
    file_open: bool,
    agent: Option<&'a str>,
    terminal: Option<&'a str>,
) -> DisplayedSession<'a> {
    if product_pane_open || file_open {
        return DisplayedSession {
            agent_id: None,
            terminal_id: None,
        };
    }
    DisplayedSession {
        agent_id: agent,
        terminal_id: terminal,
    }
}

/// macOS-style reopen: only a window-less app opens its primary window.
pub fn restore_when_activated(window_count: usize) -> bool {
    window_count == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn agent_update(agent: Value) -> Value {
        json!({"kind":"upsert","agent":agent})
    }

    fn directory_with(agents: &[Value], workspaces: &[Value]) -> Directory {
        let mut directory = Directory::default();
        for workspace in workspaces {
            directory
                .apply(
                    "workspace_update",
                    json!({"kind":"upsert","workspace":workspace}),
                )
                .unwrap();
        }
        for agent in agents {
            directory
                .apply("agent_update", agent_update(agent.clone()))
                .unwrap();
        }
        directory
    }

    fn context<'a>(
        connected: Option<&'a str>,
        connecting: Option<&'a str>,
        complete: bool,
        saved: &'a [String],
    ) -> ActivationContext<'a> {
        ActivationContext {
            connected_server_id: connected,
            connecting_server_id: connecting,
            identity_pending: false,
            directory_complete: complete,
            terminal_inventory_workspace: None,
            selected_workspace: None,
            saved_hosts: saved,
        }
    }

    #[test]
    fn deep_links_round_trip_encoded_segments_and_reject_other_routes() {
        let link = "paseo://h/server%2Fmain/agent/agent%20123";
        let target = parse_agent_deep_link(link).unwrap();
        assert_eq!(
            target,
            AgentDeepLinkTarget {
                server_id: "server/main".into(),
                agent_id: "agent 123".into()
            }
        );
        // The built link form parses back to the same target.
        let rebuilt = format!(
            "paseo://h/{}/agent/{}",
            percent_encode("server/main"),
            percent_encode("agent 123")
        );
        assert_eq!(parse_agent_deep_link(&rebuilt), Some(target));
        for rejected in [
            "https://h/server/agent/agent-1",
            "paseo://app/h/server/agent/agent-1",
            "paseo://user@h/server/agent/agent-1",
            "paseo://h:6767/server/agent/agent-1",
            "paseo://h/server/agent/agent-1?message=hello",
            "paseo://h/server/agent/agent-1#frag",
            "paseo://h/server/agent/agent-1/extra",
            "paseo://h/server/only-two",
            "paseo://h/server/wrong/agent-1",
            "paseo://h//agent/agent-1",
            "paseo://h/server%zz/agent/agent-1",
            "paseo://h/%20/agent/%20",
            "paseo://h/server/agent/%C3",
        ] {
            assert!(parse_agent_deep_link(rejected).is_none(), "{rejected}");
        }
    }

    fn percent_encode(value: &str) -> String {
        let mut out = String::new();
        for byte in value.bytes() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
                out.push(byte as char);
            } else {
                out.push_str(&format!("%{byte:02X}"));
            }
        }
        out
    }

    #[test]
    fn args_scan_finds_the_link_and_ignores_program_paths() {
        let args = [
            "/usr/bin/paseo-native".into(),
            "--flag".into(),
            "paseo://h/h1/agent/a1".into(),
        ];
        let target = parse_agent_deep_link_from_args(&args).unwrap();
        assert_eq!(target.server_id, "h1");
        assert_eq!(target.agent_id, "a1");
        assert!(parse_agent_deep_link_from_args(&["/usr/bin/paseo-native".into()]).is_none());
    }

    #[test]
    fn notification_targets_prefer_terminal_then_agent_then_host() {
        assert_eq!(
            parse_notification_target(&json!({
                "serverId":" s1 ","workspaceId":"w","terminalId":"t","agentId":"a"
            })),
            Some(ActivationTarget::Terminal {
                server_id: "s1".into(),
                workspace_id: Some("w".into()),
                terminal_id: "t".into()
            })
        );
        assert_eq!(
            parse_notification_target(&json!({"serverId":"s1","agentId":"a"})),
            Some(ActivationTarget::Agent {
                server_id: "s1".into(),
                agent_id: "a".into()
            })
        );
        assert_eq!(
            parse_notification_target(&json!({"serverId":"s1"})),
            Some(ActivationTarget::Host {
                server_id: "s1".into()
            })
        );
        for invalid in [json!(null), json!({}), json!({"terminalId":"t"})] {
            assert!(parse_notification_target(&invalid).is_none());
        }
    }

    #[test]
    fn tags_round_trip_and_encode_the_full_target() {
        let target = ActivationTarget::Terminal {
            server_id: "s1".into(),
            workspace_id: None,
            terminal_id: "t/1".into(),
        };
        let tag = notification_tag(&target, Some(7)).unwrap();
        assert_eq!(
            parse_notification_tag(&tag),
            Some(NotificationTag {
                target: target.clone(),
                sender: Some(7)
            })
        );
        // A tag without a sender is a link-shaped activation, not a window's.
        let link = notification_tag(&target, None).unwrap();
        assert_eq!(
            parse_notification_tag(&link).and_then(|tag| tag.sender),
            None
        );
        assert!(parse_notification_tag("not-json").is_none());
        // Tags are plain JSON from the OS: another version, or ids that address
        // nothing, never become an activation.
        for rejected in [
            r#"{"v":2,"target":{"kind":"agent","serverId":"s1","agentId":"a1"}}"#,
            r#"{"v":1,"target":{"kind":"agent","serverId":" ","agentId":"a1"}}"#,
            r#"{"v":1,"target":{"kind":"terminal","serverId":"s1","terminalId":""}}"#,
            r#"{"kind":"agent","serverId":"s1","agentId":"a1"}"#,
        ] {
            assert!(parse_notification_tag(rejected).is_none(), "{rejected}");
        }
        assert!(
            notification_tag(
                &ActivationTarget::Agent {
                    server_id: String::new(),
                    agent_id: "a1".into()
                },
                None
            )
            .is_none()
        );
    }

    #[test]
    fn agent_attention_suppresses_errors_unfocused_gates_and_repeats() {
        let payload = json!({
            "agentId":"a1",
            "reason":"finished",
            "timestamp":"2026-07-21T10:00:00.000Z",
            "shouldNotify":true,
            "notification":{"title":"Agent finished","body":"Done working","data":{"serverId":"s1","agentId":"a1","workspaceId":"w"}}
        });
        let decision = agent_attention(&payload, Some("a1"), true, None, "s1", 7);
        assert_eq!(decision, AttentionDecision::Suppress);

        let decision = agent_attention(
            &payload,
            None,
            true,
            Some("2026-07-21T10:00:00.000Z"),
            "s1",
            7,
        );
        assert_eq!(decision, AttentionDecision::Suppress);

        let shown = match agent_attention(&payload, Some("other"), true, None, "s1", 7) {
            AttentionDecision::Show(shown) => shown,
            other => panic!("{other:?}"),
        };
        assert_eq!(shown.title, "Agent finished");
        assert_eq!(shown.body, "Done working");
        assert_eq!(shown.timestamp.as_deref(), Some("2026-07-21T10:00:00.000Z"));
        assert_eq!(
            parse_notification_tag(&shown.tag),
            Some(NotificationTag {
                target: ActivationTarget::Agent {
                    server_id: "s1".into(),
                    agent_id: "a1".into()
                },
                sender: Some(7)
            })
        );

        let mut error = payload.clone();
        error["reason"] = json!("error");
        assert_eq!(
            agent_attention(&error, None, false, None, "s1", 7),
            AttentionDecision::Suppress
        );

        let mut off = payload;
        off["shouldNotify"] = json!(false);
        assert_eq!(
            agent_attention(&off, None, false, None, "s1", 7),
            AttentionDecision::Suppress
        );
    }

    #[test]
    fn agent_attention_without_a_payload_host_uses_the_admitted_host() {
        let payload = json!({"agentId":"a1","reason":"finished","shouldNotify":true});
        let shown = match agent_attention(&payload, None, false, None, "s9", 3) {
            AttentionDecision::Show(shown) => shown,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            parse_notification_tag(&shown.tag),
            Some(NotificationTag {
                target: ActivationTarget::Agent {
                    server_id: "s9".into(),
                    agent_id: "a1".into()
                },
                sender: Some(3)
            })
        );
        // Without any admitted identity the tag could not be routed, so nothing
        // is posted rather than a notification that cannot be opened.
        assert_eq!(
            agent_attention(&payload, None, false, None, "", 3),
            AttentionDecision::Suppress
        );
    }

    #[test]
    fn agent_attention_falls_back_to_reason_titles_without_a_daemon_payload() {
        let payload =
            json!({"agentId":"a1","reason":"permission","timestamp":"t2","shouldNotify":true});
        let shown = match agent_attention(&payload, None, false, Some("t1"), "s1", 7) {
            AttentionDecision::Show(shown) => shown,
            other => panic!("{other:?}"),
        };
        assert_eq!(shown.title, "Agent needs permission");
        assert_eq!(shown.body, "Permission requested.");
    }

    #[test]
    fn terminal_attention_requires_should_notify_and_terminal_id() {
        let payload = json!({
            "serverId":"s1","terminalId":"t1","workspaceId":"w",
            "title":"Bell","body":"Shell","shouldNotify":true
        });
        let shown = match terminal_attention(&payload, "s1", 7) {
            AttentionDecision::Show(shown) => shown,
            other => panic!("{other:?}"),
        };
        assert_eq!(shown.title, "Bell");
        assert_eq!(
            parse_notification_tag(&shown.tag),
            Some(NotificationTag {
                target: ActivationTarget::Terminal {
                    server_id: "s1".into(),
                    workspace_id: Some("w".into()),
                    terminal_id: "t1".into()
                },
                sender: Some(7)
            })
        );
        let mut off = payload;
        off["shouldNotify"] = json!(false);
        assert_eq!(
            terminal_attention(&off, "s1", 7),
            AttentionDecision::Suppress
        );
        assert_eq!(
            terminal_attention(&json!({"serverId":"s1","shouldNotify":true}), "s1", 7),
            AttentionDecision::Suppress
        );
    }

    #[test]
    fn terminal_attention_without_a_server_id_uses_the_admitted_host_only() {
        // `serverId` and `workspaceId` are optional on the wire: the session
        // already scopes the host, and the workspace resolves from the terminal.
        let payload = json!({
            "terminalId":"t1","title":"Bell","body":"Shell","shouldNotify":true
        });
        let shown = match terminal_attention(&payload, "s1", 7) {
            AttentionDecision::Show(shown) => shown,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            parse_notification_tag(&shown.tag),
            Some(NotificationTag {
                target: ActivationTarget::Terminal {
                    server_id: "s1".into(),
                    workspace_id: None,
                    terminal_id: "t1".into()
                },
                sender: Some(7)
            })
        );
        // No admitted identity means no host a click could route to.
        assert_eq!(
            terminal_attention(&payload, "", 7),
            AttentionDecision::Suppress
        );
    }

    #[test]
    fn agent_target_waits_for_the_directory_then_routes_execution_identity() {
        let built_in = json!({"id":"a1","provider":"claude","workspaceId":"w","status":"idle"});
        let terminal_agent = json!({"id":"a2","provider":"pi","workspaceId":"w","status":"idle","execution":{"kind":"terminal","terminalId":"pty"}});
        let workspace = json!({"id":"w","projectId":"p","projectDisplayName":"project","projectRootPath":"/repo","name":"main","status":"idle"});
        let directory = directory_with(&[built_in, terminal_agent], &[workspace]);

        let pending = PendingActivation::from_target(ActivationTarget::Agent {
            server_id: "s1".into(),
            agent_id: "a1".into(),
        });
        let saved = vec!["s1".to_owned()];
        assert!(matches!(
            pending.advance(context(Some("s1"), None, true, &saved), &directory),
            ActivationStep::OpenAgent { agent_id } if agent_id == "a1"
        ));

        let pending = PendingActivation::from_target(ActivationTarget::Agent {
            server_id: "s1".into(),
            agent_id: "a2".into(),
        });
        assert!(matches!(
            pending.advance(context(Some("s1"), None, true, &saved), &directory),
            ActivationStep::OpenTerminal { workspace_id, terminal_id }
                if workspace_id == "w" && terminal_id == "pty"
        ));
    }

    #[test]
    fn missing_or_archived_agents_and_unknown_executions_are_unavailable_not_waited() {
        let archived = json!({"id":"a1","provider":"pi","workspaceId":"w","status":"idle","archivedAt":"2026-01-01"});
        let unknown = json!({"id":"a2","provider":"future","workspaceId":"w","status":"idle","execution":{"kind":"remote-future","id":"x"}});
        let no_workspace = json!({"id":"a3","provider":"pi","status":"idle"});
        let workspace = json!({"id":"w","projectId":"p","projectDisplayName":"project","projectRootPath":"/repo","name":"main","status":"idle"});
        let directory = directory_with(&[archived, unknown, no_workspace], &[workspace]);
        let saved = vec!["s1".to_owned()];
        for (agent_id, expected) in [
            ("a1", "no longer available"),
            ("a2", "execution type"),
            ("a3", "no workspace"),
            ("missing", "no longer available"),
        ] {
            let pending = PendingActivation::from_target(ActivationTarget::Agent {
                server_id: "s1".into(),
                agent_id: agent_id.into(),
            });
            match pending.advance(context(Some("s1"), None, true, &saved), &directory) {
                ActivationStep::Unavailable(message) => {
                    assert!(message.contains(expected), "{agent_id}: {message}")
                }
                other => panic!("{agent_id}: {other:?}"),
            }
        }
        let pending = PendingActivation::from_target(ActivationTarget::Agent {
            server_id: "s1".into(),
            agent_id: "missing".into(),
        });
        assert!(matches!(
            pending.advance(context(Some("s1"), None, false, &saved), &directory),
            ActivationStep::Wait
        ));
    }

    #[test]
    fn terminal_target_uses_hint_then_directory_and_rejects_unavailable_workspaces() {
        let terminal = json!({"id":"pty","name":"Shell","workspaceId":"w"});
        let workspace = json!({"id":"w","projectId":"p","projectDisplayName":"project","projectRootPath":"/repo","name":"main","status":"idle"});
        let archived = json!({"id":"gone","projectId":"p","projectDisplayName":"project","projectRootPath":"/repo","name":"old","status":"idle","archivingAt":"now"});
        let mut directory = directory_with(&[], &[workspace]);
        directory
            .apply(
                "workspace_update",
                json!({"kind":"upsert","workspace":archived}),
            )
            .unwrap();
        directory
            .apply("list_terminals_response", json!({"terminals":[terminal]}))
            .unwrap();
        let saved = vec!["s1".to_owned()];
        let pending = PendingActivation::from_target(ActivationTarget::Terminal {
            server_id: "s1".into(),
            workspace_id: None,
            terminal_id: "pty".into(),
        });
        // Inventory resolves the terminal even while pages are incomplete.
        assert!(matches!(
            pending.advance(context(Some("s1"), None, false, &saved), &directory),
            ActivationStep::OpenTerminal { workspace_id, terminal_id }
                if workspace_id == "w" && terminal_id == "pty"
        ));
        // An unknown terminal with no hint waits until the directory completes.
        let pending = PendingActivation::from_target(ActivationTarget::Terminal {
            server_id: "s1".into(),
            workspace_id: None,
            terminal_id: "unlisted".into(),
        });
        assert!(matches!(
            pending.advance(context(Some("s1"), None, false, &saved), &directory),
            ActivationStep::Wait
        ));
        match pending.advance(context(Some("s1"), None, true, &saved), &directory) {
            ActivationStep::Unavailable(message) => {
                assert!(message.contains("no longer available"))
            }
            other => panic!("{other:?}"),
        }
        let pending = PendingActivation::from_target(ActivationTarget::Terminal {
            server_id: "s1".into(),
            workspace_id: Some("gone".into()),
            terminal_id: "pty".into(),
        });
        match pending.advance(context(Some("s1"), None, true, &saved), &directory) {
            ActivationStep::Unavailable(message) => assert!(message.contains("unavailable")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn cross_host_targets_wait_for_their_own_connect_and_never_guess_a_host() {
        let directory = Directory::default();
        let pending = PendingActivation::from_target(ActivationTarget::Host {
            server_id: "s2".into(),
        });
        let saved = vec!["s2".to_owned()];
        assert!(matches!(
            pending.advance(context(Some("s1"), None, true, &saved), &directory),
            ActivationStep::ConnectHost { server_id } if server_id == "s2"
        ));
        let mut connecting = pending.clone();
        connecting.begin_connecting();
        assert!(matches!(
            connecting.advance(context(Some("s1"), None, true, &saved), &directory),
            ActivationStep::Wait
        ));
        assert!(matches!(
            pending.advance(context(None, Some("s2"), true, &saved), &directory),
            ActivationStep::Wait
        ));
        let unsaved = vec!["s3".to_owned()];
        match pending.advance(context(Some("s1"), None, true, &unsaved), &directory) {
            ActivationStep::Unavailable(message) => assert!(message.contains("not saved")),
            other => panic!("{other:?}"),
        }
        // Connected to the named host: a host target only focuses.
        assert!(matches!(
            pending.advance(context(Some("s2"), None, false, &saved), &directory),
            ActivationStep::FocusHost
        ));
    }

    #[test]
    fn window_decision_prefers_the_sender_then_the_target_host_then_focus() {
        let windows = [
            window(1, Some("a"), false, true, false),
            window(2, Some("b"), false, true, false),
            window(3, None, false, false, false),
        ];
        // A notification click returns to the window that posted it, even when
        // another window is focused and connected elsewhere.
        let sender = ActivationTarget::Agent {
            server_id: "a".into(),
            agent_id: "x".into(),
        };
        assert_eq!(
            decide_activation_window(&windows, &sender, Some(1)),
            WindowDecision::Focus(0)
        );
        // A closed sender window falls back to the window already on the host.
        assert_eq!(
            decide_activation_window(&windows, &sender, Some(9)),
            WindowDecision::Focus(0)
        );
        // A link has no sender: it lands on the target's host, then on a window
        // that may connect to it, then on focus and visibility.
        let host_b = ActivationTarget::Host {
            server_id: "b".into(),
        };
        assert_eq!(
            decide_activation_window(&windows, &host_b, None),
            WindowDecision::Focus(1)
        );
        // Saved host, no window on it yet.
        let savable = [
            window(1, Some("a"), false, true, false),
            window(2, None, true, false, false),
        ];
        assert_eq!(
            decide_activation_window(&savable, &host_b, None),
            WindowDecision::Focus(1)
        );
        // Neither connected nor saved: focus decides.
        let focused = [
            window(1, Some("a"), false, false, true),
            window(2, None, false, true, false),
        ];
        assert_eq!(
            decide_activation_window(&focused, &host_b, None),
            WindowDecision::Focus(0)
        );
        // Nothing focused: the first visible window.
        let visible = [
            window(1, Some("a"), false, false, false),
            window(2, None, false, true, false),
        ];
        assert_eq!(
            decide_activation_window(&visible, &host_b, None),
            WindowDecision::Focus(1)
        );
        // No window can serve the host: the focused window still gets the
        // target, which then reports the host as unavailable instead of dialing.
        let unreachable = [window(1, Some("a"), false, true, true)];
        assert_eq!(
            decide_activation_window(&unreachable, &host_b, None),
            WindowDecision::Focus(0)
        );
        assert_eq!(
            decide_activation_window(&[], &host_b, None),
            WindowDecision::OpenPrimary
        );
        assert!(restore_when_activated(0));
        assert!(!restore_when_activated(1));
    }

    fn window(
        window_id: u64,
        connected: Option<&str>,
        host_saved: bool,
        visible: bool,
        focused: bool,
    ) -> ActivationWindow {
        ActivationWindow {
            window_id,
            focused,
            visible,
            connected_server_id: connected.map(str::to_owned),
            host_saved,
        }
    }

    #[test]
    fn cold_launch_link_waits_for_the_launch_connection_to_name_its_host() {
        let built_in = json!({"id":"a1","provider":"claude","workspaceId":"w","status":"idle"});
        let workspace = json!({"id":"w","projectId":"p","projectDisplayName":"project","projectRootPath":"/repo","name":"main","status":"idle"});
        let directory = directory_with(&[built_in], &[workspace]);
        let pending = PendingActivation::from_target(ActivationTarget::Agent {
            server_id: "s1".into(),
            agent_id: "a1".into(),
        });
        let saved: Vec<String> = Vec::new();
        // The default connection has started but has not learned its identity.
        let mut cold = context(None, None, false, &saved);
        cold.identity_pending = true;
        assert!(matches!(
            pending.advance(cold, &directory),
            ActivationStep::Wait
        ));
        // The handshake named the linked host: the link opens.
        assert!(matches!(
            pending.advance(context(Some("s1"), None, true, &saved), &directory),
            ActivationStep::OpenAgent { agent_id } if agent_id == "a1"
        ));
        // The handshake named a different host and the link's host is unsaved:
        // only now is the link honestly unavailable.
        match pending.advance(context(Some("s0"), None, true, &saved), &directory) {
            ActivationStep::Unavailable(message) => assert!(message.contains("not saved")),
            other => panic!("{other:?}"),
        }
        // The connection failed instead of naming a host: no identity is coming.
        match pending.advance(context(None, None, true, &saved), &directory) {
            ActivationStep::Unavailable(message) => assert!(message.contains("not saved")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn cross_workspace_terminal_activation_waits_for_that_workspaces_inventory() {
        let workspace_a = json!({"id":"a","projectId":"p","projectDisplayName":"project","projectRootPath":"/repo","name":"main","status":"idle"});
        let workspace_b = json!({"id":"b","projectId":"p","projectDisplayName":"project","projectRootPath":"/repo","name":"other","status":"idle"});
        let directory = directory_with(&[], &[workspace_a.clone(), workspace_b.clone()]);
        let pending = PendingActivation::from_target(ActivationTarget::Terminal {
            server_id: "s1".into(),
            workspace_id: Some("b".into()),
            terminal_id: "t1".into(),
        });
        let saved = vec!["s1".to_owned()];
        let mut showing_a = context(Some("s1"), None, true, &saved);
        showing_a.selected_workspace = Some("a");
        showing_a.terminal_inventory_workspace = Some("a");
        // `a`'s inventory cannot admit a terminal that lives in `b`: switch
        // workspace and keep the activation pending.
        assert!(matches!(
            pending.advance(showing_a, &directory),
            ActivationStep::SelectWorkspace { workspace_id } if workspace_id == "b"
        ));
        // After the switch the activation waits for `b`'s own snapshot.
        let mut switching = context(Some("s1"), None, true, &saved);
        switching.selected_workspace = Some("b");
        assert!(matches!(
            pending.advance(switching, &directory),
            ActivationStep::Wait
        ));
        // The snapshot lists the terminal: open it.
        let mut loaded = directory_with(&[], &[workspace_a, workspace_b]);
        loaded
            .apply(
                "list_terminals_response",
                json!({"terminals":[{"id":"t1","name":"Shell","workspaceId":"b"}]}),
            )
            .unwrap();
        let mut showing_b = context(Some("s1"), None, true, &saved);
        showing_b.selected_workspace = Some("b");
        showing_b.terminal_inventory_workspace = Some("b");
        assert!(matches!(
            pending.advance(showing_b, &loaded),
            ActivationStep::OpenTerminal { workspace_id, terminal_id }
                if workspace_id == "b" && terminal_id == "t1"
        ));
        // A snapshot without the terminal is the only evidence that consumes it.
        let mut shipping_nothing = context(Some("s1"), None, true, &saved);
        shipping_nothing.selected_workspace = Some("b");
        shipping_nothing.terminal_inventory_workspace = Some("b");
        match pending.advance(shipping_nothing, &directory) {
            ActivationStep::Unavailable(message) => {
                assert!(message.contains("no longer available"))
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn displayed_session_ignores_a_selection_a_cover_hides() {
        assert_eq!(
            displayed_session(false, false, Some("a1"), None),
            DisplayedSession {
                agent_id: Some("a1"),
                terminal_id: None
            }
        );
        // A product pane replaces the session surface; the selection behind it
        // is not on screen, so it must not suppress that session's notification.
        assert_eq!(
            displayed_session(true, false, Some("a1"), Some("t1")),
            DisplayedSession {
                agent_id: None,
                terminal_id: None
            }
        );
        // An editor tab covers the terminal pane the same way.
        assert_eq!(
            displayed_session(false, true, None, Some("t1")),
            DisplayedSession {
                agent_id: None,
                terminal_id: None
            }
        );
    }

    #[test]
    fn a_window_is_watched_only_when_it_is_visible_and_active() {
        // GPUI visibility means the frame is drawn; a background window draws too.
        assert!(window_watched(true, true));
        assert!(!window_watched(true, false));
        assert!(!window_watched(false, true));
    }

    #[test]
    fn a_failed_dial_ends_the_connecting_activation_instead_of_waiting_forever() {
        let mut dialing = PendingActivation::from_target(ActivationTarget::Host {
            server_id: "s2".into(),
        });
        dialing.begin_connecting();
        let outcome = connect_failure_outcome(Some(&dialing));
        assert!(outcome.is_some_and(|message| message.contains("Reopen the link")));
        // A target that was never dialing keeps waiting: nothing failed for it.
        assert_eq!(
            connect_failure_outcome(Some(&PendingActivation::from_target(
                ActivationTarget::Host {
                    server_id: "s2".into()
                }
            ))),
            None
        );
        assert_eq!(connect_failure_outcome(None), None);
    }
}

use std::collections::BTreeMap;

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    pub server_id: String,
    pub hostname: Option<String>,
    pub version: Option<String>,
    #[serde(default)]
    pub features: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub project_id: String,
    pub project_display_name: String,
    pub project_root_path: String,
    pub project_kind: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    pub id: String,
    pub project_id: String,
    pub project_display_name: String,
    pub project_root_path: String,
    pub workspace_directory: Option<String>,
    pub name: String,
    pub status: String,
    #[serde(default)]
    pub internal: bool,
    #[serde(default)]
    pub background: bool,
    pub archiving_at: Option<String>,
}
impl Workspace {
    pub fn available(&self) -> bool {
        !self.internal && !self.background && self.archiving_at.is_none()
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Terminal {
    pub id: String,
    pub name: String,
    pub workspace_id: Option<String>,
    pub title: Option<String>,
    pub cwd: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspacePage {
    pub entries: Vec<Workspace>,
    pub page_info: PageInfo,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageInfo {
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

#[derive(Debug, Deserialize)]
pub struct ProjectList {
    pub projects: Vec<Project>,
}

#[derive(Debug, Deserialize)]
pub struct TerminalList {
    pub terminals: Vec<Terminal>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum AgentExecution {
    #[serde(rename_all = "camelCase")]
    Terminal {
        terminal_id: String,
        icon: Option<String>,
        profile_id: Option<String>,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Agent {
    pub id: String,
    pub provider: String,
    pub workspace_id: Option<String>,
    pub title: Option<String>,
    pub status: String,
    pub execution: Option<AgentExecution>,
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    pub archived_at: Option<String>,
    #[serde(default)]
    pub internal: bool,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(default)]
    pub requires_attention: bool,
    pub attention_reason: Option<String>,
    #[serde(default)]
    pub pending_permissions: Vec<Value>,
    #[serde(default, deserialize_with = "deserialize_active_turn")]
    pub active_turn: Option<Value>,
}

fn deserialize_active_turn<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentStatusBucket {
    NeedsInput,
    Failed,
    Running,
    Attention,
    Done,
}

impl Agent {
    pub fn sidebar_status(&self) -> AgentStatusBucket {
        if !self.pending_permissions.is_empty()
            || self.attention_reason.as_deref() == Some("permission")
        {
            return AgentStatusBucket::NeedsInput;
        }
        let running = if self.execution.is_some() {
            self.status == "running"
        } else {
            self.is_working()
        };
        if self.attention_reason.as_deref() == Some("error") || (self.status == "error" && !running)
        {
            return AgentStatusBucket::Failed;
        }
        if running {
            return AgentStatusBucket::Running;
        }
        if self.requires_attention {
            return AgentStatusBucket::Attention;
        }
        AgentStatusBucket::Done
    }
    pub fn is_working(&self) -> bool {
        match &self.active_turn {
            Some(value) => !value.is_null(),
            None => self.status == "running", // Missing on older hosts, unlike explicit null.
        }
    }
    pub fn sidebar_icon(&self) -> &str {
        &self.provider
    }
    pub fn display_title(&self) -> &str {
        self.title
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .unwrap_or(&self.provider)
    }
    pub fn parent_agent_id(&self) -> Option<&str> {
        self.labels
            .get("paseo.parent-agent-id")
            .map(String::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
    }
}

#[derive(Debug, Deserialize)]
pub struct AgentEntry {
    pub agent: Agent,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentPage {
    pub entries: Vec<AgentEntry>,
    pub page_info: PageInfo,
}

/// fetch_agent_history_response: chronological agent history with cursor
/// pagination, the source for the native History panel.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryPage {
    pub entries: Vec<AgentEntry>,
    pub page_info: PageInfo,
    pub search_truncated: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum FocusedSession {
    Agent(String),
    Terminal(String),
}

#[derive(Default)]
pub struct DirectoryRefresh {
    agents: std::collections::BTreeSet<String>,
    workspaces: std::collections::BTreeSet<String>,
    removed_agents: std::collections::BTreeSet<String>,
    removed_workspaces: std::collections::BTreeSet<String>,
    agents_done: bool,
    workspaces_done: bool,
}

impl DirectoryRefresh {
    pub fn apply(&mut self, kind: &str, payload: &mut Value, directory: &mut Directory) {
        match kind {
            "agent_update" if !self.agents_done => {
                if payload["kind"] == "remove" {
                    if let Some(id) = payload["agentId"].as_str() {
                        self.removed_agents.insert(id.into());
                        self.agents.remove(id);
                    }
                } else if let Some(id) = payload["agent"]["id"].as_str() {
                    self.agents.insert(id.into());
                    self.removed_agents.remove(id);
                }
            }
            "workspace_update" if !self.workspaces_done => {
                if payload["kind"] == "remove" {
                    if let Some(id) = payload["workspaceId"].as_str() {
                        self.removed_workspaces.insert(id.into());
                        self.workspaces.remove(id);
                    }
                } else if let Some(id) = payload["workspace"]["id"].as_str() {
                    self.workspaces.insert(id.into());
                    self.removed_workspaces.remove(id);
                }
            }
            "fetch_agents_response" if !self.agents_done => {
                if let Some(entries) = payload["entries"].as_array_mut() {
                    entries.retain(|entry| {
                        entry["agent"]["id"]
                            .as_str()
                            .is_none_or(|id| !self.removed_agents.contains(id))
                    });
                    self.agents.extend(
                        entries
                            .iter()
                            .filter_map(|entry| entry["agent"]["id"].as_str().map(str::to_owned)),
                    );
                }
                if payload["pageInfo"]["hasMore"] == false {
                    self.agents_done = true;
                    directory.agents.retain(|id, _| self.agents.contains(id));
                }
            }
            "fetch_workspaces_response" if !self.workspaces_done => {
                if let Some(entries) = payload["entries"].as_array_mut() {
                    entries.retain(|entry| {
                        entry["id"]
                            .as_str()
                            .is_none_or(|id| !self.removed_workspaces.contains(id))
                    });
                    self.workspaces.extend(
                        entries
                            .iter()
                            .filter_map(|entry| entry["id"].as_str().map(str::to_owned)),
                    );
                }
                if payload["pageInfo"]["hasMore"] == false {
                    self.workspaces_done = true;
                    directory
                        .workspaces
                        .retain(|id, _| self.workspaces.contains(id));
                }
            }
            _ => {}
        }
    }
    pub fn complete(&self) -> bool {
        self.agents_done && self.workspaces_done
    }
}

#[derive(Default, Debug, Serialize)]
pub struct Directory {
    pub projects: BTreeMap<String, Project>,
    pub workspaces: BTreeMap<String, Workspace>,
    pub terminals: BTreeMap<String, Terminal>,
    pub agents: BTreeMap<String, Agent>,
    #[serde(skip)]
    last_focused: BTreeMap<String, FocusedSession>,
    #[serde(skip)]
    provider_icons: BTreeMap<String, Vec<u8>>,
    #[serde(skip)]
    server_id: Option<String>,
}

pub fn is_builtin_provider(provider: &str) -> bool {
    matches!(
        provider,
        "antigravity"
            | "claude"
            | "codex"
            | "copilot"
            | "kiro"
            | "minimax"
            | "omp"
            | "opencode"
            | "pi"
    )
}

// Host provider SVGs are validated by packages/plugin/src/icons.ts before publishing.
// Keep the same self-contained subset and bound the client copy before GPUI parses it.
fn validate_provider_svg(svg: &str) -> Result<()> {
    ensure!(svg.len() <= 64 * 1024, "Provider icon exceeds 64 KiB");
    let document = roxmltree::Document::parse_with_options(
        svg,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: 4096,
        },
    );
    let document = document.map_err(|error| anyhow::anyhow!("Invalid provider SVG: {error}"))?;
    ensure!(
        document
            .root_element()
            .tag_name()
            .name()
            .eq_ignore_ascii_case("svg"),
        "Provider icon is not an SVG document"
    );
    for node in document.descendants().filter(|node| node.is_element()) {
        let name = node.tag_name().name();
        ensure!(
            !["script", "foreignObject", "style"]
                .iter()
                .any(|tag| name.eq_ignore_ascii_case(tag)),
            "Provider icon contains unsupported elements"
        );
        for attribute in node.attributes() {
            let name = attribute.name().to_ascii_lowercase();
            ensure!(
                !name.starts_with("on"),
                "Provider icon has an event handler"
            );
            let value = attribute.value();
            let compact = value
                .chars()
                .filter(|ch| !ch.is_whitespace())
                .collect::<String>()
                .to_ascii_lowercase();
            ensure!(
                !compact.contains("javascript:"),
                "Provider icon has a javascript URL"
            );
            if name == "href" {
                ensure!(
                    value.trim().starts_with('#'),
                    "Provider icon has an external href"
                );
            }
            // Paint servers may reference local IDs, never files or remote images.
            let mut urls = compact.as_str();
            while let Some(start) = urls.find("url(") {
                urls = &urls[start + 4..];
                let end = urls
                    .find(')')
                    .ok_or_else(|| anyhow::anyhow!("Invalid provider icon paint reference"))?;
                ensure!(
                    urls[..end].trim_matches(['\'', '"']).starts_with('#'),
                    "Provider icon has an external paint reference"
                );
                urls = &urls[end + 1..];
            }
        }
    }
    Ok(())
}

pub fn provider_svg_color(svg: &[u8], color: u32) -> Result<Vec<u8>> {
    let source = std::str::from_utf8(svg)?;
    let document = roxmltree::Document::parse(source)?;
    let root = document.root_element();
    if root.attribute("color").is_some() {
        return Ok(svg.to_vec());
    }
    let start = root.range().start + 1;
    let name_end = source[start..]
        .find(|ch: char| ch.is_whitespace() || ch == '/' || ch == '>')
        .ok_or_else(|| anyhow::anyhow!("Missing SVG opening tag"))?
        + start;
    let mut output = String::with_capacity(source.len() + 24);
    output.push_str(&source[..name_end]);
    output.push_str(&format!(" color=\"#{color:06x}\""));
    output.push_str(&source[name_end..]);
    Ok(output.into_bytes())
}

impl Directory {
    pub fn set_host(&mut self, server_id: &str) {
        if self.server_id.as_deref() != Some(server_id) {
            self.last_focused.clear();
        }
        self.clear_provider_icons();
        self.server_id = Some(server_id.into());
    }

    pub fn replace_provider_icons(&mut self, payload: &Value) -> Result<()> {
        let entries = payload["entries"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("Missing provider icon snapshot"))?;
        ensure!(
            entries.len() <= 256,
            "Provider icon snapshot exceeds 256 entries"
        );
        let mut icons = BTreeMap::new();
        let mut bytes = 0usize;
        for entry in entries {
            let provider = entry["provider"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Provider icon has no identity"))?;
            if is_builtin_provider(provider) {
                continue;
            }
            if let Some(svg) = entry["iconSvg"].as_str() {
                validate_provider_svg(svg)?;
                bytes = bytes.saturating_add(svg.len());
                ensure!(bytes <= 1024 * 1024, "Provider icon snapshot exceeds 1 MiB");
                icons.insert(provider.into(), svg.as_bytes().to_vec());
            }
        }
        self.provider_icons = icons;
        Ok(())
    }
    pub fn clear_provider_icons(&mut self) {
        self.provider_icons.clear();
    }
    pub fn provider_svg(&self, provider: &str) -> Option<&[u8]> {
        self.provider_icons.get(provider).map(Vec::as_slice)
    }
    pub fn agent_target(&self, id: &str) -> Option<FocusedSession> {
        let agent = self.agents.get(id)?;
        if agent.archived_at.is_some() {
            return None;
        }
        match &agent.execution {
            None => Some(FocusedSession::Agent(agent.id.clone())),
            Some(AgentExecution::Terminal { terminal_id, .. })
                if !terminal_id.trim().is_empty() =>
            {
                Some(FocusedSession::Terminal(terminal_id.clone()))
            }
            Some(AgentExecution::Terminal { .. }) => None,
            Some(AgentExecution::Unknown) => None,
        }
    }
    pub fn remember_focus(&mut self, workspace_id: &str, target: FocusedSession) -> bool {
        let owned = match &target {
            FocusedSession::Agent(id) => self.agents.get(id).is_some_and(|a| {
                a.workspace_id.as_deref() == Some(workspace_id)
                    && a.archived_at.is_none()
                    && a.execution.is_none()
            }),
            FocusedSession::Terminal(id) => self.terminal_workspace(id) == Some(workspace_id),
        };
        if owned {
            self.last_focused.insert(workspace_id.into(), target);
        }
        owned
    }
    pub fn terminal_workspace(&self, terminal_id: &str) -> Option<&str> {
        self.terminals.get(terminal_id).and_then(|t|t.workspace_id.as_deref()).or_else(||self.agents.values().find(|a|a.archived_at.is_none() && matches!(&a.execution,Some(AgentExecution::Terminal{terminal_id:id,..}) if id==terminal_id)).and_then(|a|a.workspace_id.as_deref()))
    }
    pub fn terminal_agent(&self, terminal_id: &str) -> Option<&Agent> {
        let workspace = self.terminal_workspace(terminal_id)?;
        self.sidebar_agents(workspace).into_iter().find(|a| matches!(&a.execution, Some(AgentExecution::Terminal { terminal_id: id, .. }) if id == terminal_id))
    }
    pub fn terminal_title(&self, terminal: &Terminal) -> String {
        self.terminal_agent(&terminal.id)
            .map(|a| a.display_title().to_owned())
            .unwrap_or_else(|| {
                terminal
                    .title
                    .as_deref()
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .unwrap_or(&terminal.name)
                    .into()
            })
    }
    pub fn workspace_title(&self, workspace_id: &str, fallback: &str) -> String {
        if let Some(target) = self.last_focused.get(workspace_id) {
            match target {
                FocusedSession::Agent(id) => {
                    if let Some(agent) = self.agents.get(id).filter(|a| {
                        a.workspace_id.as_deref() == Some(workspace_id)
                            && a.archived_at.is_none()
                            && a.execution.is_none()
                    }) {
                        return agent.display_title().into();
                    }
                }
                FocusedSession::Terminal(id) => {
                    if let Some(agent) = self
                        .terminal_agent(id)
                        .filter(|a| a.workspace_id.as_deref() == Some(workspace_id))
                    {
                        return agent.display_title().into();
                    }
                    if let Some(terminal) = self
                        .terminals
                        .get(id)
                        .filter(|t| t.workspace_id.as_deref() == Some(workspace_id))
                    {
                        return self.terminal_title(terminal);
                    }
                }
            }
        }
        self.sidebar_agents(workspace_id)
            .first()
            .map(|a| a.display_title().to_owned())
            .unwrap_or_else(|| fallback.into())
    }
    pub fn sidebar_agents(&self, workspace_id: &str) -> Vec<&Agent> {
        let mut agents: Vec<_> = self
            .agents
            .values()
            .filter(|a| {
                if a.internal
                    || a.archived_at.is_some()
                    || a.workspace_id.as_deref() != Some(workspace_id)
                {
                    return false;
                }
                if a.labels
                    .get("paseo.parent-server-id")
                    .map(|id| id.trim())
                    .filter(|id| !id.is_empty())
                    .is_some_and(|parent| {
                        self.server_id
                            .as_deref()
                            .is_some_and(|local| local != parent)
                    })
                {
                    return true;
                }
                match a.parent_agent_id() {
                    None => true,
                    Some(parent) => self
                        .agents
                        .get(parent)
                        .and_then(|p| p.workspace_id.as_deref())
                        .is_some_and(|w| w != workspace_id),
                }
            })
            .collect();
        agents.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        agents
    }
    pub fn apply(&mut self, kind: &str, payload: Value) -> Result<()> {
        match kind {
            "agent_stream" => {
                if let Some(agent) = payload["agentId"]
                    .as_str()
                    .and_then(|id| self.agents.get_mut(id))
                {
                    let event = &payload["event"];
                    let turn_matches = event["turnId"]
                        .as_str()
                        .zip(
                            agent
                                .active_turn
                                .as_ref()
                                .and_then(|turn| turn["turnId"].as_str()),
                        )
                        .is_none_or(|(incoming, current)| incoming == current);
                    match event["type"].as_str() {
                        Some("turn_started") if agent.execution.is_none() => {
                            agent.active_turn = Some(serde_json::json!({"turnId":event["turnId"]}));
                            agent.status = "running".into();
                            if agent.pending_permissions.is_empty() {
                                agent.requires_attention = false;
                                agent.attention_reason = None;
                            }
                        }
                        Some("turn_completed" | "turn_canceled")
                            if turn_matches && agent.execution.is_none() =>
                        {
                            agent.active_turn = Some(Value::Null);
                            agent.status = "idle".into();
                        }
                        Some("turn_failed") if turn_matches && agent.execution.is_none() => {
                            agent.active_turn = Some(Value::Null);
                            agent.status = "error".into();
                        }
                        Some("permission_requested") => {
                            let request = event["request"].clone();
                            if !agent
                                .pending_permissions
                                .iter()
                                .any(|old| old["id"] == request["id"])
                            {
                                agent.pending_permissions.push(request);
                            }
                        }
                        Some("permission_resolved") => {
                            agent
                                .pending_permissions
                                .retain(|request| request["id"] != event["requestId"]);
                            if agent.attention_reason.as_deref() == Some("permission") {
                                if agent.pending_permissions.is_empty() {
                                    agent.requires_attention = false;
                                    agent.attention_reason = None;
                                }
                            }
                        }
                        Some("attention_required") => {
                            agent.requires_attention = true;
                            agent.attention_reason = event["reason"].as_str().map(str::to_owned);
                        }
                        _ => {}
                    }
                }
            }
            "fetch_agents_response" => {
                let page: AgentPage = serde_json::from_value(payload)?;
                self.agents.extend(
                    page.entries
                        .into_iter()
                        .map(|entry| (entry.agent.id.clone(), entry.agent)),
                );
            }
            "agent_update" => match payload["kind"].as_str() {
                Some("upsert") => {
                    let agent: Agent = serde_json::from_value(payload["agent"].clone())?;
                    self.agents.insert(agent.id.clone(), agent);
                }
                Some("remove") => {
                    if let Some(id) = payload["agentId"].as_str() {
                        self.agents.remove(id);
                    }
                }
                _ => {}
            },
            "agent_status" => {
                let agent: Agent = serde_json::from_value(payload["info"].clone())?;
                self.agents.insert(agent.id.clone(), agent);
            }
            "project.list.response" => {
                let list: ProjectList = serde_json::from_value(payload)?;
                self.projects = list
                    .projects
                    .into_iter()
                    .map(|p| (p.project_id.clone(), p))
                    .collect();
            }
            "fetch_workspaces_response" => {
                let page: WorkspacePage = serde_json::from_value(payload)?;
                self.workspaces
                    .extend(page.entries.into_iter().map(|w| (w.id.clone(), w)));
            }
            "list_terminals_response" => {
                let list: TerminalList = serde_json::from_value(payload)?;
                self.terminals = list
                    .terminals
                    .into_iter()
                    .map(|t| (t.id.clone(), t))
                    .collect();
            }
            "workspace_update" => match payload["kind"].as_str() {
                Some("upsert") => {
                    let workspace: Workspace =
                        serde_json::from_value(payload["workspace"].clone())?;
                    self.workspaces.insert(workspace.id.clone(), workspace);
                }
                Some("remove") => {
                    if let Some(id) = payload["id"].as_str() {
                        self.workspaces.remove(id);
                        self.last_focused.remove(id);
                    }
                }
                _ => {}
            },
            "project.update" => match payload["kind"].as_str() {
                Some("upsert") => {
                    let project: Project = serde_json::from_value(payload["project"].clone())?;
                    self.projects.insert(project.project_id.clone(), project);
                }
                Some("remove") => {
                    if let Some(id) = payload["projectId"].as_str() {
                        self.projects.remove(id);
                    }
                }
                _ => {}
            },
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn workspace(id: &str) -> Value {
        json!({"id":id,"projectId":"p","projectDisplayName":"project","projectRootPath":"/repo","name":"main","status":"future-status","futureField":{}})
    }

    #[test]
    fn directory_pages_accumulate_and_live_tombstones_remove_only_their_workspace() {
        let mut directory = Directory::default();
        for id in ["one", "two"] {
            directory.apply("fetch_workspaces_response", json!({"entries":[workspace(id)],"pageInfo":{"nextCursor":null,"hasMore":false}})).unwrap();
        }
        directory
            .apply("workspace_update", json!({"kind":"remove","id":"one"}))
            .unwrap();
        assert_eq!(directory.workspaces.len(), 1);
        assert_eq!(directory.workspaces["two"].status, "future-status");
    }

    #[test]
    fn malformed_projection_is_rejected_before_mutating_directory() {
        let mut directory = Directory::default();
        directory
            .apply(
                "workspace_update",
                json!({"kind":"upsert","workspace":workspace("one")}),
            )
            .unwrap();
        assert!(
            directory
                .apply(
                    "workspace_update",
                    json!({"kind":"upsert","workspace":{"id":"one"}})
                )
                .is_err()
        );
        assert_eq!(directory.workspaces["one"].name, "main");
    }

    #[test]
    fn idle_terminal_agents_keep_execution_identity_through_status_updates() {
        let mut directory = Directory::default();
        let mut agent = json!({"id":"agent","provider":"pi","workspaceId":"workspace","title":"Current task","status":"idle","execution":{"kind":"terminal","terminalId":"pty","providerSessionId":"different-id"}});
        directory
            .apply(
                "fetch_agents_response",
                json!({"entries":[{"agent":agent}],"pageInfo":{"nextCursor":null,"hasMore":false}}),
            )
            .unwrap();
        assert!(
            matches!(&directory.agents["agent"].execution,Some(AgentExecution::Terminal{terminal_id,..}) if terminal_id=="pty")
        );
        agent["status"] = "running".into();
        agent["title"] = "New task".into();
        directory
            .apply(
                "agent_status",
                json!({"agentId":"agent","status":"running","info":agent}),
            )
            .unwrap();
        assert_eq!(directory.agents["agent"].title.as_deref(), Some("New task"));
        directory
            .apply("agent_update", json!({"kind":"remove","agentId":"agent"}))
            .unwrap();
        assert!(directory.agents.is_empty());
    }

    #[test]
    fn invalid_terminal_execution_cannot_fall_back_to_an_agent_conversation() {
        assert!(
            serde_json::from_value::<Agent>(
                json!({"id":"a","provider":"pi","status":"idle","execution":{"kind":"terminal"}})
            )
            .is_err()
        );
        let future:Agent=serde_json::from_value(json!({"id":"a","provider":"future","status":"future","execution":{"kind":"remote-future","id":"other"}})).unwrap();
        assert!(matches!(future.execution, Some(AgentExecution::Unknown)));
    }
    #[test]
    fn sidebar_preserves_idle_roots_and_uses_managed_turn_or_terminal_status() {
        let mut directory = Directory::default();
        let agents = json!([
          {"id":"idle","provider":"pi","workspaceId":"w","status":"idle"},
          {"id":"managed","provider":"claude","workspaceId":"w","status":"running","activeTurn":null},
          {"id":"working","provider":"claude","workspaceId":"w","status":"idle","activeTurn":{"turnId":"t"}},
          {"id":"terminal","provider":"pi","workspaceId":"w","status":"running","execution":{"kind":"terminal","terminalId":"pty","icon":"bot"}},
          {"id":"child","provider":"pi","workspaceId":"w","status":"idle","labels":{"paseo.parent-agent-id":"idle"}},
          {"id":"archived","provider":"pi","workspaceId":"w","status":"idle","archivedAt":"2026-01-01"},
          {"id":"other","provider":"pi","workspaceId":"other","status":"idle"}
        ]);
        for agent in agents.as_array().unwrap() {
            directory
                .apply("agent_update", json!({"kind":"upsert","agent":agent}))
                .unwrap();
        }
        let rows = directory.sidebar_agents("w");
        assert_eq!(
            rows.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
            vec!["idle", "managed", "terminal", "working"]
        );
        assert_eq!(
            directory.agents["managed"].sidebar_status(),
            AgentStatusBucket::Done
        );
        assert_eq!(
            directory.agents["working"].sidebar_status(),
            AgentStatusBucket::Running
        );
        assert_eq!(
            directory.agents["terminal"].sidebar_status(),
            AgentStatusBucket::Running
        );
        assert_eq!(directory.agents["terminal"].sidebar_icon(), "pi");
    }

    #[test]
    fn sidebar_attention_priority_matches_app_without_hiding_idle_or_cross_workspace_roots() {
        let mut agent: Agent = serde_json::from_value(json!({"id":"a","provider":"pi","workspaceId":"w","status":"running","activeTurn":{"turnId":"t"},"requiresAttention":true})).unwrap();
        assert_eq!(agent.sidebar_status(), AgentStatusBucket::Running);
        agent.attention_reason = Some("error".into());
        assert_eq!(agent.sidebar_status(), AgentStatusBucket::Failed);
        agent.pending_permissions.push(json!({"id":"permission"}));
        assert_eq!(agent.sidebar_status(), AgentStatusBucket::NeedsInput);
        agent.pending_permissions.clear();
        agent.attention_reason = Some("permission".into());
        assert_eq!(agent.sidebar_status(), AgentStatusBucket::NeedsInput);
        agent.active_turn = Some(Value::Null);
        agent.status = "idle".into();
        agent.attention_reason = Some("finished".into());
        assert_eq!(agent.sidebar_status(), AgentStatusBucket::Attention);
        agent.requires_attention = false;
        assert_eq!(agent.sidebar_status(), AgentStatusBucket::Done);
        let mut directory = Directory::default();
        for (id, workspace, parent) in [
            ("parent", "other", None),
            ("root", "w", Some("parent")),
            ("orphan", "w", Some("missing")),
            ("hidden", "w", Some("root")),
        ] {
            directory.apply("agent_update",json!({"kind":"upsert","agent":{"id":id,"provider":"pi","workspaceId":workspace,"status":"idle","labels":parent.map(|p| json!({"paseo.parent-agent-id":p})).unwrap_or(json!({}))}})).unwrap();
        }
        assert_eq!(
            directory
                .sidebar_agents("w")
                .iter()
                .map(|a| a.id.as_str())
                .collect::<Vec<_>>(),
            vec!["root"]
        );
    }

    #[test]
    fn navigation_uses_execution_identity_and_focus_titles_survive_workspace_switches() {
        let mut directory = Directory::default();
        for agent in [
            json!({"id":"built-in","provider":"pi","workspaceId":"w","title":"Same title","status":"idle"}),
            json!({"id":"terminal-agent","provider":"pi","workspaceId":"w","title":"Same title","status":"idle","execution":{"kind":"terminal","terminalId":"real-pty","icon":"bot","profileId":"profile"}}),
        ] {
            directory
                .apply("agent_update", json!({"kind":"upsert","agent":agent}))
                .unwrap();
        }
        assert_eq!(
            directory.agent_target("built-in"),
            Some(FocusedSession::Agent("built-in".into()))
        );
        assert_eq!(
            directory.agent_target("terminal-agent"),
            Some(FocusedSession::Terminal("real-pty".into()))
        );
        directory.terminals.insert(
            "real-pty".into(),
            serde_json::from_value(
                json!({"id":"real-pty","name":"Shell","title":"Shell title","workspaceId":"w"}),
            )
            .unwrap(),
        );
        assert!(directory.remember_focus("w", FocusedSession::Terminal("real-pty".into())));
        assert_eq!(
            directory.workspace_title("w", "Workspace fallback"),
            "Same title"
        );
        directory.agents.get_mut("terminal-agent").unwrap().title =
            Some("Fresh terminal task".into());
        assert_eq!(
            directory.workspace_title("w", "Workspace fallback"),
            "Fresh terminal task"
        );
        assert!(directory.remember_focus("w", FocusedSession::Agent("built-in".into())));
        assert_eq!(
            directory.workspace_title("w", "Workspace fallback"),
            "Same title"
        );
        assert!(!directory.remember_focus("other", FocusedSession::Agent("built-in".into())));
        assert_eq!(directory.workspace_title("other", "Other"), "Other");
        assert_eq!(
            directory.workspace_title("w", "Workspace fallback"),
            "Same title"
        );
        assert!(directory.remember_focus("w", FocusedSession::Terminal("real-pty".into())));
        directory.agents.remove("terminal-agent");
        assert_eq!(
            directory.workspace_title("w", "Workspace fallback"),
            "Shell title"
        );
        directory.terminals.remove("real-pty");
        assert_eq!(
            directory.workspace_title("w", "Workspace fallback"),
            "Same title"
        );
    }

    #[test]
    fn host_provider_icons_keep_builtin_identity_and_reset_without_stale_fallback() {
        let mut directory = Directory::default();
        let svg = "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 16 16\"><path d=\"M0 0h16v16H0z\"/></svg>";
        directory.replace_provider_icons(&json!({"entries":[{"provider":"custom","iconSvg":svg},{"provider":"pi","iconSvg":svg}]})).unwrap();
        assert_eq!(directory.provider_svg("custom"), Some(svg.as_bytes()));
        assert_eq!(directory.provider_svg("pi"), None);
        for bad in [
            "<svg><script>bad</script></svg>",
            "<svg><image href=\"file:///secret\"/></svg>",
            "<svg onload=\"bad()\"></svg>",
            "<svg><style>bad</style></svg>",
        ] {
            assert!(
                directory
                    .replace_provider_icons(
                        &json!({"entries":[{"provider":"custom","iconSvg":bad}]})
                    )
                    .is_err()
            );
            assert_eq!(directory.provider_svg("custom"), Some(svg.as_bytes()));
        }
        directory
            .replace_provider_icons(&json!({"entries":[]}))
            .unwrap();
        assert_eq!(directory.provider_svg("custom"), None);
    }

    #[test]
    fn provider_svg_rejects_entities_external_references_and_budgets_atomically() {
        let mut directory = Directory::default();
        for svg in [
            "<svg><SCRIPT/></svg>",
            "<svg><script\t/></svg>",
            "<svg><image href=\"&#102;ile:///secret\"/></svg>",
            "<svg xmlns:xlink=\"http://www.w3.org/1999/xlink\"><use xlink:href=\"https://bad\"/></svg>",
            "<svg style=\"fill:url(https://bad)\"/>",
            "<!DOCTYPE svg [<!ENTITY e SYSTEM 'file:///secret'>]><svg>&e;</svg>",
            "<svg><path></svg>",
        ] {
            assert!(
                directory
                    .replace_provider_icons(
                        &json!({"entries":[{"provider":"custom","iconSvg":svg}]})
                    )
                    .is_err(),
                "{svg}"
            );
            assert_eq!(directory.provider_svg("custom"), None);
        }
        let good = "<svg><defs><path id=\"p\"/></defs><use href=\"#p\" fill=\"url(#p)\"/></svg>";
        directory
            .replace_provider_icons(&json!({"entries":[{"provider":"custom","iconSvg":good}]}))
            .unwrap();
        let huge = format!("<svg>{}</svg>", " ".repeat(65536));
        assert!(
            directory
                .replace_provider_icons(&json!({"entries":[{"provider":"custom","iconSvg":huge}]}))
                .is_err()
        );
        assert!(
            directory
                .replace_provider_icons(&json!({"entries":vec![json!({"provider":"p"});257]}))
                .is_err()
        );
        assert_eq!(directory.provider_svg("custom"), Some(good.as_bytes()));
        directory.clear_provider_icons();
        assert_eq!(directory.provider_svg("custom"), None);
    }

    #[test]
    fn reconnect_directory_refresh_keeps_live_changes_and_prunes_only_after_complete_pages() {
        let mut directory = Directory::default();
        directory
            .apply(
                "agent_update",
                json!({"kind":"upsert","agent":{"id":"stale","provider":"pi","status":"idle"}}),
            )
            .unwrap();
        let mut refresh = DirectoryRefresh::default();
        let mut live =
            json!({"kind":"upsert","agent":{"id":"live","provider":"pi","status":"idle"}});
        refresh.apply("agent_update", &mut live, &mut directory);
        directory.apply("agent_update", live).unwrap();
        let mut remove = json!({"kind":"remove","agentId":"gone"});
        refresh.apply("agent_update", &mut remove, &mut directory);
        let mut page = json!({"entries":[{"agent":{"id":"gone","provider":"pi","status":"idle"}},{"agent":{"id":"fresh","provider":"pi","status":"idle"}}],"pageInfo":{"hasMore":false,"nextCursor":null}});
        refresh.apply("fetch_agents_response", &mut page, &mut directory);
        directory.apply("fetch_agents_response", page).unwrap();
        assert_eq!(
            directory.agents.keys().cloned().collect::<Vec<_>>(),
            ["fresh", "live"]
        );
        assert!(!refresh.complete());
        let mut page = json!({"entries":[],"pageInfo":{"hasMore":false,"nextCursor":null}});
        refresh.apply("fetch_workspaces_response", &mut page, &mut directory);
        assert!(refresh.complete());
    }

    #[test]
    fn custom_svg_default_color_does_not_replace_explicit_paints_or_authored_style() {
        let svg = br##"<?xml version="1.0"?><svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" style="color:red"><path fill="currentColor"/><path fill="#00ff00"/></svg>"##;
        let transformed = provider_svg_color(svg, 0xa1a5a4).unwrap();
        let parsed =
            roxmltree::Document::parse(std::str::from_utf8(&transformed).unwrap()).unwrap();
        assert_eq!(parsed.root_element().attribute("color"), Some("#a1a5a4"));
        assert_eq!(parsed.root_element().attribute("style"), Some("color:red"));
        assert_eq!(
            parsed
                .root_element()
                .last_element_child()
                .unwrap()
                .attribute("fill"),
            Some("#00ff00")
        );
        let explicit = br#"<svg color="blue"/>"#;
        assert_eq!(provider_svg_color(explicit, 0xa1a5a4).unwrap(), explicit);
    }

    #[test]
    fn managed_open_turn_overrides_stale_error_and_last_permission_clears_attention() {
        let mut directory = Directory::default();
        directory.apply("agent_update", json!({"kind":"upsert","agent":{"id":"a","provider":"pi","workspaceId":"w","status":"error","activeTurn":{"turnId":"new"}}})).unwrap();
        assert_eq!(
            directory.agents["a"].sidebar_status(),
            AgentStatusBucket::Running
        );
        directory
            .apply(
                "agent_stream",
                json!({"agentId":"a","event":{"type":"attention_required","reason":"permission"}}),
            )
            .unwrap();
        directory.apply("agent_stream", json!({"agentId":"a","event":{"type":"permission_requested","request":{"id":"p1"}}})).unwrap();
        directory.apply("agent_stream", json!({"agentId":"a","event":{"type":"permission_requested","request":{"id":"p2"}}})).unwrap();
        directory
            .apply(
                "agent_stream",
                json!({"agentId":"a","event":{"type":"permission_resolved","requestId":"p1"}}),
            )
            .unwrap();
        assert_eq!(
            directory.agents["a"].sidebar_status(),
            AgentStatusBucket::NeedsInput
        );
        directory
            .apply(
                "agent_stream",
                json!({"agentId":"a","event":{"type":"permission_resolved","requestId":"p2"}}),
            )
            .unwrap();
        assert!(!directory.agents["a"].requires_attention);
        assert_eq!(
            directory.agents["a"].sidebar_status(),
            AgentStatusBucket::Running
        );
        directory
            .apply(
                "agent_stream",
                json!({"agentId":"a","event":{"type":"turn_completed","turnId":"new"}}),
            )
            .unwrap();
        assert_eq!(
            directory.agents["a"].sidebar_status(),
            AgentStatusBucket::Done
        );
    }

    #[test]
    fn live_turn_and_permission_updates_change_badges_without_a_timeline_reload() {
        let mut directory = Directory::default();
        directory.apply("agent_update",json!({"kind":"upsert","agent":{"id":"a","provider":"pi","workspaceId":"w","status":"idle"}})).unwrap();
        directory
            .apply(
                "agent_stream",
                json!({"agentId":"a","event":{"type":"turn_started","turnId":"new"}}),
            )
            .unwrap();
        assert_eq!(
            directory.agents["a"].sidebar_status(),
            AgentStatusBucket::Running
        );
        directory
            .apply(
                "agent_stream",
                json!({"agentId":"a","event":{"type":"turn_completed","turnId":"old"}}),
            )
            .unwrap();
        assert_eq!(
            directory.agents["a"].sidebar_status(),
            AgentStatusBucket::Running
        );
        directory
            .apply(
                "agent_stream",
                json!({"agentId":"a","event":{"type":"permission_requested","request":{"id":"p"}}}),
            )
            .unwrap();
        assert_eq!(
            directory.agents["a"].sidebar_status(),
            AgentStatusBucket::NeedsInput
        );
        directory
            .apply(
                "agent_stream",
                json!({"agentId":"a","event":{"type":"permission_resolved","requestId":"p"}}),
            )
            .unwrap();
        assert_eq!(
            directory.agents["a"].sidebar_status(),
            AgentStatusBucket::Running
        );
        directory
            .apply(
                "agent_stream",
                json!({"agentId":"a","event":{"type":"turn_failed","turnId":"new"}}),
            )
            .unwrap();
        assert_eq!(
            directory.agents["a"].sidebar_status(),
            AgentStatusBucket::Failed
        );
    }

    #[test]
    fn focused_terminal_identity_precedes_inventory_and_archive_removal_invalidates_names() {
        let mut directory = Directory::default();
        directory.apply("agent_update",json!({"kind":"upsert","agent":{"id":"a","provider":"pi","workspaceId":"w","status":"idle","title":"Task","execution":{"kind":"terminal","terminalId":"pty"}}})).unwrap();
        assert!(directory.remember_focus("w", FocusedSession::Terminal("pty".into())));
        assert_eq!(directory.workspace_title("w", "Fallback"), "Task");
        directory.apply("agent_update",json!({"kind":"upsert","agent":{"id":"a","provider":"pi","workspaceId":"w","status":"idle","title":" ","execution":{"kind":"terminal","terminalId":"pty"}}})).unwrap();
        assert_eq!(directory.workspace_title("w", "Fallback"), "pi");
        directory
            .apply("agent_update", json!({"kind":"remove","agentId":"a"}))
            .unwrap();
        assert_eq!(directory.workspace_title("w", "Fallback"), "Fallback");
        let unknown:Agent=serde_json::from_value(json!({"id":"empty","provider":"pi","status":"idle","execution":{"kind":"terminal","terminalId":""}})).unwrap();
        directory.agents.insert(unknown.id.clone(), unknown);
        assert_eq!(directory.agent_target("empty"), None);
    }

    #[test]
    fn icon_snapshot_budget_and_host_reset_do_not_retain_previous_host_assets() {
        let mut directory = Directory::default();
        let svg = format!("<svg>{}</svg>", " ".repeat(63000));
        let entries = (0..17)
            .map(|i| json!({"provider":format!("p{i}"),"iconSvg":svg}))
            .collect::<Vec<_>>();
        assert!(
            directory
                .replace_provider_icons(&json!({"entries":entries}))
                .is_err()
        );
        assert!(directory.provider_icons.is_empty());
        directory
            .replace_provider_icons(&json!({"entries":[{"provider":"custom","iconSvg":"<svg/>"}]}))
            .unwrap();
        directory.clear_provider_icons();
        assert!(directory.provider_icons.is_empty());
        directory
            .replace_provider_icons(&json!({"entries":[{"provider":"other","iconSvg":"<svg/>"}]}))
            .unwrap();
        assert_eq!(directory.provider_svg("custom"), None);
        assert_eq!(directory.provider_svg("other"), Some("<svg/>".as_bytes()));
    }
    #[test]
    fn legacy_missing_turn_and_explicit_idle_turn_follow_the_app_normalization() {
        let missing: Agent =
            serde_json::from_value(json!({"id":"a","provider":"pi","status":"running"})).unwrap();
        let idle: Agent = serde_json::from_value(
            json!({"id":"b","provider":"pi","status":"running","activeTurn":null}),
        )
        .unwrap();
        assert_eq!(missing.sidebar_status(), AgentStatusBucket::Running);
        assert_eq!(idle.sidebar_status(), AgentStatusBucket::Done);
    }

    #[test]
    fn remote_parent_ids_do_not_hide_local_roots_and_host_change_clears_focus() {
        let mut directory = Directory::default();
        directory.set_host("local");
        for agent in [
            json!({"id":"parent","provider":"pi","workspaceId":"w","status":"idle"}),
            json!({"id":"remote-child","provider":"pi","workspaceId":"w","title":"Remote work","status":"idle","labels":{"paseo.parent-agent-id":"parent","paseo.parent-server-id":"remote"}}),
            json!({"id":"internal","provider":"pi","workspaceId":"w","status":"idle","internal":true}),
        ] {
            directory
                .apply("agent_update", json!({"kind":"upsert","agent":agent}))
                .unwrap();
        }
        assert_eq!(
            directory
                .sidebar_agents("w")
                .iter()
                .map(|a| a.id.as_str())
                .collect::<Vec<_>>(),
            vec!["parent", "remote-child"]
        );
        assert!(directory.remember_focus("w", FocusedSession::Agent("remote-child".into())));
        assert_eq!(directory.workspace_title("w", "Fallback"), "Remote work");
        directory.set_host("other");
        assert_eq!(directory.workspace_title("w", "Fallback"), "pi");
    }
    #[test]
    fn history_pages_carry_entries_cursors_and_sort_recency() {
        let page: HistoryPage = serde_json::from_value(json!({
            "entries":[
                {"agent":{"id":"new","provider":"pi","workspaceId":"w","status":"idle","createdAt":"2026-01-02","updatedAt":"2026-01-03"}},
                {"agent":{"id":"old","provider":"claude","status":"idle","archivedAt":"2026-01-01","createdAt":"2025-12-30","updatedAt":"2026-01-01"}}
            ],
            "pageInfo":{"nextCursor":"c2","hasMore":true}
        }))
        .unwrap();
        assert_eq!(page.entries.len(), 2);
        assert_eq!(
            page.entries[0].agent.updated_at.as_deref(),
            Some("2026-01-03")
        );
        assert!(page.entries[1].agent.archived_at.is_some());
        assert_eq!(page.page_info.next_cursor.as_deref(), Some("c2"));
        assert!(page.page_info.has_more);
        // Older daemons omit searchTruncated entirely.
        let minimal: HistoryPage = serde_json::from_value(json!({
            "entries":[],
            "pageInfo":{"nextCursor":null,"hasMore":false}
        }))
        .unwrap();
        assert!(minimal.search_truncated.is_none());
    }

    #[test]
    fn workspace_lifecycle_visibility_excludes_private_background_and_archiving() {
        for extra in [
            json!({"internal":true}),
            json!({"background":true}),
            json!({"archivingAt":"now"}),
        ] {
            let mut value = workspace("w");
            value
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            let workspace: Workspace = serde_json::from_value(value).unwrap();
            assert!(!workspace.available());
        }
        let workspace: Workspace = serde_json::from_value(workspace("w")).unwrap();
        assert!(workspace.available());
    }
}

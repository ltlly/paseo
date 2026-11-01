use std::collections::BTreeMap;

use anyhow::Result;
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
    Terminal { terminal_id: String },
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

#[derive(Default, Debug, Serialize)]
pub struct Directory {
    pub projects: BTreeMap<String, Project>,
    pub workspaces: BTreeMap<String, Workspace>,
    pub terminals: BTreeMap<String, Terminal>,
    pub agents: BTreeMap<String, Agent>,
}

impl Directory {
    pub fn apply(&mut self, kind: &str, payload: Value) -> Result<()> {
        match kind {
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
            matches!(&directory.agents["agent"].execution,Some(AgentExecution::Terminal{terminal_id}) if terminal_id=="pty")
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
}

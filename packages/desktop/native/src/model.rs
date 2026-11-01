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

#[derive(Default, Debug, Serialize)]
pub struct Directory {
    pub projects: BTreeMap<String, Project>,
    pub workspaces: BTreeMap<String, Workspace>,
    pub terminals: BTreeMap<String, Terminal>,
}

impl Directory {
    pub fn apply(&mut self, kind: &str, payload: Value) -> Result<()> {
        match kind {
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
}

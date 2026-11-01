use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Operation {
    AddProject,
    CreateDirectory,
    CreateWorkspace,
    Archive(String),
    Inspect(String),
    Restore(String),
}
#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    Ready,
    Pending { id: String, operation: Operation },
    Failed(String),
    Complete,
}
#[derive(Clone, Debug)]
pub struct Lifecycle {
    pub phase: Phase,
    pub recovery: Option<Value>,
    pub last_request: Option<(Operation, Value)>,
    /// Request ids already settled (success, failure, or timeout). Their late
    /// rpc_error replies must not resurface as global errors.
    pub settled: Vec<String>,
}
impl Default for Lifecycle {
    fn default() -> Self {
        Self {
            phase: Phase::Ready,
            recovery: None,
            last_request: None,
            settled: Vec::new(),
        }
    }
}
impl Lifecycle {
    pub fn admit(&self) -> Result<()> {
        ensure!(
            !matches!(self.phase, Phase::Pending { .. }),
            "An operation is already pending"
        );
        Ok(())
    }
    pub fn begin(&mut self, id: String, operation: Operation, payload: Value) {
        self.last_request = Some((operation.clone(), payload));
        self.phase = Phase::Pending { id, operation };
    }
    pub fn fail(&mut self, error: String) {
        self.phase = Phase::Failed(error);
    }
    /// Timeout: acceptance is unknown, so the pending id is remembered and a
    /// late rpc_error for it is swallowed instead of raising a global banner.
    pub fn expire(&mut self, id: &str, error: String) {
        self.remember_settled(id);
        self.phase = Phase::Failed(error);
    }
    pub fn dismisses_error(&self, payload: &Value) -> bool {
        payload["requestId"]
            .as_str()
            .is_some_and(|id| self.settled.iter().any(|seen| seen == id))
    }
    fn remember_settled(&mut self, id: &str) {
        if !self.settled.iter().any(|seen| seen == id) {
            self.settled.push(id.into());
            let excess = self.settled.len().saturating_sub(32);
            self.settled.drain(..excess);
        }
    }
    pub fn owns(&self, payload: &Value) -> bool {
        matches!(&self.phase,Phase::Pending{id,..} if payload["requestId"].as_str()==Some(id))
    }
    pub fn receive(&mut self, kind: &str, payload: &Value) -> Result<Option<String>> {
        let pending_id = match &self.phase {
            Phase::Pending { id, .. } => Some(id.clone()),
            _ => None,
        };
        let settled = self.receive_inner(kind, payload);
        if !matches!(self.phase, Phase::Pending { .. })
            && let Some(id) = pending_id
        {
            self.remember_settled(&id);
        }
        settled
    }
    fn receive_inner(&mut self, kind: &str, payload: &Value) -> Result<Option<String>> {
        if !self.owns(payload) {
            return Ok(None);
        }
        let Phase::Pending { operation, .. } = &self.phase else {
            return Ok(None);
        };
        let expected = match operation {
            Operation::Archive(_) => "archive_workspace_response",
            _ => match operation {
                Operation::AddProject => "project.add.response",
                Operation::CreateDirectory => "project.create_directory.response",
                Operation::CreateWorkspace => "workspace.create.response",
                Operation::Inspect(_) => "workspace.recovery.inspect.response",
                Operation::Restore(_) => "workspace.recovery.restore.response",
                _ => unreachable!(),
            },
        };
        if kind != expected && kind != "rpc_error" {
            return Ok(None);
        }
        if let Operation::Archive(id) | Operation::Restore(id) = operation
            && payload["workspaceId"].as_str() != Some(id)
            && kind != "rpc_error"
        {
            self.fail("The host replied for a different workspace".into());
            return Ok(None);
        }
        if kind == "rpc_error" || payload["error"].as_str().is_some() {
            self.fail(
                payload["error"]
                    .as_str()
                    .unwrap_or("Operation failed")
                    .into(),
            );
            return Ok(None);
        }
        let Phase::Pending { operation, .. } = &self.phase else {
            return Ok(None);
        };
        let selected = match operation {
            Operation::CreateWorkspace => payload["workspace"]["id"].as_str().map(str::to_owned),
            Operation::Restore(id) if payload["accepted"] == true => Some(id.clone()),
            Operation::Inspect(id) => {
                let state = &payload["state"];
                if state["workspaceId"].as_str() != Some(id) {
                    self.fail("The host replied for a different workspace".into());
                    return Ok(None);
                }
                ensure!(
                    state["kind"].as_str().is_some(),
                    "Invalid workspace recovery response"
                );
                self.recovery = Some(state.clone());
                None
            }
            Operation::Archive(_) if payload["archivedAt"].as_str().is_some() => None,
            Operation::AddProject if payload["project"].is_object() => None,
            Operation::CreateDirectory if payload["directoryPath"].is_string() => None,
            _ => {
                self.fail("The host did not confirm this operation".into());
                return Ok(None);
            }
        };
        if matches!(operation, Operation::CreateWorkspace) && selected.is_none() {
            self.fail("The host did not create a workspace".into());
        } else {
            self.phase = Phase::Complete;
        }
        Ok(selected)
    }
    pub fn restore_request(&self) -> Result<Value> {
        let state = self
            .recovery
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Inspect this workspace before restoring it"))?;
        ensure!(
            state["kind"] == "recoverable"
                && matches!(state["action"].as_str(), Some("restore" | "unarchive")),
            "{}",
            state["message"]
                .as_str()
                .unwrap_or("This recovery action is not supported")
        );
        Ok(json!({"workspaceId":state["workspaceId"]}))
    }
}
pub fn request_kind(operation: &Operation) -> &'static str {
    match operation {
        Operation::AddProject => "project.add.request",
        Operation::CreateDirectory => "project.create_directory.request",
        Operation::CreateWorkspace => "workspace.create.request",
        Operation::Archive(_) => "archive_workspace_request",
        Operation::Inspect(_) => "workspace.recovery.inspect.request",
        Operation::Restore(_) => "workspace.recovery.restore.request",
    }
}

pub fn workspace_source(
    path: &str,
    project_id: Option<&str>,
    worktree: bool,
    branch: &str,
) -> Result<Value> {
    ensure!(!path.trim().is_empty(), "Select a project directory");
    let mut source = if worktree {
        ensure!(!branch.trim().is_empty(), "Enter a branch name");
        json!({"kind":"worktree","cwd":path.trim(),"action":"branch-off","branchName":branch.trim()})
    } else {
        json!({"kind":"directory","path":path.trim()})
    };
    if let Some(id) = project_id {
        source["projectId"] = id.into();
    }
    Ok(source)
}

#[derive(Debug, PartialEq)]
pub enum SelectionRestore {
    Waiting,
    Select(String),
    Unavailable(String),
    None,
}
pub fn restore_selection(
    id: Option<&str>,
    directory_complete: bool,
    available: bool,
) -> SelectionRestore {
    let Some(id) = id else {
        return SelectionRestore::None;
    };
    if !directory_complete {
        return SelectionRestore::Waiting;
    }
    if available {
        SelectionRestore::Select(id.into())
    } else {
        SelectionRestore::Unavailable(id.into())
    }
}

#[derive(Default, Clone, Debug, Serialize, Deserialize)]
pub struct RememberedWorkspaces {
    pub by_host: BTreeMap<String, String>,
}
impl RememberedWorkspaces {
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error.into()),
        }
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
        std::fs::write(&temporary, serde_json::to_vec(self)?)?;
        std::fs::rename(temporary, path)?;
        Ok(())
    }
    pub fn select(&mut self, host: &str, id: &str) {
        self.by_host.insert(host.into(), id.into());
    }
    pub fn forget(&mut self, host: &str, id: &str) {
        if self.by_host.get(host).is_some_and(|current| current == id) {
            self.by_host.remove(host);
        }
    }
    pub fn get(&self, host: &str) -> Option<&str> {
        self.by_host.get(host).map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn create_archive_restore_never_commit_before_matching_host_confirmation() {
        let mut state = Lifecycle::default();
        state.begin(
            "create".into(),
            Operation::CreateWorkspace,
            json!({"source":workspace_source("/repo",Some("prj"),false,"").unwrap()}),
        );
        assert!(state.admit().is_err());
        assert_eq!(
            state
                .receive(
                    "workspace.create.response",
                    &json!({"requestId":"other","workspace":{"id":"wrong"}})
                )
                .unwrap(),
            None
        );
        assert!(matches!(state.phase, Phase::Pending { .. }));
        assert_eq!(
            state
                .receive(
                    "workspace.create.response",
                    &json!({"requestId":"create","workspace":{"id":"w"},"error":null})
                )
                .unwrap(),
            Some("w".into())
        );
        state.begin(
            "archive".into(),
            Operation::Archive("w".into()),
            json!({"workspaceId":"w"}),
        );
        state
            .receive(
                "archive_workspace_response",
                &json!({"requestId":"archive","workspaceId":"w","archivedAt":null,"error":"dirty"}),
            )
            .unwrap();
        assert_eq!(state.phase, Phase::Failed("dirty".into()));
        state.begin(
            "inspect".into(),
            Operation::Inspect("w".into()),
            json!({"workspaceId":"w"}),
        );
        state.receive("workspace.recovery.inspect.response",&json!({"requestId":"inspect","state":{"kind":"recoverable","workspaceId":"w","action":"restore"}})).unwrap();
        assert_eq!(state.restore_request().unwrap(), json!({"workspaceId":"w"}));
        state.begin(
            "restore".into(),
            Operation::Restore("w".into()),
            json!({"workspaceId":"w"}),
        );
        assert_eq!(
            state
                .receive(
                    "workspace.recovery.restore.response",
                    &json!({"requestId":"restore","workspaceId":"w","accepted":true})
                )
                .unwrap(),
            Some("w".into())
        );
    }
    #[test]
    fn local_and_worktree_sources_are_explicit_and_failure_remains_retryable() {
        assert_eq!(
            workspace_source("/repo", Some("p"), false, "").unwrap(),
            json!({"kind":"directory","path":"/repo","projectId":"p"})
        );
        assert_eq!(
            workspace_source("/repo", Some("p"), true, " feature ").unwrap(),
            json!({"kind":"worktree","cwd":"/repo","projectId":"p","action":"branch-off","branchName":"feature"})
        );
        assert!(workspace_source("", None, false, "").is_err());
        assert!(workspace_source("/repo", None, true, "").is_err());
        let mut state = Lifecycle::default();
        state.begin("i".into(), Operation::Inspect("w".into()), json!({}));
        state.receive("workspace.recovery.inspect.response",&json!({"requestId":"i","state":{"kind":"unavailable","workspaceId":"w","message":"Directory missing"}})).unwrap();
        assert!(state.restore_request().is_err());
        state.fail("disconnected".into());
        assert!(state.admit().is_ok());
    }
    #[test]
    fn remembered_host_scope_roundtrips_atomically_without_transcripts() {
        let path = std::env::temp_dir().join(format!(
            "paseo-workspace-selection-{}.json",
            uuid::Uuid::new_v4()
        ));
        let mut store = RememberedWorkspaces::default();
        store.select("one", "w");
        store.select("two", "other");
        store.save(&path).unwrap();
        let mut loaded = RememberedWorkspaces::load(&path).unwrap();
        assert_eq!(loaded.get("one"), Some("w"));
        loaded.forget("one", "other");
        assert_eq!(loaded.get("one"), Some("w"));
        loaded.forget("one", "w");
        assert_eq!(loaded.get("one"), None);
        assert_eq!(loaded.get("two"), Some("other"));
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn wrong_kind_or_workspace_does_not_settle_pending_mutations() {
        let mut state = Lifecycle::default();
        state.begin(
            "r".into(),
            Operation::Archive("a".into()),
            json!({"workspaceId":"a"}),
        );
        state
            .receive(
                "workspace.recovery.restore.response",
                &json!({"requestId":"r","accepted":true,"workspaceId":"a","archivedAt":"now"}),
            )
            .unwrap();
        assert!(matches!(state.phase, Phase::Pending { .. }));
        state
            .receive(
                "archive_workspace_response",
                &json!({"requestId":"r","workspaceId":"b","archivedAt":"now"}),
            )
            .unwrap();
        assert!(matches!(state.phase, Phase::Failed(_)));
    }

    #[test]
    fn recovery_identity_and_startup_gating_do_not_guess_authority() {
        let mut state = Lifecycle::default();
        state.begin(
            "i".into(),
            Operation::Inspect("a".into()),
            json!({"workspaceId":"a"}),
        );
        state.receive("workspace.recovery.inspect.response",&json!({"requestId":"i","state":{"kind":"recoverable","workspaceId":"b","action":"restore"}})).unwrap();
        assert!(matches!(state.phase, Phase::Failed(_)));
        assert!(state.recovery.is_none());
        assert_eq!(
            restore_selection(Some("a"), false, false),
            SelectionRestore::Waiting
        );
        assert_eq!(
            restore_selection(Some("a"), true, false),
            SelectionRestore::Unavailable("a".into())
        );
        assert_eq!(
            restore_selection(Some("a"), true, true),
            SelectionRestore::Select("a".into())
        );
    }
    #[test]
    fn late_rpc_error_after_timeout_or_failure_is_swallowed_not_raised() {
        let mut state = Lifecycle::default();
        state.begin("t".into(), Operation::Archive("w".into()), json!({}));
        state.expire("t", "Acceptance is unknown".into());
        assert!(state.dismisses_error(&json!({"requestId":"t"})));
        assert!(!state.dismisses_error(&json!({"requestId":"other"})));
        state.begin("s".into(), Operation::CreateWorkspace, json!({}));
        state
            .receive(
                "workspace.create.response",
                &json!({"requestId":"s","workspace":{"id":"w"},"error":null}),
            )
            .unwrap();
        assert!(state.dismisses_error(&json!({"requestId":"s"})));
    }

    #[test]
    fn expiry_never_retries_or_settles_late_reply_and_rejected_archive_keeps_target() {
        let mut state = Lifecycle::default();
        state.begin(
            "a".into(),
            Operation::Archive("w".into()),
            json!({"workspaceId":"w"}),
        );
        state.fail("Acceptance is unknown".into());
        assert_eq!(
            state
                .receive(
                    "archive_workspace_response",
                    &json!({"requestId":"a","workspaceId":"w","archivedAt":"now"})
                )
                .unwrap(),
            None
        );
        assert_eq!(state.phase, Phase::Failed("Acceptance is unknown".into()));
        assert_eq!(
            state.last_request,
            Some((Operation::Archive("w".into()), json!({"workspaceId":"w"})))
        );
        state.begin(
            "b".into(),
            Operation::Restore("w".into()),
            json!({"workspaceId":"w"}),
        );
        state.receive("workspace.recovery.restore.response",&json!({"requestId":"b","workspaceId":"w","accepted":false,"error":"Branch missing"})).unwrap();
        assert_eq!(state.phase, Phase::Failed("Branch missing".into()));
    }
}

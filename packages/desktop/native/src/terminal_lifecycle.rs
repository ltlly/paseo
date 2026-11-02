//! Native terminal commands keep the request's workspace/pane, never a later
//! selection. Mutations are never replayed after disconnect or timeout.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, VecDeque};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub icon: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Launch {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
}
#[path = "terminal_profile_data.rs"]
mod data;
pub fn defaults() -> &'static [Profile] {
    static PROFILES: std::sync::OnceLock<Vec<Profile>> = std::sync::OnceLock::new();
    PROFILES.get_or_init(|| {
        serde_json::from_str(data::DEFAULT_PROFILES).expect("generated terminal profiles")
    })
}
impl Profile {
    pub fn launch(&self, prompt: &str) -> Launch {
        let sentinel = data::PROMPT_SENTINEL;
        Launch {
            name: self.name.clone(),
            command: self.command.replace(sentinel, prompt),
            args: self
                .args
                .iter()
                .filter(|arg| {
                    !(prompt.is_empty()
                        && (arg.as_str() == sentinel
                            || arg.split_once('=').is_some_and(|(name, value)| {
                                !name.is_empty() && value == sentinel
                            })))
                })
                .map(|arg| arg.replace(sentinel, prompt))
                .collect(),
        }
    }
    pub fn icon_key(&self) -> Option<&str> {
        if let Some(icon) = &self.icon {
            return Some(icon);
        }
        let base = self
            .command
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or("")
            .to_lowercase();
        let base = base
            .find('.')
            .filter(|offset| *offset > 0)
            .map_or(base.as_str(), |offset| &base[..offset]);
        if base == "cursor-agent" {
            return Some("cursor");
        }
        data::KNOWN_ICONS.iter().copied().find(|name| *name == base)
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Focus {
    pub workspace: String,
    pub pane: String,
    pub tab: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Destination {
    pub workspace: String,
    pub cwd: String,
    pub pane: String,
    pub replace: Option<String>,
    pub focus: Option<Focus>,
}
impl Destination {
    pub fn fields(&self, profile: Option<&Profile>) -> Value {
        let mut fields = json!({"cwd":self.cwd,"workspaceId":self.workspace});
        if let Some(profile) = profile {
            let launch = profile.launch("");
            fields["name"] = launch.name.into();
            fields["command"] = launch.command.into();
            fields["args"] = json!(launch.args);
        }
        fields
    }
}
#[derive(Clone, Debug)]
pub enum Operation {
    Config(u64),
    Create(Destination),
    Kill { workspace: String, terminal: String },
}
#[derive(Clone)]
struct Pending {
    op: Operation,
    expired: bool,
}
pub enum Outcome {
    Ignored,
    Consumed,
    Profiles,
    Created {
        terminal: Value,
        destination: Destination,
        late: bool,
    },
    Killed {
        workspace: String,
        terminal: String,
    },
}
pub struct State {
    requests: BTreeMap<String, Pending>,
    settled: VecDeque<String>,
    pub profiles: Option<Vec<Profile>>,
    pub config_error: Option<String>,
    pub demanded: bool,
    generation: u64,
    pub config_pending: bool,
    error: Option<(String, String)>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            requests: BTreeMap::new(),
            settled: VecDeque::new(),
            profiles: None,
            config_error: None,
            demanded: false,
            generation: 0,
            config_pending: false,
            error: None,
        }
    }
}
impl State {
    pub fn profiles(&self) -> &[Profile] {
        self.profiles.as_deref().unwrap_or_else(|| defaults())
    }
    pub fn error_for(&self, workspace: Option<&str>) -> Option<String> {
        self.error
            .as_ref()
            .filter(|(id, _)| Some(id.as_str()) == workspace)
            .map(|(_, message)| message.clone())
    }
    pub fn fail(&mut self, workspace: &str, message: String) {
        self.error = Some((workspace.into(), message));
    }
    pub fn creating(&self) -> bool {
        self.requests
            .values()
            .any(|r| !r.expired && matches!(r.op, Operation::Create(_)))
    }
    pub fn killing(&self, id: &str) -> bool {
        self.requests
            .values()
            .any(|r| !r.expired && matches!(&r.op,Operation::Kill{terminal,..}if terminal==id))
    }
    pub fn room(&self) -> bool {
        self.requests.len() < 64
    }
    pub fn next_config(&mut self) -> u64 {
        self.demanded = true;
        self.generation += 1;
        self.config_pending = true;
        self.config_error = None;
        self.generation
    }
    pub fn begin(&mut self, id: String, op: Operation) {
        if let Operation::Create(dest) = &op {
            if self
                .error
                .as_ref()
                .is_some_and(|(id, _)| id == &dest.workspace)
            {
                self.error = None;
            }
        }
        self.requests.insert(id, Pending { op, expired: false });
    }
    fn retire(&mut self, id: &str) {
        self.settled.push_back(id.into());
        if self.settled.len() > 64 {
            self.settled.pop_front();
        }
    }
    pub fn expire(&mut self, id: &str, message: &str) {
        let Some(request) = self.requests.get_mut(id).filter(|r| !r.expired) else {
            return;
        };
        request.expired = true;
        match &request.op {
            Operation::Config(generation) if *generation == self.generation => {
                self.config_pending = false;
                self.config_error = Some(message.into());
            }
            Operation::Create(dest) => self.error = Some((dest.workspace.clone(), message.into())),
            Operation::Kill { workspace, .. } => {
                self.error = Some((workspace.clone(), message.into()))
            }
            _ => {}
        }
    }
    pub fn disconnected(&mut self) {
        let ids: Vec<_> = self.requests.keys().cloned().collect();
        for id in ids {
            self.expire(&id,"The host disconnected. The terminal operation may have completed; check its terminals before trying again.");
        }
    }
    pub fn config_changed(&mut self, config: &Value) {
        // A push supersedes an older in-flight read without sending another RPC.
        self.generation += 1;
        self.config_pending = false;
        self.apply_config(config);
    }
    fn apply_config(&mut self, config: &Value) {
        let parsed = config
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("The host did not return terminal configuration"))
            .and_then(|config| match config.get("terminalProfiles") {
                None => Ok(defaults().to_vec()),
                Some(value) => {
                    serde_json::from_value::<Vec<Profile>>(value.clone()).map_err(Into::into)
                }
            });
        match parsed {
            Ok(profiles) => {
                self.profiles = Some(profiles);
                self.config_error = None;
            }
            Err(_) => {
                self.config_error = Some("The host returned invalid terminal profiles.".into())
            }
        }
    }
    pub fn receive(&mut self, kind: &str, payload: &Value) -> anyhow::Result<Outcome> {
        let Some(id) = payload["requestId"].as_str() else {
            return Ok(Outcome::Ignored);
        };
        let Some(request) = self.requests.get(id) else {
            return Ok(if self.settled.iter().any(|seen| seen == id) {
                Outcome::Consumed
            } else {
                Outcome::Ignored
            });
        };
        let expected = match request.op {
            Operation::Config(_) => "get_daemon_config_response",
            Operation::Create(_) => "create_terminal_response",
            Operation::Kill { .. } => "kill_terminal_response",
        };
        if kind != expected && kind != "rpc_error" {
            return Ok(Outcome::Ignored);
        }
        let request = self.requests.remove(id).unwrap();
        self.retire(id);
        if kind == "rpc_error" || payload["error"].as_str().is_some() {
            let error = payload["error"]
                .as_str()
                .or_else(|| payload["error"]["message"].as_str())
                .unwrap_or("The host rejected this terminal operation.");
            match request.op {
                Operation::Config(generation) if generation == self.generation => {
                    self.config_pending = false;
                    self.config_error = Some(error.into());
                }
                Operation::Create(dest) => self.fail(&dest.workspace, error.into()),
                Operation::Kill { workspace, .. } => self.fail(&workspace, error.into()),
                _ => {}
            };
            return Ok(Outcome::Consumed);
        }
        match request.op {
            Operation::Config(generation) => {
                if generation != self.generation {
                    return Ok(Outcome::Consumed);
                }
                self.config_pending = false;
                self.apply_config(&payload["config"]);
                Ok(Outcome::Profiles)
            }
            Operation::Create(destination) => {
                let terminal = &payload["terminal"];
                if !terminal.is_object() || terminal["id"].as_str().is_none() {
                    self.fail(
                        &destination.workspace,
                        "The host did not create a terminal.".into(),
                    );
                    return Ok(Outcome::Consumed);
                }
                if terminal["workspaceId"]
                    .as_str()
                    .is_some_and(|id| id != destination.workspace)
                {
                    self.fail(
                        &destination.workspace,
                        "The host created this terminal in a different workspace.".into(),
                    );
                    return Ok(Outcome::Consumed);
                }
                Ok(Outcome::Created {
                    terminal: terminal.clone(),
                    destination,
                    late: request.expired,
                })
            }
            Operation::Kill {
                workspace,
                terminal,
            } => {
                if payload["success"] != true || payload["terminalId"].as_str() != Some(&terminal) {
                    self.fail(&workspace, "Unable to close terminal.".into());
                    return Ok(Outcome::Consumed);
                }
                Ok(Outcome::Killed {
                    workspace,
                    terminal,
                })
            }
        }
    }
}

/// A terminal can enter inventory before its create reply. Creation's explicit
/// destination still wins; passive adoption must not claim that placement.
pub fn place_created(
    layout: &mut crate::split_layout::SplitLayout,
    destination: &Destination,
    terminal: &str,
    select: bool,
) -> anyhow::Result<()> {
    let before = layout.clone();
    let target = layout
        .pane(&destination.pane)
        .map(|p| p.id.clone())
        .or_else(|| layout.focused_pane().map(|p| p.id.clone()))
        .or_else(|| layout.panes().first().map(|p| p.id.clone()))
        .ok_or_else(|| anyhow::anyhow!("No workspace pane is available"))?;
    if let Some(owner) = layout.find_pane_containing_tab(terminal) {
        if owner.id != target && !layout.move_tab_to_pane(terminal, &target) {
            anyhow::bail!("Terminal was created but its pane is full.");
        }
        layout.focus_pane(&target);
        layout.set_focused_tab(&target, terminal);
    } else {
        layout.focus_pane(&target);
        if layout.attach_tab(terminal).is_none() {
            *layout = before;
            anyhow::bail!("Terminal was created but its pane is full.");
        }
    }
    if let Some(replace) = &destination.replace {
        layout.prune_tabs(|tab| tab != replace);
    }
    if !select {
        for pane in before.panes() {
            if let Some(tab) = pane.focused_tab {
                layout.set_focused_tab(&pane.id, &tab);
            }
        }
        if let Some(pane) = before.focused_pane_id {
            layout.focus_pane(&pane);
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn dest() -> Destination {
        Destination {
            workspace: "a".into(),
            cwd: "/a".into(),
            pane: "main".into(),
            replace: None,
            focus: None,
        }
    }
    #[test]
    fn profiles_match_the_authored_launch_and_icon_oracle() {
        let oracle: Value =
            serde_json::from_str(include_str!("testdata/terminal-profiles.json")).unwrap();
        for case in oracle["cases"].as_array().unwrap() {
            let profile: Profile = serde_json::from_value(case["profile"].clone()).unwrap();
            assert_eq!(
                json!(profile.launch(case["prompt"].as_str().unwrap())),
                case["launch"]
            );
            assert_eq!(json!(profile.icon_key()), case["icon"]);
        }
        let d = dest();
        assert_eq!(d.fields(None), json!({"cwd":"/a","workspaceId":"a"}));
    }
    #[test]
    fn expired_create_keeps_its_destination_and_late_error_ownership() {
        let mut state = State::default();
        state.begin("create".into(), Operation::Create(dest()));
        assert!(state.creating());
        state.expire("create", "Check terminals before retrying");
        assert!(!state.creating());
        assert!(state.error_for(Some("b")).is_none());
        let outcome=state.receive("create_terminal_response",&json!({"requestId":"create","terminal":{"id":"t","name":"Terminal","workspaceId":"a"}})).unwrap();
        assert!(
            matches!(outcome,Outcome::Created{destination,late:true,..}if destination.workspace=="a")
        );
        assert!(matches!(
            state
                .receive("rpc_error", &json!({"requestId":"create","error":"late"}))
                .unwrap(),
            Outcome::Consumed
        ));
    }
    #[test]
    fn config_generations_empty_profiles_and_mismatched_kills_are_explicit() {
        let mut s = State::default();
        let first = s.next_config();
        s.begin("first".into(), Operation::Config(first));
        let second = s.next_config();
        s.begin("second".into(), Operation::Config(second));
        s.receive(
            "get_daemon_config_response",
            &json!({"requestId":"first","config":{"terminalProfiles":[]}}),
        )
        .unwrap();
        assert!(s.profiles.is_none());
        assert!(s.config_pending);
        s.receive(
            "get_daemon_config_response",
            &json!({"requestId":"second","config":{"terminalProfiles":[]}}),
        )
        .unwrap();
        assert!(s.profiles().is_empty());
        s.begin(
            "kill".into(),
            Operation::Kill {
                workspace: "a".into(),
                terminal: "t".into(),
            },
        );
        assert!(s.killing("t"));
        assert!(matches!(
            s.receive(
                "kill_terminal_response",
                &json!({"requestId":"kill","terminalId":"other","success":true})
            )
            .unwrap(),
            Outcome::Consumed
        ));
        assert!(s.error_for(Some("a")).is_some());
    }
    #[test]
    fn inventory_before_create_reply_cannot_move_a_terminal_out_of_its_requested_pane() {
        use crate::split_layout::{SplitIds, SplitLayout, SplitPosition};
        let mut layout = SplitLayout::single("main");
        layout.attach_tab("old");
        let mut ids = SplitIds::for_layout(&layout.root);
        let right = layout
            .split_empty("main", SplitPosition::Right, &mut ids)
            .unwrap();
        layout.adopt_tab("new");
        assert_eq!(layout.find_pane_containing_tab("new").unwrap().id, "main");
        let mut destination = dest();
        destination.pane = right.clone();
        place_created(&mut layout, &destination, "new", true).unwrap();
        assert_eq!(layout.find_pane_containing_tab("new").unwrap().id, right);
        assert_eq!(layout.pane("main").unwrap().tabs, ["old"]);
        layout.focus_pane("main");
        layout.set_focused_tab("main", "old");
        place_created(&mut layout, &destination, "new", false).unwrap();
        assert_eq!(
            layout.focused_pane().unwrap().focused_tab.as_deref(),
            Some("old")
        );
    }
    #[test]
    fn rpc_errors_and_invalid_create_replies_stay_in_the_original_workspace() {
        let mut state = State::default();
        state.begin("reject".into(), Operation::Create(dest()));
        assert!(matches!(
            state
                .receive(
                    "rpc_error",
                    &json!({"requestId":"reject","error":{"message":"Not connected"}})
                )
                .unwrap(),
            Outcome::Consumed
        ));
        assert_eq!(state.error_for(Some("a")).as_deref(), Some("Not connected"));
        assert!(state.error_for(Some("b")).is_none());
        state.begin("wrong".into(), Operation::Create(dest()));
        assert!(matches!(
            state
                .receive(
                    "create_terminal_response",
                    &json!({"requestId":"wrong","terminal":{"id":"t","workspaceId":"b"}})
                )
                .unwrap(),
            Outcome::Consumed
        ));
        assert!(
            state
                .error_for(Some("a"))
                .unwrap()
                .contains("different workspace")
        );
        assert!(!state.creating());
    }
    #[test]
    fn disconnect_does_not_replay_mutations_or_allow_an_old_config_to_win() {
        let mut state = State::default();
        let old = state.next_config();
        state.begin("old-config".into(), Operation::Config(old));
        state.begin("create".into(), Operation::Create(dest()));
        state.disconnected();
        assert!(!state.creating());
        assert!(!state.config_pending);
        let new = state.next_config();
        state.begin("new-config".into(), Operation::Config(new));
        state
            .receive(
                "get_daemon_config_response",
                &json!({"requestId":"new-config","config":{"terminalProfiles":[]}}),
            )
            .unwrap();
        state
            .receive(
                "get_daemon_config_response",
                &json!({"requestId":"old-config","config":{}}),
            )
            .unwrap();
        assert!(state.profiles().is_empty());
        assert!(matches!(
            state
                .receive(
                    "create_terminal_response",
                    &json!({"requestId":"create","terminal":{"id":"t","workspaceId":"a"}})
                )
                .unwrap(),
            Outcome::Created { late: true, .. }
        ));
    }
    #[test]
    fn a_created_terminal_replaces_only_its_launcher_and_keeps_background_focus() {
        use crate::split_layout::{SplitIds, SplitLayout, SplitPosition};
        let mut layout = SplitLayout::single("main");
        layout.attach_tab("old");
        let mut ids = SplitIds::for_layout(&layout.root);
        let pane = layout
            .split_empty("main", SplitPosition::Right, &mut ids)
            .unwrap();
        layout.focus_pane(&pane);
        layout.attach_tab("launcher:1");
        layout.attach_tab("file:dirty");
        layout.focus_pane("main");
        let mut target = dest();
        target.pane = pane.clone();
        target.replace = Some("launcher:1".into());
        place_created(&mut layout, &target, "new", false).unwrap();
        assert_eq!(
            layout.focused_pane().unwrap().focused_tab.as_deref(),
            Some("old")
        );
        assert_eq!(
            layout.pane(&pane).unwrap().focused_tab.as_deref(),
            Some("file:dirty")
        );
        assert_eq!(layout.pane(&pane).unwrap().tabs, ["file:dirty", "new"]);
    }
    #[test]
    fn a_config_push_supersedes_an_older_read_and_keeps_an_explicit_empty_catalog() {
        let mut state = State::default();
        let generation = state.next_config();
        state.begin("read".into(), Operation::Config(generation));
        state.config_changed(&json!({"terminalProfiles":[]}));
        assert!(!state.config_pending);
        assert!(state.profiles().is_empty());
        state
            .receive(
                "get_daemon_config_response",
                &json!({"requestId":"read","config":{}}),
            )
            .unwrap();
        assert!(state.profiles().is_empty());
        state.config_changed(&json!({"terminalProfiles":"invalid"}));
        assert!(state.profiles().is_empty());
        assert!(state.config_error.is_some());
        state.config_changed(&json!({}));
        assert_eq!(state.profiles(), defaults());
        assert!(state.config_error.is_none());
    }
}

//! Host profile edits preserve the persisted objects. Launcher prompt adoption
//! must never be written back by a rename or reorder.
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::collections::VecDeque;

#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub struct Draft {
    pub name: String,
    pub command: String,
    pub args: String,
}
impl Draft {
    pub fn from_profile(profile: &Value) -> Self {
        Self {
            name: profile["name"].as_str().unwrap_or_default().into(),
            command: profile["command"].as_str().unwrap_or_default().into(),
            args: profile["args"]
                .as_array()
                .map(|args| {
                    args.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default(),
        }
    }
    pub fn errors(&self) -> (bool, bool) {
        (trim(&self.name).is_empty(), trim(&self.command).is_empty())
    }
}
fn space(c: char) -> bool {
    matches!(c,'\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}')
}
fn trim(value: &str) -> &str {
    value.trim_matches(space)
}
fn parsed_args(value: &str) -> Option<Value> {
    let args: Vec<_> = value.split(space).filter(|s| !s.is_empty()).collect();
    (!args.is_empty()).then(|| json!(args))
}
#[derive(Clone)]
pub enum Change {
    Add { id: String, draft: Draft },
    Edit { base: Value, draft: Draft },
    Remove(String),
    Move { id: String, offset: isize },
    Hooks(bool),
}
impl Change {
    pub fn patch(&self, profiles: &[Value]) -> Result<Value> {
        if let Self::Hooks(value) = self {
            return Ok(json!({"enableTerminalAgentHooks":value}));
        }
        let mut next = profiles.to_vec();
        match self {
            Self::Add { id, draft } => {
                ensure!(
                    !draft.errors().0 && !draft.errors().1,
                    "Name and command are required."
                );
                ensure!(
                    !next.iter().any(|p| p["id"] == *id),
                    "This profile already exists. Reload before trying again."
                );
                let mut profile =
                    json!({"id":id,"name":trim(&draft.name),"command":trim(&draft.command)});
                if let Some(args) = parsed_args(&draft.args) {
                    profile["args"] = args;
                }
                next.push(profile);
            }
            Self::Edit { base, draft } => {
                ensure!(
                    !draft.errors().0 && !draft.errors().1,
                    "Name and command are required."
                );
                let current = next
                    .iter_mut()
                    .find(|p| p["id"] == base["id"])
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "This profile was removed on the host. Close the editor and reload."
                        )
                    })?;
                let original = Draft::from_profile(base);
                for (field, changed, value) in [
                    (
                        "name",
                        draft.name != original.name,
                        Some(json!(trim(&draft.name))),
                    ),
                    (
                        "command",
                        draft.command != original.command,
                        Some(json!(trim(&draft.command))),
                    ),
                    (
                        "args",
                        draft.args != original.args,
                        parsed_args(&draft.args),
                    ),
                ] {
                    if !changed {
                        continue;
                    }
                    let wanted = value.as_ref().unwrap_or(&Value::Null);
                    ensure!(
                        current[field] == base[field] || &current[field] == wanted,
                        "This field changed on the host. Close the editor and reopen the latest profile before saving."
                    );
                    if let Some(value) = value {
                        current[field] = value;
                    } else {
                        current.as_object_mut().unwrap().remove(field);
                    }
                }
            }
            Self::Remove(id) => {
                let index = next
                    .iter()
                    .position(|p| p["id"] == *id)
                    .ok_or_else(|| anyhow::anyhow!("This profile was removed on the host."))?;
                next.remove(index);
            }
            Self::Move { id, offset } => {
                let index = next
                    .iter()
                    .position(|p| p["id"] == *id)
                    .ok_or_else(|| anyhow::anyhow!("This profile was removed on the host."))?;
                let target = index
                    .checked_add_signed(*offset)
                    .filter(|index| *index < next.len())
                    .ok_or_else(|| anyhow::anyhow!("This profile cannot move further."))?;
                let item = next.remove(index);
                next.insert(target, item);
            }
            Self::Hooks(_) => unreachable!(),
        }
        Ok(json!({"terminalProfiles":next}))
    }
}
struct Write {
    id: String,
    generation: u64,
    editor: Option<u64>,
}
pub enum Outcome {
    Ignored,
    Updated,
    Saved(Option<u64>),
}
#[derive(Default)]
pub struct State {
    pub profiles: Option<Vec<Value>>,
    pub hooks: bool,
    pub error: Option<String>,
    pub needs_reload: bool,
    read: Option<String>,
    read_generation: u64,
    write: Option<Write>,
    retired: VecDeque<String>,
    generation: u64,
}
impl State {
    pub fn busy(&self) -> bool {
        self.write.is_some()
    }
    pub fn loading(&self) -> bool {
        self.read.is_some()
    }
    pub fn writable(&self) -> bool {
        self.profiles.is_some() && !self.busy() && !self.needs_reload
    }
    pub fn begin_read(&mut self, id: String) {
        if let Some(old) = self.read.replace(id) {
            self.retire(old);
        }
        self.read_generation = self.generation;
        self.error = None;
    }
    pub fn begin_write(&mut self, id: String, editor: Option<u64>) {
        self.write = Some(Write {
            id,
            generation: self.generation,
            editor,
        });
        self.error = None;
    }
    fn retire(&mut self, id: String) {
        self.retired.push_back(id);
        if self.retired.len() > 64 {
            self.retired.pop_front();
        }
    }
    pub fn expire(&mut self, id: &str) {
        if self.read.as_deref() == Some(id) {
            self.read = None;
            self.retire(id.into());
            self.error = Some("Unable to load terminal configuration. Try again.".into());
        }
        if self.write.as_ref().is_some_and(|write| write.id == id) {
            self.write = None;
            self.retire(id.into());
            self.needs_reload = true;
            self.error = Some(
                "The host did not confirm this save. Reload its configuration before trying again."
                    .into(),
            );
        }
    }
    pub fn disconnected(&mut self) {
        let ids = self
            .read
            .iter()
            .cloned()
            .chain(self.write.iter().map(|w| w.id.clone()))
            .collect::<Vec<_>>();
        for id in ids {
            self.expire(&id);
        }
        self.needs_reload = true;
    }
    pub fn snapshot(&mut self, config: &Value) -> Result<()> {
        ensure!(
            config.is_object(),
            "The host returned invalid terminal configuration."
        );
        let profiles = match config.get("terminalProfiles") {
            None => serde_json::to_value(crate::terminal_lifecycle::defaults())?
                .as_array()
                .unwrap()
                .clone(),
            Some(profiles) => {
                let list = profiles.as_array().ok_or_else(|| {
                    anyhow::anyhow!("The host returned invalid terminal profiles.")
                })?;
                for profile in list {
                    serde_json::from_value::<crate::terminal_lifecycle::Profile>(profile.clone())?;
                }
                list.clone()
            }
        };
        self.profiles = Some(profiles);
        self.hooks = config["enableTerminalAgentHooks"] == true;
        self.generation += 1;
        self.needs_reload = false;
        self.error = None;
        if let Some(read) = self.read.take() {
            self.retire(read);
        }
        Ok(())
    }
    pub fn receive(&mut self, kind: &str, payload: &Value) -> Result<Outcome> {
        if kind == "status" && payload["status"] == "daemon_config_changed" {
            self.snapshot(&payload["config"])?;
            return Ok(Outcome::Updated);
        }
        let Some(id) = payload["requestId"].as_str() else {
            return Ok(Outcome::Ignored);
        };
        let read = self.read.as_deref() == Some(id);
        let write = self.write.as_ref().is_some_and(|w| w.id == id);
        if !read && !write {
            return Ok(if self.retired.iter().any(|seen| seen == id) {
                Outcome::Updated
            } else {
                Outcome::Ignored
            });
        }
        if kind != "rpc_error"
            && kind
                != if read {
                    "get_daemon_config_response"
                } else {
                    "set_daemon_config_response"
                }
        {
            return Ok(Outcome::Ignored);
        }
        let pending = if write {
            self.write.take()
        } else {
            self.read = None;
            None
        };
        self.retire(id.into());
        if read && self.read_generation != self.generation {
            return Ok(Outcome::Updated);
        }
        if kind == "rpc_error" || payload["error"].as_str().is_some() {
            self.error = Some(
                payload["error"]
                    .as_str()
                    .or_else(|| payload["error"]["message"].as_str())
                    .unwrap_or("Unable to save terminal configuration.")
                    .into(),
            );
            return Ok(Outcome::Updated);
        }
        if let Some(write) = pending {
            if self.generation == write.generation {
                self.snapshot(&payload["config"])?;
            }
            Ok(Outcome::Saved(write.editor))
        } else {
            self.snapshot(&payload["config"])?;
            Ok(Outcome::Updated)
        }
    }
    pub fn patch(&self, change: &Change) -> Result<Value> {
        ensure!(
            self.writable(),
            "Load the latest host configuration before saving."
        );
        change.patch(self.profiles.as_deref().unwrap())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn editing_one_field_preserves_unknown_keys_and_exact_unedited_argument_boundaries() {
        let base = json!({"id":"a","name":"Old","command":"pi","args":["-c","a b"],"icon":"grok","future":{"a":1}});
        let mut draft = Draft::from_profile(&base);
        draft.name = "  New  ".into();
        let patch = Change::Edit {
            base: base.clone(),
            draft,
        }
        .patch(&[base.clone()])
        .unwrap();
        let mut expected = base;
        expected["name"] = "New".into();
        assert_eq!(patch, json!({"terminalProfiles":[expected]}));
    }
    #[test]
    fn concurrent_unrelated_edits_merge_but_changed_fields_and_deleted_profiles_do_not_get_overwritten()
     {
        let base = json!({"id":"a","name":"Old","command":"pi"});
        let mut latest = base.clone();
        latest["future"] = true.into();
        latest["command"] = "grok".into();
        let mut draft = Draft::from_profile(&base);
        draft.name = "New".into();
        let change = Change::Edit {
            base: base.clone(),
            draft,
        };
        let patch = change.patch(&[latest.clone()]).unwrap();
        assert_eq!(patch["terminalProfiles"][0]["command"], "grok");
        latest["name"] = "Other".into();
        assert!(change.patch(&[latest]).is_err());
        assert!(change.patch(&[]).is_err());
    }
    #[test]
    fn empty_catalog_reordering_and_args_follow_persisted_semantics() {
        let mut state = State::default();
        state.snapshot(&json!({"terminalProfiles":[]})).unwrap();
        assert!(state.profiles.as_ref().unwrap().is_empty());
        let draft = Draft {
            name: " A ".into(),
            command: " pi ".into(),
            args: "\u{feff}--flag\u{a0}x\u{85}y\t{{{prompt}}}".into(),
        };
        let patch = state
            .patch(&Change::Add {
                id: "a".into(),
                draft,
            })
            .unwrap();
        assert_eq!(
            patch["terminalProfiles"][0]["args"],
            json!(["--flag", "x\u{85}y", "{{{prompt}}}"])
        );
        let b = json!({"id":"b","name":"B","command":"b","unknown":12});
        let a = patch["terminalProfiles"][0].clone();
        assert_eq!(
            Change::Move {
                id: "b".into(),
                offset: -1
            }
            .patch(&[a.clone(), b.clone()])
            .unwrap()["terminalProfiles"],
            json!([b, a])
        );
    }
    #[test]
    fn timeout_requires_reload_and_late_save_cannot_close_a_new_editor_or_overwrite_a_push() {
        let mut s = State::default();
        s.snapshot(&json!({})).unwrap();
        s.begin_write("one".into(), Some(4));
        s.expire("one");
        assert!(!s.writable());
        s.begin_read("read".into());
        s.receive(
            "get_daemon_config_response",
            &json!({"requestId":"read","config":{"terminalProfiles":[]}}),
        )
        .unwrap();
        assert!(s.writable());
        assert!(matches!(
            s.receive(
                "set_daemon_config_response",
                &json!({"requestId":"one","config":{}})
            )
            .unwrap(),
            Outcome::Updated
        ));
        assert!(s.profiles.as_ref().unwrap().is_empty());
        s.begin_write("two".into(), Some(5));
        s.snapshot(&json!({"terminalProfiles":[]})).unwrap();
        assert!(matches!(
            s.receive(
                "set_daemon_config_response",
                &json!({"requestId":"two","config":{}})
            )
            .unwrap(),
            Outcome::Saved(Some(5))
        ));
        assert!(s.profiles.as_ref().unwrap().is_empty());
    }
    #[test]
    fn edited_args_match_the_authored_settings_parser() {
        let cases: Value =
            serde_json::from_str(include_str!("testdata/terminal-profile-settings.json")).unwrap();
        for case in cases.as_array().unwrap() {
            assert_eq!(
                parsed_args(case["raw"].as_str().unwrap()).unwrap_or(Value::Null),
                case["args"]
            );
        }
    }
    #[test]
    fn a_profile_push_supersedes_an_older_read() {
        let mut state = State::default();
        state.begin_read("read".into());
        state.snapshot(&json!({"terminalProfiles":[]})).unwrap();
        state
            .receive(
                "get_daemon_config_response",
                &json!({"requestId":"read","config":{}}),
            )
            .unwrap();
        assert!(state.profiles.as_ref().unwrap().is_empty());
    }
}

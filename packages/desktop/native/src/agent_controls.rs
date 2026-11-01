use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct OptionRow {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
    pub icon: Option<String>,
    #[serde(default)]
    pub is_default: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Model {
    #[serde(flatten)]
    pub row: OptionRow,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub is_selectable: Option<bool>,
    #[serde(default)]
    pub is_default: bool,
    #[serde(default)]
    pub thinking_options: Vec<OptionRow>,
    pub default_thinking_option_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub id: String,
    pub provider: String,
    pub cwd: String,
    pub updated_at: Option<String>,
    pub model: Option<String>,
    pub thinking_option_id: Option<String>,
    pub current_mode_id: Option<String>,
    #[serde(default)]
    pub available_modes: Vec<OptionRow>,
    #[serde(default)]
    pub runtime_info: Value,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Control {
    Model,
    Thinking,
    Mode,
}
impl Control {
    pub fn title(self) -> &'static str {
        match self {
            Self::Model => "Model",
            Self::Thinking => "Thinking",
            Self::Mode => "Mode",
        }
    }
    pub fn request(self, agent: &str, value: &str) -> (&'static str, Value) {
        match self {
            Self::Model => (
                "set_agent_model_request",
                json!({"agentId":agent,"modelId":value}),
            ),
            Self::Thinking => (
                "set_agent_thinking_request",
                json!({"agentId":agent,"thinkingOptionId":value}),
            ),
            Self::Mode => (
                "set_agent_mode_request",
                json!({"agentId":agent,"modeId":value}),
            ),
        }
    }
}

#[derive(Default)]
pub struct Controls {
    pub snapshot: Option<Snapshot>,
    pub models: Option<Vec<Model>>,
    pub pending: Option<(String, Control)>,
    pub error: Option<String>,
    pub revision: u64,
}

impl Controls {
    pub fn apply_snapshot(&mut self, agent_id: &str, payload: &Value) -> anyhow::Result<bool> {
        if payload["id"].as_str() != Some(agent_id) {
            return Ok(false);
        }
        let snapshot: Snapshot = serde_json::from_value(payload.clone())?;
        if self.snapshot.as_ref().is_some_and(|old| {
            old.updated_at
                .as_ref()
                .zip(snapshot.updated_at.as_ref())
                .is_some_and(|(a, b)| a > b)
        }) {
            return Ok(true);
        }
        if self.snapshot.as_ref().is_none_or(|old| {
            old.provider != snapshot.provider
                || old.cwd != snapshot.cwd
                || old.model != snapshot.model
                || old.thinking_option_id != snapshot.thinking_option_id
                || old.current_mode_id != snapshot.current_mode_id
                || old.available_modes != snapshot.available_modes
                || old.runtime_info != snapshot.runtime_info
        }) {
            self.revision = self.revision.wrapping_add(1);
        }
        self.snapshot = Some(snapshot);
        Ok(true)
    }
    pub fn apply_models(&mut self, value: &Value) -> anyhow::Result<()> {
        let models: Vec<Model> = serde_json::from_value(value.clone())?;
        let mut ids = std::collections::BTreeSet::new();
        self.models = Some(
            models
                .into_iter()
                .filter(|m| ids.insert(m.row.id.clone()))
                .collect(),
        );
        Ok(())
    }
    fn model_by_id(&self, id: &str) -> Option<&Model> {
        let models = self.models.as_ref()?;
        models
            .iter()
            .find(|m| m.row.id == id)
            .or_else(|| models.iter().find(|m| m.aliases.iter().any(|a| a == id)))
    }
    pub fn model_id(&self) -> Option<&str> {
        let snapshot = self.snapshot.as_ref()?;
        let runtime = snapshot.runtime_info["model"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        runtime
            .and_then(|id| self.model_by_id(id))
            .map(|m| m.row.id.as_str())
            .or_else(|| {
                snapshot
                    .model
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(|id| self.model_by_id(id).map_or(id, |m| m.row.id.as_str()))
            })
            .or(runtime)
            .or_else(|| {
                self.models
                    .as_ref()?
                    .iter()
                    .find(|m| m.is_default)
                    .or_else(|| self.models.as_ref()?.first())
                    .map(|m| m.row.id.as_str())
            })
    }
    fn selected_model(&self) -> Option<&Model> {
        self.model_by_id(self.model_id()?)
    }
    pub fn selected(&self, control: Control) -> Option<&str> {
        let snapshot = self.snapshot.as_ref()?;
        match control {
            Control::Model => self.model_id(),
            Control::Mode => snapshot
                .current_mode_id
                .as_deref()
                .filter(|id| snapshot.available_modes.iter().any(|m| m.id == *id))
                .or_else(|| snapshot.available_modes.first().map(|m| m.id.as_str())),
            Control::Thinking => {
                let model = self.selected_model()?;
                let runtime = snapshot.runtime_info["thinkingOptionId"].as_str();
                let explicit = snapshot
                    .thinking_option_id
                    .as_deref()
                    .filter(|id| *id != "default");
                let resolved = runtime
                    .filter(|id| model.thinking_options.iter().any(|o| o.id == *id))
                    .or(explicit)
                    .or(model.default_thinking_option_id.as_deref())
                    .or_else(|| {
                        model
                            .thinking_options
                            .iter()
                            .find(|o| o.is_default)
                            .map(|o| o.id.as_str())
                    });
                resolved
                    .filter(|id| model.thinking_options.iter().any(|o| o.id == *id))
                    .or_else(|| model.thinking_options.first().map(|o| o.id.as_str()))
                    .or(resolved)
            }
        }
    }
    pub fn options(&self, control: Control) -> Vec<OptionRow> {
        let mut options = match control {
            Control::Model => self
                .models
                .as_ref()
                .map(|models| {
                    models
                        .iter()
                        .filter(|m| m.is_selectable != Some(false))
                        .map(|m| m.row.clone())
                        .collect()
                })
                .unwrap_or_default(),
            Control::Thinking => self
                .selected_model()
                .map(|m| m.thinking_options.clone())
                .unwrap_or_default(),
            Control::Mode => self
                .snapshot
                .as_ref()
                .map(|s| s.available_modes.clone())
                .unwrap_or_default(),
        };
        if control != Control::Model {
            for option in &mut options {
                option.label = control_label(&option.label, control == Control::Thinking);
            }
        }
        options
    }
    pub fn mode_icon(&self) -> &'static str {
        let icon = self
            .options(Control::Mode)
            .into_iter()
            .find(|o| Some(o.id.as_str()) == self.selected(Control::Mode))
            .and_then(|o| o.icon);
        match icon.as_deref() {
            Some("Shield") => "shield",
            Some("ShieldAlert") => "shield-alert",
            Some("ShieldCheck") => "shield-check",
            Some("ShieldEllipsis") => "shield-ellipsis",
            Some("ShieldOff") => "shield-off",
            Some("ShieldPlus") => "shield-plus",
            Some("ShieldQuestionMark") => "shield-question-mark",
            _ => "bot",
        }
    }
    pub fn label(&self, control: Control) -> String {
        let selected = self.selected(control);
        self.options(control)
            .iter()
            .find(|o| Some(o.id.as_str()) == selected)
            .map(|o| {
                if control == Control::Model {
                    o.label.clone()
                } else {
                    control_label(&o.label, control == Control::Thinking)
                }
            })
            .or_else(|| selected.map(str::to_owned))
            .unwrap_or_else(|| control.title().into())
    }
    pub fn acknowledge(&mut self, request: &str, accepted: bool, error: Option<&str>) -> bool {
        if !self.pending.as_ref().is_some_and(|(id, _)| id == request) {
            return false;
        }
        self.pending = None;
        self.error = if accepted {
            None
        } else {
            Some(
                error
                    .unwrap_or("Agent configuration was not accepted")
                    .into(),
            )
        };
        true
    }
}

// Mirrors the app's label formatting; user-provided model labels retain their casing.
fn control_label(label: &str, split_hyphen: bool) -> String {
    let chars: Vec<_> = label.trim().chars().collect();
    let mut spaced = String::new();
    for (i, ch) in chars.iter().copied().enumerate() {
        if ch == '_' || split_hyphen && ch == '-' {
            spaced.push(' ');
            continue;
        }
        if i > 0
            && ch.is_ascii_uppercase()
            && (chars[i - 1].is_ascii_lowercase()
                || chars[i - 1].is_ascii_uppercase()
                    && chars.get(i + 1).is_some_and(char::is_ascii_lowercase))
        {
            spaced.push(' ');
        }
        spaced.push(ch);
    }
    let normalized = spaced
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    if split_hyphen && matches!(normalized.replace(' ', "").as_str(), "xhigh" | "extrahigh") {
        return "Extra high".into();
    }
    let mut chars = normalized.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn state() -> Controls {
        let mut state = Controls::default();
        state.apply_snapshot("a", &json!({"id":"a","provider":"codex","cwd":"/repo","model":"configured","thinkingOptionId":"high","currentModeId":"ask","availableModes":[{"id":"ask","label":"Ask"}],"runtimeInfo":{"model":"runtime-alias","thinkingOptionId":"low"}})).unwrap();
        state.apply_models(&json!([{"id":"runtime","label":"Runtime","aliases":["runtime-alias"],"thinkingOptions":[{"id":"low","label":"Low"},{"id":"high","label":"High"}]},{"id":"configured","label":"Configured"},{"id":"runtime","label":"Duplicate"},{"id":"hidden","label":"Hidden","isSelectable":false}])).unwrap();
        state
    }
    #[test]
    fn runtime_alias_and_thinking_win_without_duplicate_or_unselectable_options() {
        let state = state();
        assert_eq!(state.model_id(), Some("runtime"));
        assert_eq!(state.selected(Control::Thinking), Some("low"));
        assert_eq!(state.options(Control::Model).len(), 2);
        assert_eq!(state.label(Control::Mode), "Ask");
    }
    #[test]
    fn other_agents_and_late_acks_cannot_replace_configuration() {
        let mut state = state();
        assert!(!state.apply_snapshot("a", &json!({"id":"b"})).unwrap());
        state.pending = Some(("new".into(), Control::Model));
        assert!(!state.acknowledge("old", true, None));
        assert!(state.pending.is_some());
        assert!(state.acknowledge("new", false, Some("Busy")));
        assert_eq!(state.model_id(), Some("runtime"));
        assert_eq!(state.error.as_deref(), Some("Busy"));
    }
    #[test]
    fn future_runtime_model_does_not_hide_configured_model_and_catalog_failure_is_atomic() {
        let mut state = state();
        state.snapshot.as_mut().unwrap().runtime_info = json!({"model":"future-model"});
        assert_eq!(state.model_id(), Some("configured"));
        assert!(state.apply_models(&json!([{"id":"broken"}])).is_err());
        assert_eq!(state.options(Control::Model).len(), 2);
        state.snapshot.as_mut().unwrap().runtime_info = json!({});
        state.snapshot.as_mut().unwrap().model = Some("runtime-alias".into());
        assert_eq!(state.model_id(), Some("runtime"));
        assert_eq!(state.label(Control::Model), "Runtime");
        let (kind, payload) = Control::Thinking.request("a", "high");
        assert_eq!(kind, "set_agent_thinking_request");
        assert_eq!(payload["thinkingOptionId"], "high");
    }
    #[test]
    fn older_snapshots_cannot_overwrite_live_configuration_and_option_defaults_are_used() {
        let mut state = state();
        state.snapshot.as_mut().unwrap().updated_at = Some("2026-11-01T01:00:00Z".into());
        assert!(state.apply_snapshot("a",&json!({"id":"a","provider":"codex","cwd":"/repo","updatedAt":"2026-11-01T00:00:00Z","model":"older"})).unwrap());
        assert_eq!(state.model_id(), Some("runtime"));
        let revision = state.revision;
        let snapshot = state.snapshot.as_ref().unwrap();
        let mut same = json!({"id":"a","provider":snapshot.provider,"cwd":snapshot.cwd,"model":snapshot.model,"thinkingOptionId":snapshot.thinking_option_id,"currentModeId":snapshot.current_mode_id,"availableModes":[{"id":"ask","label":"Ask"}],"runtimeInfo":snapshot.runtime_info,"updatedAt":"2026-11-01T01:00:01Z"});
        state.apply_snapshot("a", &same).unwrap();
        assert_eq!(state.revision, revision);
        same["currentModeId"] = "changed".into();
        state.apply_snapshot("a", &same).unwrap();
        assert_eq!(state.revision, revision + 1);
        state.snapshot.as_mut().unwrap().runtime_info = json!({});
        state.snapshot.as_mut().unwrap().model = Some("runtime".into());
        state.snapshot.as_mut().unwrap().thinking_option_id = None;
        state.models.as_mut().unwrap()[0].thinking_options[1].is_default = true;
        assert_eq!(state.selected(Control::Thinking), Some("high"));
        assert_eq!(control_label("Always Ask", false), "Always ask");
        assert_eq!(
            control_label("bypassPermissions", false),
            "Bypass permissions"
        );
        assert_eq!(control_label("extra_high", true), "Extra high");
    }
}

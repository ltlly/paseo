//! Command Center state, independent of a window or transport. File results
//! retain daemon order; only built-in navigation rows use client-side ranking.
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    All,
    Files,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Target {
    File(String),
    Workspace(String),
    Agent(String),
    Action(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub target: Target,
    pub icon: String,
    pub keywords: Vec<String>,
    pub activity: i64,
    pub query_only: bool,
}

impl Row {
    pub fn file(path: &str) -> Self {
        let path = path.replace('\\', "/");
        let (directory, name) = path.rsplit_once('/').unwrap_or(("", &path));
        Self {
            id: format!("file:{path}"),
            title: name.into(),
            subtitle: directory.into(),
            target: Target::File(path),
            icon: String::new(),
            keywords: vec![],
            activity: 0,
            query_only: false,
        }
    }
    pub fn section(&self) -> &'static str {
        match self.target {
            Target::Action(_) => "Actions",
            Target::Workspace(_) => "Workspaces",
            Target::Agent(_) => "Agents",
            Target::File(_) => "Files",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Search {
    pub scope: Scope,
    pub query: String,
    pub files: Vec<Row>,
    pub loading: bool,
    pub error: Option<String>,
    pub generation: u64,
    pub active: Option<String>,
}
impl Search {
    pub fn reopen(&mut self, scope: Scope) -> u64 {
        self.scope = scope;
        self.query.clear();
        self.files.clear();
        self.active = None;
        self.begin()
    }
    pub fn close(&mut self) {
        self.generation += 1;
        self.loading = false;
        self.files.clear();
        self.active = None;
    }
    pub fn new(scope: Scope) -> Self {
        Self {
            scope,
            query: String::new(),
            files: vec![],
            loading: false,
            error: None,
            generation: 0,
            active: None,
        }
    }
    pub fn enabled(&self) -> bool {
        self.scope == Scope::Files || !crate::command_center_match::query_is_empty(&self.query)
    }
    pub fn begin(&mut self) -> u64 {
        self.generation += 1;
        self.loading = self.enabled();
        self.error = None;
        if !self.enabled() {
            self.files.clear();
        }
        self.generation
    }
    pub fn set_query(&mut self, query: String) -> Option<u64> {
        if query == self.query {
            return None;
        }
        self.query = query;
        self.active = None;
        Some(self.begin())
    }
    pub fn accept(&mut self, generation: u64, payload: &Value) -> bool {
        if generation != self.generation {
            return false;
        }
        self.loading = false;
        self.error = payload["error"].as_str().map(str::to_owned);
        self.files = if self.error.is_some() {
            vec![]
        } else {
            payload["entries"]
                .as_array()
                .map(|entries| {
                    entries
                        .iter()
                        .filter_map(|entry| entry["path"].as_str())
                        .map(Row::file)
                        .collect()
                })
                .unwrap_or_default()
        };
        true
    }
    pub fn fail(&mut self, generation: u64, message: String) -> bool {
        if generation != self.generation {
            return false;
        }
        self.loading = false;
        self.error = Some(message);
        self.files.clear();
        true
    }
    pub fn select_first(&mut self, rows: &[Row]) {
        if self.scope == Scope::Files && self.loading {
            return;
        }
        if !self
            .active
            .as_ref()
            .is_some_and(|id| rows.iter().any(|row| &row.id == id))
        {
            self.active = rows.first().map(|row| row.id.clone());
        }
    }
    pub fn move_selection(&mut self, rows: &[Row], direction: i32) -> bool {
        if rows.is_empty() || (self.scope == Scope::Files && self.loading) {
            return false;
        }
        let current = self
            .active
            .as_ref()
            .and_then(|id| rows.iter().position(|row| &row.id == id));
        let start = current
            .map(|i| i as i32)
            .unwrap_or(if direction < 0 { 0 } else { -1 });
        self.active = Some(
            rows[(start + direction).rem_euclid(rows.len() as i32) as usize]
                .id
                .clone(),
        );
        true
    }
    pub fn rows(&self, builtins: &[Row]) -> Vec<Row> {
        if self.scope == Scope::Files {
            return self.files.clone();
        }
        let mut output = vec![];
        for section in ["Actions", "Workspaces", "Agents"] {
            let mut candidates: Vec<_> = builtins
                .iter()
                .filter(|row| row.section() == section)
                .filter(|row| {
                    !row.query_only || !crate::command_center_match::query_is_empty(&self.query)
                })
                .filter_map(|row| {
                    crate::command_center_match::score_fields(
                        &self.query,
                        &[&row.title, &row.subtitle],
                        &row.keywords,
                    )
                    .map(|score| (row, score))
                })
                .collect();
            candidates.sort_by(|(a, sa), (b, sb)| {
                let comparison = if section == "Actions" {
                    sa.cmp(sb)
                } else {
                    (sa.0, sa.1.tier).cmp(&(sb.0, sb.1.tier))
                };
                comparison.then(b.activity.cmp(&a.activity))
            });
            let limit = if section != "Actions"
                && crate::command_center_match::query_is_empty(&self.query)
            {
                5
            } else {
                usize::MAX
            };
            output.extend(
                candidates
                    .into_iter()
                    .take(limit)
                    .map(|(row, _)| row.clone()),
            );
        }
        output.extend(self.files.clone());
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn reopening_the_same_query_never_adopts_a_closed_modal_reply() {
        let mut state = Search::new(Scope::Files);
        let old = state.reopen(Scope::Files);
        state.close();
        let current = state.reopen(Scope::Files);
        assert!(current > old);
        assert!(!state.accept(old, &json!({"entries":[{"path":"old-workspace.txt"}]})));
        assert!(state.loading);
        assert!(state.accept(current, &json!({"entries":[{"path":"current.txt"}]})));
        assert!(!state.fail(old, "late error".into()));
        assert_eq!(state.files[0].title, "current.txt");
        assert!(state.error.is_none());
    }
    #[test]
    fn file_paths_match_the_authored_name_and_directory_projection() {
        let file = Row::file("src\\app\\main.rs");
        assert_eq!(
            (file.title.as_str(), file.subtitle.as_str()),
            ("main.rs", "src/app")
        );
        assert_eq!(file.target, Target::File("src/app/main.rs".into()));
        let file = Row::file("README.md");
        assert_eq!(file.title, "README.md");
        assert!(file.subtitle.is_empty());
    }
    #[test]
    fn a_query_keeps_results_but_stale_replies_cannot_settle_loading() {
        let mut state = Search::new(Scope::Files);
        let first = state.begin();
        assert!(state.accept(
            first,
            &json!({"entries":[{"path":"src\\first.ts"},{"path":"last.rs"}]})
        ));
        let rows = state.rows(&[]);
        state.select_first(&rows);
        assert_eq!(state.active.as_deref(), Some("file:src/first.ts"));
        let next = state.set_query("different".into()).unwrap();
        assert_eq!(state.files.len(), 2);
        assert_eq!(state.active, None);
        assert!(!state.move_selection(&rows, 1));
        assert!(!state.accept(first, &json!({"entries":[]})));
        assert!(state.loading);
        assert!(!state.fail(first, "old failure".into()));
        assert!(state.accept(next, &json!({"entries":[{"path":"different.txt"}]})));
        let rows = state.rows(&[]);
        state.select_first(&rows);
        assert_eq!(state.active.as_deref(), Some("file:different.txt"));
    }
    #[test]
    fn root_empty_query_has_no_file_demand_and_file_navigation_wraps() {
        let mut state = Search::new(Scope::All);
        state.begin();
        assert!(!state.loading);
        state.scope = Scope::Files;
        let generation = state.begin();
        assert!(state.loading);
        state.accept(generation, &json!({"entries":[{"path":"a"},{"path":"b"}]}));
        let rows = state.rows(&[]);
        state.select_first(&rows);
        assert!(state.move_selection(&rows, -1));
        assert_eq!(state.active.as_deref(), Some("file:b"));
        assert!(state.move_selection(&rows, 1));
        assert_eq!(state.active.as_deref(), Some("file:a"));
    }
}

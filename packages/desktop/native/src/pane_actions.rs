//! In-flight action state shared by the schedules and plugins panes.
//!
//! The pane owns request/response matching; this owns the per-target state
//! transitions so the regressions that matter — an unknown-outcome disconnect
//! fails visibly without automatic retry, and an error is scoped to its own
//! request — stay testable without a window.

use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PendingAction<K> {
    pub kind: K,
    pub request_id: String,
    pub error: Option<String>,
}

pub(crate) struct PendingActions<K> {
    entries: BTreeMap<String, PendingAction<K>>,
}

impl<K> Default for PendingActions<K> {
    fn default() -> Self {
        Self {
            entries: BTreeMap::new(),
        }
    }
}

impl<K: Copy> PendingActions<K> {
    pub fn start(&mut self, id: impl Into<String>, kind: K, request_id: String) {
        self.entries.insert(
            id.into(),
            PendingAction {
                kind,
                request_id,
                error: None,
            },
        );
    }

    /// A local send failure keeps the row actionable without an in-flight
    /// request.
    pub fn fail(&mut self, id: &str, error: String) {
        if let Some(entry) = self.entries.get_mut(id) {
            entry.request_id.clear();
            entry.error = Some(error);
        }
    }

    /// An acknowledged outcome removes the pending state.
    pub fn complete(&mut self, id: &str) {
        self.entries.remove(id);
    }

    /// A disconnect is an unknown outcome, never an automatic retry: every
    /// in-flight action fails visibly and waits for a manual retry.
    pub fn fail_all_unknown(&mut self) {
        for entry in self.entries.values_mut() {
            entry.request_id.clear();
            entry.error = Some("The host disconnected before this finished.".to_owned());
        }
    }

    pub fn get(&self, id: &str) -> Option<&PendingAction<K>> {
        self.entries.get(id)
    }

    pub fn find_by_request(&self, request_id: &str) -> Option<(&str, K)> {
        self.entries
            .iter()
            .find(|(_, entry)| entry.request_id == request_id)
            .map(|(id, entry)| (id.as_str(), entry.kind))
    }

    pub fn retain_known(&mut self, known: impl Fn(&str) -> bool) {
        self.entries.retain(|id, _| known(id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Kind {
        One,
        Two,
    }

    #[test]
    fn an_error_is_scoped_to_its_own_request() {
        let mut actions = PendingActions::default();
        actions.start("a", Kind::One, "req-a".into());
        actions.start("b", Kind::Two, "req-b".into());
        let (failed, kind) = actions
            .find_by_request("req-a")
            .map(|(id, kind)| (id.to_owned(), kind))
            .expect("owned request");
        actions.fail(&failed, "boom".into());
        assert_eq!(kind, Kind::One);
        assert_eq!(actions.get("a").unwrap().error.as_deref(), Some("boom"));
        assert_eq!(actions.get("b").unwrap().error, None);
        // A failed entry has no in-flight request left to claim.
        assert_eq!(actions.find_by_request("req-a"), None);
    }

    #[test]
    fn completion_removes_the_pending_state() {
        let mut actions = PendingActions::default();
        actions.start("a", Kind::One, "req-a".into());
        actions.complete("a");
        assert!(actions.get("a").is_none());
    }

    #[test]
    fn disconnect_fails_every_in_flight_action_without_auto_retry() {
        let mut actions = PendingActions::default();
        actions.start("a", Kind::One, "req-a".into());
        actions.start("b", Kind::Two, "req-b".into());
        actions.fail_all_unknown();
        for id in ["a", "b"] {
            let entry = actions.get(id).expect("entry survives the disconnect");
            assert_eq!(
                entry.error.as_deref(),
                Some("The host disconnected before this finished.")
            );
            assert_eq!(
                entry.request_id, "",
                "no request can be claimed after a disconnect"
            );
        }
        // A later stale response for the pre-disconnect request owns nothing.
        assert_eq!(actions.find_by_request("req-a"), None);
    }

    #[test]
    fn stale_entries_are_pruned_when_their_target_disappears() {
        let mut actions = PendingActions::default();
        actions.start("a", Kind::One, "req-a".into());
        actions.start("gone", Kind::Two, "req-b".into());
        actions.retain_known(|id| id != "gone");
        assert!(actions.get("gone").is_none());
        assert!(actions.get("a").is_some());
    }
}

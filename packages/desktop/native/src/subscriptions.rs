use std::collections::{BTreeMap, VecDeque};

use anyhow::{Result, ensure};
use serde_json::Value;

#[derive(Default)]
pub(crate) struct Subscriptions {
    current: BTreeMap<String, String>,
    pending: BTreeMap<String, String>,
    bound: BTreeMap<String, String>,
    releases: VecDeque<String>,
}

impl Subscriptions {
    pub fn reset(&mut self) {
        self.current.clear();
        self.pending.clear();
        self.bound.clear();
        self.releases.clear();
    }
    pub fn can_open(&self) -> Result<()> {
        // Abandoned requests must remain identifiable until their first response:
        // a late host-assigned ID still needs releasing. Bound this debt explicitly.
        ensure!(
            self.pending.len() < 128,
            "Too many subscriptions are still opening. Reconnect to retry."
        );
        Ok(())
    }
    pub fn open(&mut self, key: &str, request: String) {
        if let Some(id) = self.close(key) {
            self.defer_release(id);
        }
        self.pending.insert(request.clone(), key.into());
        self.current.insert(key.into(), request);
    }
    pub fn close(&mut self, key: &str) -> Option<String> {
        if let Some(request) = self.current.remove(key)
            && let Some(id) = self.bound.remove(&request)
        {
            return Some(id);
        }
        None
    }
    pub fn defer_release(&mut self, id: String) {
        self.releases.push_back(id);
    }
    pub fn next_release(&mut self) -> Option<String> {
        self.releases.pop_front()
    }
    pub fn receive(&mut self, payload: &Value) -> bool {
        if let Some(request) = payload["requestId"].as_str()
            && let Some(key) = self.pending.remove(request)
        {
            let current = self.current.get(&key).is_some_and(|id| id == request);
            if let Some(id) = payload["subscriptionId"].as_str() {
                if current {
                    self.bound.insert(request.into(), id.into());
                } else {
                    self.releases.push_back(id.into());
                }
            } else if current {
                self.current.remove(&key);
            }
            return current;
        }
        payload["subscriptionId"]
            .as_str()
            .is_none_or(|id| self.bound.values().any(|bound| bound == id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn disconnect_resets_physical_owners_without_releasing_them_on_a_new_socket() {
        let mut subscriptions = Subscriptions::default();
        subscriptions.open("terminal", "old".into());
        subscriptions.receive(&json!({"requestId":"old","subscriptionId":"old-owner"}));
        subscriptions.close("terminal");
        subscriptions.defer_release("old-owner".into());
        subscriptions.reset();
        assert_eq!(subscriptions.next_release(), None);
        assert!(!subscriptions.receive(&json!({"subscriptionId":"old-owner"})));
        subscriptions.open("terminal", "new".into());
        assert!(subscriptions.receive(&json!({"requestId":"new","subscriptionId":"new-owner"})));
    }

    #[test]
    fn switching_before_binding_releases_late_owners_and_rejects_their_updates() {
        let mut subscriptions = Subscriptions::default();
        subscriptions.open("directory", "old".into());
        subscriptions.open("directory", "new".into());
        assert!(!subscriptions.receive(&json!({"requestId":"old","subscriptionId":"old-owner"})));
        assert_eq!(subscriptions.next_release().as_deref(), Some("old-owner"));
        assert!(subscriptions.receive(&json!({"requestId":"new","subscriptionId":"new-owner"})));
        assert!(!subscriptions.receive(&json!({"subscriptionId":"old-owner"})));
        assert!(subscriptions.receive(&json!({"subscriptionId":"new-owner"})));
        assert_eq!(
            subscriptions.close("directory").as_deref(),
            Some("new-owner")
        );
        assert!(!subscriptions.receive(&json!({"subscriptionId":"new-owner"})));
    }

    #[test]
    fn independent_terminal_owners_and_failed_requests_do_not_replace_each_other() {
        let mut subscriptions = Subscriptions::default();
        subscriptions.open("a", "a1".into());
        subscriptions.open("b", "b1".into());
        subscriptions.close("a");
        assert!(!subscriptions.receive(&json!({"requestId":"a1","subscriptionId":"a-owner"})));
        assert_eq!(subscriptions.next_release().as_deref(), Some("a-owner"));
        assert!(subscriptions.receive(&json!({"requestId":"b1","error":"failed"})));
        assert!(subscriptions.current.is_empty());
        assert!(subscriptions.pending.is_empty());
        assert!(subscriptions.bound.is_empty());
        assert!(subscriptions.receive(&json!({"requestId":"ordinary-query","result":true})));
    }

    #[test]
    fn unanswered_request_debt_is_bounded_even_after_close() {
        let mut subscriptions = Subscriptions::default();
        for n in 0..128 {
            subscriptions.can_open().unwrap();
            subscriptions.open("directory", n.to_string());
        }
        subscriptions.close("directory");
        assert!(subscriptions.can_open().is_err());
        assert!(!subscriptions.receive(&json!({"requestId":"0","error":"failed"})));
        subscriptions.can_open().unwrap();
    }
}

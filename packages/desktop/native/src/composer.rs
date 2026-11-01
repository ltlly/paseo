use crate::editing::Editing;
use uuid::Uuid;

pub struct Attempt {
    pub request_id: String,
    pub message_id: String,
    pub text: String,
    revision: u64,
}

#[derive(Default)]
pub struct ComposerState {
    pub editing: Editing,
    pub pending: Option<Attempt>,
    pub error: Option<String>,
    pub queued: bool,
    retry: Option<Attempt>,
}

impl ComposerState {
    pub fn can_send(&self) -> bool {
        self.pending.is_none()
            && self.editing.marked.is_none()
            && !self.editing.text.trim().is_empty()
    }

    pub fn begin(&mut self, request_id: String) -> Option<&Attempt> {
        if !self.can_send() {
            return None;
        }
        let text = self.editing.text.clone();
        // A transport failure may arrive after delivery. Reuse the daemon's dedup key on retry.
        let message_id = self
            .retry
            .take()
            .filter(|a| a.text == text)
            .map(|a| a.message_id)
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        self.pending = Some(Attempt {
            request_id,
            message_id,
            text,
            revision: self.editing.revision,
        });
        self.error = None;
        self.queued = false;
        self.pending.as_ref()
    }

    pub fn acknowledge(
        &mut self,
        request_id: &str,
        accepted: bool,
        queued: bool,
        error: Option<String>,
    ) -> bool {
        if self
            .pending
            .as_ref()
            .is_none_or(|a| a.request_id != request_id)
        {
            return false;
        }
        let attempt = self.pending.take().unwrap();
        if accepted {
            // An ack is allowed to clear only the exact draft that was submitted.
            if attempt.revision == self.editing.revision {
                self.editing.clear();
            }
            self.retry = None;
            self.error = None;
            self.queued = queued;
        } else {
            self.retry = Some(attempt);
            self.error =
                Some(error.unwrap_or_else(|| {
                    "Message was not accepted. Your draft is preserved.".into()
                }));
        }
        true
    }

    pub fn disconnected(&mut self) {
        if let Some(attempt) = self.pending.take() {
            self.retry = Some(attempt);
            self.error = Some("Host disconnected. Reconnect before retrying this draft.".into());
        }
    }

    pub fn is_disposable(&self) -> bool {
        self.editing.text.is_empty() && self.pending.is_none() && self.error.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn late_ack_cannot_clear_a_newer_draft_or_another_request() {
        let mut s = ComposerState::default();
        s.editing.replace(None, "first", false, None);
        s.begin("one".into()).unwrap();
        assert!(s.begin("duplicate".into()).is_none());
        s.editing.replace(None, " second", false, None);
        assert!(!s.acknowledge("other", true, false, None));
        assert!(s.acknowledge("one", true, false, None));
        assert_eq!(s.editing.text, "first second");
        assert!(s.pending.is_none());
    }

    #[test]
    fn accepted_snapshot_clears_and_queued_is_not_completion() {
        let mut s = ComposerState::default();
        s.editing.replace(None, "draft", false, None);
        s.begin("one".into()).unwrap();
        s.acknowledge("one", true, true, None);
        assert!(s.editing.text.is_empty());
        assert!(s.queued);
        assert!(s.is_disposable());
    }

    #[test]
    fn failed_delivery_preserves_text_and_retry_identity() {
        let mut s = ComposerState::default();
        s.editing.replace(None, "draft", false, None);
        let id = s.begin("one".into()).unwrap().message_id.clone();
        s.disconnected();
        assert_eq!(s.editing.text, "draft");
        assert!(s.error.is_some());
        assert_eq!(s.begin("two".into()).unwrap().message_id, id);
        s.acknowledge("two", false, false, Some("Rejected".into()));
        s.editing.replace(None, " changed", false, None);
        assert_ne!(s.begin("three".into()).unwrap().message_id, id);
    }

    #[test]
    fn empty_and_composing_drafts_cannot_send() {
        let mut s = ComposerState::default();
        s.editing.replace(None, "  \n", false, None);
        assert!(!s.can_send());
        s.editing.replace(None, "ni", true, None);
        assert!(!s.can_send());
        s.editing.replace(None, "你", false, None);
        assert!(s.can_send());
    }
}

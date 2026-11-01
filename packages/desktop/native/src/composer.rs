use crate::{
    attachments::{DraftAttachment, ReadyAttachment},
    editing::Editing,
};
use uuid::Uuid;

#[derive(Clone)]
pub struct Attempt {
    pub request_id: String,
    pub message_id: String,
    pub text: String,
    revision: u64,
    pub attachments: Vec<(String, ReadyAttachment)>,
}

struct Retry {
    message_id: String,
    text: String,
    attachment_ids: Vec<String>,
}
impl From<Attempt> for Retry {
    fn from(a: Attempt) -> Self {
        Self {
            message_id: a.message_id,
            text: a.text,
            attachment_ids: a.attachments.into_iter().map(|(id, _)| id).collect(),
        }
    }
}

#[derive(serde::Serialize)]
pub struct Message<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    #[serde(rename = "requestId")]
    request_id: &'a str,
    #[serde(rename = "agentId")]
    agent_id: &'a str,
    #[serde(rename = "messageId")]
    message_id: &'a str,
    text: &'a str,
    images: Vec<&'a crate::attachments::ImageData>,
    attachments: Vec<&'a crate::attachments::UploadedFile>,
}
impl Attempt {
    pub fn message<'a>(&'a self, agent_id: &'a str) -> Message<'a> {
        Message {
            kind: "send_agent_message_request",
            request_id: &self.request_id,
            agent_id,
            message_id: &self.message_id,
            text: &self.text,
            images: self
                .attachments
                .iter()
                .filter_map(|(_, a)| match a {
                    ReadyAttachment::Image(image) => Some(image.as_ref()),
                    _ => None,
                })
                .collect(),
            attachments: self
                .attachments
                .iter()
                .filter_map(|(_, a)| match a {
                    ReadyAttachment::File(file) => Some(file.as_ref()),
                    _ => None,
                })
                .collect(),
        }
    }
}

#[derive(Default)]
pub struct ComposerState {
    pub editing: Editing,
    pub pending: Option<Attempt>,
    pub error: Option<String>,
    pub queued: bool,
    pub attachments: Vec<DraftAttachment>,
    retry: Option<Retry>,
}

impl ComposerState {
    pub fn can_send(&self) -> bool {
        self.pending.is_none()
            && self.editing.marked.is_none()
            && (!self.editing.text.trim().is_empty() || !self.attachments.is_empty())
            && self
                .attachments
                .iter()
                .all(|a| a.ready.is_some() && a.error.is_none())
    }

    pub fn begin(&mut self, request_id: String) -> Option<&Attempt> {
        if !self.can_send() {
            return None;
        }
        let text = self.editing.text.clone();
        let attachments: Vec<_> = self
            .attachments
            .iter()
            .map(|a| (a.id.clone(), a.ready.as_ref().unwrap().clone()))
            .collect();
        // A transport failure may arrive after delivery. Reuse the daemon's dedup key on retry.
        let message_id = self
            .retry
            .take()
            .filter(|a| {
                a.text == text
                    && a.attachment_ids
                        .iter()
                        .eq(attachments.iter().map(|(id, _)| id))
            })
            .map(|a| a.message_id)
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        self.pending = Some(Attempt {
            request_id,
            message_id,
            text,
            attachments,
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
            self.attachments
                .retain(|a| !attempt.attachments.iter().any(|(id, _)| *id == a.id));
            self.retry = None;
            self.error = None;
            self.queued = queued;
        } else {
            self.retry = Some(attempt.into());
            self.error =
                Some(error.unwrap_or_else(|| {
                    "Message was not accepted. Your draft is preserved.".into()
                }));
        }
        true
    }

    pub fn disconnected(&mut self) {
        if let Some(attempt) = self.pending.take() {
            self.retry = Some(attempt.into());
            self.error = Some("Host disconnected. Reconnect before retrying this draft.".into());
        }
    }

    pub fn is_disposable(&self) -> bool {
        self.editing.text.is_empty()
            && self.attachments.is_empty()
            && self.pending.is_none()
            && self.error.is_none()
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
    #[test]
    fn attachment_only_send_keeps_new_attachments_and_retry_identity_tracks_content() {
        use crate::attachments::{DraftAttachment, ReadyAttachment, UploadedFile};
        use std::{path::PathBuf, sync::Arc};
        let mut state = ComposerState::default();
        let mut attachment = DraftAttachment::new(PathBuf::from("/selected/report.txt"));
        let first_id = attachment.id.clone();
        attachment.ready = Some(ReadyAttachment::File(Arc::new(UploadedFile {
            kind: "uploaded_file".into(),
            id: "upload-a".into(),
            file_name: "report.txt".into(),
            mime_type: "application/octet-stream".into(),
            size: 3,
            path: "/daemon/uploads/report.txt".into(),
        })));
        state.attachments.push(attachment);
        assert!(state.can_send());
        let message_id = state.begin("one".into()).unwrap().message_id.clone();
        state.acknowledge("one", false, false, Some("Network failure".into()));
        assert_eq!(state.begin("two".into()).unwrap().message_id, message_id);
        state
            .attachments
            .push(DraftAttachment::new(PathBuf::from("/selected/later.txt")));
        state.acknowledge("two", true, false, None);
        assert_eq!(state.attachments.len(), 1);
        assert_ne!(state.attachments[0].id, first_id);
        assert!(!state.can_send());
        assert!(!state.is_disposable());
    }
    #[test]
    fn image_only_payload_serializes_original_bytes_and_removing_a_failed_image_releases_it() {
        use crate::attachments::{DraftAttachment, ImageData, ReadyAttachment};
        use std::{path::PathBuf, sync::Arc};
        let image = Arc::new(ImageData {
            data: "aGVsbG8=".into(),
            mime_type: "image/png".into(),
        });
        let weak = Arc::downgrade(&image);
        let mut state = ComposerState::default();
        let mut a = DraftAttachment::new(PathBuf::from("image.png"));
        a.ready = Some(ReadyAttachment::Image(image));
        state.attachments.push(a);
        let first = state.begin("first".into()).unwrap();
        let first_id = first.message_id.clone();
        let payload = serde_json::to_value(first.message("agent")).unwrap();
        assert_eq!(
            payload["images"],
            serde_json::json!([{"data":"aGVsbG8=","mimeType":"image/png"}])
        );
        assert_eq!(payload["attachments"], serde_json::json!([]));
        assert_eq!(payload["text"], "");
        state.acknowledge("first", false, false, Some("rejected".into()));
        state.attachments.clear();
        assert!(weak.upgrade().is_none());
        state
            .editing
            .replace(None, "different payload", false, None);
        assert_ne!(state.begin("next".into()).unwrap().message_id, first_id);
    }
}

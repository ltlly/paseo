//! Permission cards for the active agent. The board renders the daemon's pending request objects
//! and answers them over the session socket, matching the desktop permission card: allow/deny
//! actions, a per-action pending state, and an inline error when the reply is rejected.
use crate::{
    message_style::MessageStyle,
    question_form::QuestionForm,
    settings_view::{SharedSettings, active_palette},
    theme::{font_size, radius},
};
use gpui::{Context, MouseButton, Render, SharedString, Window, div, prelude::*, px, rgb};
use paseo_desktop::client::Connection;
use serde_json::{Value, json};
use std::time::Duration;
use std::{collections::BTreeMap, sync::Arc};

const RESPONSE_TIMEOUT: Duration = Duration::from_secs(15);
const RESPONSE_UNCONFIRMED: &str = "The host has not confirmed the response yet. It may still be processing; retry only if the request stays open.";

fn resolution_matches_agent(agent_id: &str, payload: &Value) -> bool {
    payload["requestId"].as_str().is_some()
        && payload
            .get("agentId")
            .and_then(Value::as_str)
            .is_none_or(|id| id == agent_id)
}

fn error_matches_reply(responding: Option<&(String, String)>, payload: &Value) -> bool {
    payload["requestType"].as_str() == Some("agent_permission_response")
        && payload["requestId"]
            .as_str()
            .is_some_and(|id| responding.is_some_and(|(current, _)| current == id))
}

pub struct PermissionBoard {
    agent_id: String,
    connection: Arc<Connection>,
    pending: Vec<Value>,
    responding: Option<(String, String)>,
    error: Option<String>,
    generation: u64,
    questions: BTreeMap<String, gpui::Entity<QuestionForm>>,
    shared: SharedSettings,
}

impl PermissionBoard {
    pub fn new(
        agent_id: String,
        connection: Arc<Connection>,
        shared: SharedSettings,
        _cx: &mut Context<Self>,
    ) -> Self {
        Self {
            agent_id,
            connection,
            pending: Vec::new(),
            responding: None,
            error: None,
            generation: 0,
            questions: BTreeMap::new(),
            shared,
        }
    }

    pub fn set_pending(&mut self, agent_id: &str, pending: Vec<Value>, cx: &mut Context<Self>) {
        if agent_id != self.agent_id {
            return;
        }
        if self.pending == pending {
            return;
        }
        if let Some((request_id, _)) = &self.responding
            && !pending
                .iter()
                .any(|request| request["id"].as_str() == Some(request_id.as_str()))
        {
            self.responding = None;
            self.error = None;
        }
        let question_ids = pending
            .iter()
            .filter(|request| request["kind"] == "question")
            .filter_map(|request| request["id"].as_str().map(str::to_owned))
            .collect::<Vec<_>>();
        self.questions.retain(|id, _| question_ids.contains(id));
        for request in &pending {
            if request["kind"] != "question" {
                continue;
            }
            let Some(id) = request["id"].as_str() else {
                continue;
            };
            if self.questions.contains_key(id) {
                continue;
            }
            let id = id.to_owned();
            let input = request.get("input").cloned().unwrap_or_else(|| json!({}));
            if crate::question_form::parse(&input).is_none() {
                continue;
            }
            let board = cx.entity();
            let shared = self.shared.clone();
            let form = cx.new(|cx| QuestionForm::new(id.clone(), input, board, shared, cx));
            self.questions.insert(id, form);
        }
        self.pending = pending;
        cx.notify();
    }

    /// Handle a session message. Returns true when the message belonged to a permission response.
    pub fn receive(&mut self, kind: &str, payload: &Value, cx: &mut Context<Self>) -> bool {
        match kind {
            "agent_permission_resolved" => {
                if !resolution_matches_agent(&self.agent_id, payload) {
                    return false;
                }
                let Some(request_id) = payload["requestId"].as_str() else {
                    return false;
                };
                let before = self.pending.len();
                self.pending
                    .retain(|request| request["id"].as_str() != Some(request_id));
                if let Some((responding, _)) = &self.responding
                    && responding == request_id
                {
                    self.responding = None;
                    self.error = None;
                }
                if self.pending.len() != before {
                    cx.notify();
                }
                true
            }
            "rpc_error" => {
                if !error_matches_reply(self.responding.as_ref(), payload) {
                    return false;
                }
                let request_id = payload["requestId"].as_str().unwrap_or("");
                self.responding = None;
                let message = payload["error"]
                    .as_str()
                    .filter(|error| !error.is_empty())
                    .unwrap_or("The host rejected the response.")
                    .to_owned();
                self.error = Some(message.clone());
                self.notify_question_error(request_id, &message, cx);
                cx.notify();
                true
            }
            _ => false,
        }
    }

    fn respond(&mut self, request_id: &str, action: &Value, cx: &mut Context<Self>) {
        if self.responding.is_some() {
            return;
        }
        let behavior = action["behavior"].as_str().unwrap_or("deny");
        let action_id = action["id"].as_str().unwrap_or("");
        let response = if behavior == "allow" {
            json!({ "behavior": "allow", "selectedActionId": action_id })
        } else {
            json!({
                "behavior": "deny",
                "selectedActionId": action_id,
                "message": "Denied by user",
            })
        };
        if let Err(error) = self.send_response(request_id, action_id, response, cx) {
            self.error = Some(format!("Could not send the permission response: {error}"));
            cx.notify();
        }
    }

    /// Answer a question permission. `updated_input` carries the answers (allow); `None` denies.
    pub fn answer_question(
        &mut self,
        request_id: &str,
        action: &str,
        updated_input: Option<Value>,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        if self.responding.is_some() {
            anyhow::bail!("A permission response is already pending");
        }
        let response = match updated_input {
            Some(updated_input) => json!({ "behavior": "allow", "updatedInput": updated_input }),
            None => json!({ "behavior": "deny", "message": "Dismissed by user" }),
        };
        self.send_response(request_id, action, response, cx)
    }

    fn send_response(
        &mut self,
        request_id: &str,
        action_id: &str,
        response: Value,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        let message = json!({
            "type": "agent_permission_response",
            "agentId": self.agent_id,
            "requestId": request_id,
            "response": response,
        });
        self.connection.send_session_data(&message)?;
        self.error = None;
        let request_id = request_id.to_owned();
        self.responding = Some((request_id.clone(), action_id.to_owned()));
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        cx.notify();
        let timer = cx.background_executor().timer(RESPONSE_TIMEOUT);
        cx.spawn(async move |this, cx| {
            timer.await;
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                if this
                    .responding
                    .as_ref()
                    .is_some_and(|(id, _)| id == &request_id)
                {
                    this.responding = None;
                    this.error = Some(RESPONSE_UNCONFIRMED.into());
                    this.notify_question_error(&request_id, RESPONSE_UNCONFIRMED, cx);
                    cx.notify();
                }
            });
        })
        .detach();
        Ok(())
    }
    fn notify_question_error(&mut self, request_id: &str, error: &str, cx: &mut Context<Self>) {
        if let Some(form) = self.questions.get(request_id).cloned() {
            let error = Some(error.to_owned());
            form.update(cx, |form, cx| form.set_state(None, error, cx));
        }
    }
}

impl Render for PermissionBoard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let style = MessageStyle::resolve(&self.shared.get(), active_palette(&self.shared, window));
        div()
            .flex()
            .flex_col()
            .gap_2()
            .children(self.pending.iter().map(|request| {
                let request_id = request["id"].as_str().unwrap_or("").to_owned();
                if request["kind"] == "question" {
                    return match self.questions.get(&request_id) {
                        Some(form) => form.clone().into_any_element(),
                        None => unreadable_question(request, &request_id, &style),
                    };
                }
                let responding = self
                    .responding
                    .as_ref()
                    .is_some_and(|(id, _)| id == &request_id);
                card(
                    request,
                    &request_id,
                    responding,
                    self.error.as_deref(),
                    &cx.entity(),
                    &style,
                )
            }))
            .into_any_element()
    }
}

fn card(
    request: &Value,
    request_id: &str,
    responding: bool,
    error: Option<&str>,
    entity: &gpui::Entity<PermissionBoard>,
    style: &MessageStyle,
) -> gpui::AnyElement {
    let kind = request["kind"].as_str().unwrap_or("other");
    let plan = kind == "plan";
    let title = if plan {
        "Plan".to_owned()
    } else {
        request["title"]
            .as_str()
            .or_else(|| request["name"].as_str())
            .unwrap_or("Permission required")
            .to_owned()
    };
    let description = request["description"].as_str().unwrap_or("");
    let plan_text = request["metadata"]["planText"]
        .as_str()
        .or_else(|| request["input"]["plan"].as_str());
    let actions = actions_for(request, plan);
    let is_question = kind == "question";
    let mut body = div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .text_size(px(font_size::BASE))
                .line_height(px(22.))
                .text_color(rgb(style.palette.foreground))
                .child(title),
        )
        .when(!description.is_empty(), |body| {
            body.child(
                div()
                    .text_size(px(font_size::BASE))
                    .line_height(px(20.))
                    .text_color(rgb(style.palette.foreground_muted))
                    .child(description.to_owned()),
            )
        });
    if is_question {
        body = body.child(
            div()
                .text_size(px(font_size::BASE))
                .line_height(px(20.))
                .text_color(rgb(style.palette.foreground_muted))
                .child("This question needs the desktop app to answer."),
        );
    } else {
        if plan {
            if let Some(text) = plan_text.filter(|text| !text.is_empty()) {
                body = body.child(
                    div()
                        .rounded(px(radius::MD))
                        .border_1()
                        .border_color(rgb(style.palette.border))
                        .bg(rgb(style.palette.surface0))
                        .p_3()
                        .text_size(px(font_size::BASE))
                        .line_height(px(22.))
                        .text_color(rgb(style.palette.foreground))
                        .child(crate::tool_row::prose(text)),
                );
            }
        } else if let Some(detail) = request.get("detail").filter(|detail| !detail.is_null()) {
            body = body.child(
                div()
                    .id("permission-detail")
                    .rounded(px(radius::MD))
                    .border_1()
                    .border_color(rgb(style.palette.border))
                    .overflow_hidden()
                    .max_h(px(200.))
                    .overflow_y_scroll()
                    .child(crate::tool_row::detail_element(detail, None, style)),
            );
        }
        if !is_question {
            body = body.child(
                div()
                    .mt_1()
                    .mb_1()
                    .text_size(px(font_size::BASE))
                    .text_color(rgb(style.palette.foreground_muted))
                    .child("Do you want to proceed?"),
            );
        }
        body =
            body.child(div().flex().flex_wrap().items_center().gap_2().children(
                actions.into_iter().map(|action| {
                    action_button(action, request_id, responding, entity.clone(), style)
                }),
            ));
    }
    if let Some(error) = error {
        body = body.child(
            div()
                .text_size(px(font_size::SM))
                .line_height(px(18.))
                .text_color(rgb(style.palette.destructive))
                .child(error.to_owned()),
        );
    }
    div()
        .my_3()
        .p_3()
        .rounded(px(radius::LG))
        .border_1()
        .border_color(rgb(style.palette.border))
        .bg(rgb(style.palette.surface1))
        .font_family(style.ui_family.clone())
        .child(body)
        .into_any_element()
}

fn unreadable_question(
    request: &Value,
    request_id: &str,
    style: &MessageStyle,
) -> gpui::AnyElement {
    let title = request["title"]
        .as_str()
        .or_else(|| request["name"].as_str())
        .unwrap_or("Question");
    div()
        .my_3()
        .p_3()
        .rounded(px(radius::LG))
        .border_1()
        .border_color(rgb(style.palette.border))
        .bg(rgb(style.palette.surface1))
        .font_family(style.ui_family.clone())
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .text_size(px(font_size::BASE))
                .text_color(rgb(style.palette.foreground))
                .child(title.to_owned()),
        )
        .child(
            div()
                .text_size(px(font_size::SM))
                .text_color(rgb(style.palette.foreground_muted))
                .child(format!(
                    "This question request ({request_id}) has no readable fields."
                )),
        )
        .into_any_element()
}

fn actions_for(request: &Value, plan: bool) -> Vec<Value> {
    if let Some(actions) = request["actions"]
        .as_array()
        .filter(|actions| !actions.is_empty())
    {
        return actions.clone();
    }
    vec![
        json!({
            "id": "reject",
            "label": "Deny",
            "behavior": "deny",
            "variant": "danger",
            "intent": "dismiss",
        }),
        json!({
            "id": "accept",
            "label": if plan { "Implement" } else { "Accept" },
            "behavior": "allow",
            "variant": "primary",
        }),
    ]
}

fn action_button(
    action: Value,
    request_id: &str,
    responding: bool,
    entity: gpui::Entity<PermissionBoard>,
    style: &MessageStyle,
) -> gpui::AnyElement {
    let request_id = request_id.to_owned();
    let behavior = action["behavior"].as_str().unwrap_or("deny").to_owned();
    let is_primary = action["variant"].as_str() == Some("primary");
    let label = action["label"].as_str().unwrap_or("").to_owned();
    let disabled = responding;
    let allow = behavior == "allow";
    div()
        .id(SharedString::from(format!(
            "permission-{}-{}",
            request_id,
            action["id"].as_str().unwrap_or("")
        )))
        .py_2()
        .px_3()
        .rounded(px(radius::MD))
        .border_1()
        .border_color(rgb(style.palette.border_accent))
        .bg(rgb(style.palette.surface1))
        .when(!disabled, |button| {
            button
                .cursor_pointer()
                .hover(|v| v.bg(rgb(style.palette.surface2)))
                .active(|v| v.opacity(0.9))
        })
        .when(disabled, |button| button.opacity(0.6))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_center()
                        .size(px(16.))
                        .child(
                            crate::chrome::icon(if allow { "check" } else { "x" }, 14.)
                                .text_color(rgb(style.palette.foreground_muted)),
                        ),
                )
                .child(
                    div()
                        .text_size(px(font_size::BASE))
                        .text_color(rgb(if is_primary {
                            style.palette.foreground
                        } else {
                            style.palette.foreground_muted
                        }))
                        .child(label),
                ),
        )
        .when(!disabled, |button| {
            button.on_mouse_down(MouseButton::Left, move |_, _, cx| {
                cx.stop_propagation();
                entity.update(cx, |board, cx| {
                    board.respond(&request_id, &action, cx);
                });
            })
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolutions_only_belong_to_the_board_agent() {
        assert!(resolution_matches_agent(
            "agent-a",
            &json!({"agentId":"agent-a","requestId":"request-1"})
        ));
        assert!(!resolution_matches_agent(
            "agent-a",
            &json!({"agentId":"agent-b","requestId":"request-1"})
        ));
        // Older payloads omit agentId; the resolution still belongs to the only pending board.
        assert!(resolution_matches_agent(
            "agent-a",
            &json!({"requestId":"request-1"})
        ));
        assert!(!resolution_matches_agent(
            "agent-a",
            &json!({"agentId":"agent-a"})
        ));
    }

    #[test]
    fn reply_errors_only_consume_the_responding_request() {
        let responding = ("request-1".to_owned(), "accept".to_owned());
        assert!(error_matches_reply(
            Some(&responding),
            &json!({"requestType":"agent_permission_response","requestId":"request-1"})
        ));
        assert!(!error_matches_reply(
            Some(&responding),
            &json!({"requestType":"agent_permission_response","requestId":"request-2"})
        ));
        assert!(!error_matches_reply(
            Some(&responding),
            &json!({"requestType":"agent_permission_request","requestId":"request-1"})
        ));
        assert!(!error_matches_reply(
            None,
            &json!({"requestType":"agent_permission_response","requestId":"request-1"})
        ));
    }
}

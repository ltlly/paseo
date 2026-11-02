//! Question permissions. The daemon asks typed questions through a permission request; the form
//! answers with `behavior: allow` and the selected labels in `updatedInput.answers`, matching the
//! desktop question card and its dismissal rules.
use crate::{
    chrome,
    inline_input::{InlineInput, InlineInputEvent},
    message_style::MessageStyle,
    permission_board::PermissionBoard,
    settings_view::{SharedSettings, active_palette},
    theme::{font_size, radius},
};
use gpui::{
    Context, Entity, MouseButton, Render, SharedString, StyledText, Window, div, prelude::*, px,
    rgb,
};
use serde_json::{Map, Value};
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    pub label: String,
    pub description: Option<String>,
    pub preview: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Question {
    pub header: String,
    pub question: String,
    pub options: Vec<Choice>,
    pub multi_select: bool,
    pub allow_other: bool,
    pub allow_empty: bool,
    pub placeholder: Option<String>,
    pub dismiss_label: Option<String>,
}

pub fn parse(input: &Value) -> Option<Vec<Question>> {
    let raw = input.get("questions")?.as_array()?;
    let mut questions = Vec::new();
    for item in raw {
        let options = item
            .get("options")?
            .as_array()?
            .iter()
            .map(|option| {
                Some(Choice {
                    label: option.get("label")?.as_str()?.to_owned(),
                    description: option
                        .get("description")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    preview: option
                        .get("preview")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                })
            })
            .collect::<Option<Vec<_>>>()?;
        questions.push(Question {
            header: item.get("header")?.as_str()?.to_owned(),
            question: item.get("question")?.as_str()?.to_owned(),
            multi_select: item.get("multiSelect") == Some(&Value::Bool(true)),
            allow_other: item.get("allowOther") == Some(&Value::Bool(true))
                || item.get("isOther") == Some(&Value::Bool(true)),
            allow_empty: item.get("allowEmpty") == Some(&Value::Bool(true)),
            placeholder: item
                .get("placeholder")
                .and_then(Value::as_str)
                .map(str::to_owned),
            dismiss_label: item
                .get("dismissLabel")
                .and_then(Value::as_str)
                .map(str::to_owned),
            options,
        });
    }
    (!questions.is_empty()).then_some(questions)
}

pub fn shows_text_input(question: &Question) -> bool {
    question.options.is_empty() || question.allow_other
}

pub fn is_answered(question: &Question, selections: &BTreeSet<usize>, other: &str) -> bool {
    if !selections.is_empty() {
        return true;
    }
    if !shows_text_input(question) {
        return false;
    }
    if !other.trim().is_empty() {
        return true;
    }
    question.allow_empty
}

pub fn answers(
    questions: &[Question],
    selections: &[BTreeSet<usize>],
    others: &[String],
) -> Map<String, Value> {
    let mut answers = Map::new();
    for (index, question) in questions.iter().enumerate() {
        let selected = selections.get(index).cloned().unwrap_or_default();
        let other = others.get(index).map(String::as_str).unwrap_or("").trim();
        let labels = selected
            .iter()
            .filter_map(|option| question.options.get(*option))
            .map(|option| option.label.clone())
            .collect::<Vec<_>>();
        if shows_text_input(question) {
            if !other.is_empty() {
                let answer = if question.multi_select {
                    labels
                        .iter()
                        .map(String::as_str)
                        .chain(std::iter::once(other))
                        .collect::<Vec<_>>()
                        .join(", ")
                } else {
                    other.to_owned()
                };
                answers.insert(question.header.clone(), Value::String(answer));
                continue;
            }
            if question.allow_empty && question.options.is_empty() {
                answers.insert(question.header.clone(), Value::String(String::new()));
                continue;
            }
        }
        if !labels.is_empty() {
            answers.insert(question.header.clone(), Value::String(labels.join(", ")));
        }
    }
    answers
}

pub fn submits_empty_on_dismiss(questions: &[Question]) -> bool {
    !questions.is_empty()
        && questions
            .iter()
            .all(|question| question.allow_empty && question.options.is_empty())
}

pub struct QuestionForm {
    request_id: String,
    request_input: Value,
    questions: Vec<Question>,
    selections: Vec<BTreeSet<usize>>,
    others: Vec<String>,
    active: usize,
    input: Entity<InlineInput>,
    board: Entity<PermissionBoard>,
    responding: Option<String>,
    error: Option<String>,
    shared: SharedSettings,
}

impl QuestionForm {
    pub fn new(
        request_id: String,
        request_input: Value,
        board: Entity<PermissionBoard>,
        shared: SharedSettings,
        cx: &mut Context<Self>,
    ) -> Self {
        let questions = parse(&request_input).unwrap_or_default();
        let count = questions.len();
        let placeholder = questions
            .first()
            .and_then(|question| question.placeholder.clone())
            .unwrap_or_else(|| "Type your answer".into());
        let input = cx.new(|cx| InlineInput::new(placeholder, shared.clone(), cx));
        let form = Self {
            request_id,
            request_input,
            selections: vec![BTreeSet::new(); count],
            others: vec![String::new(); count],
            questions,
            active: 0,
            input,
            board,
            responding: None,
            error: None,
            shared,
        };
        cx.subscribe(
            &form.input,
            |this, _, event: &InlineInputEvent, cx| match event {
                InlineInputEvent::Changed => {
                    let text = this.input.read(cx).text().to_owned();
                    if let Some(other) = this.others.get_mut(this.active) {
                        *other = text.clone();
                    }
                    if !text.is_empty()
                        && this
                            .questions
                            .get(this.active)
                            .is_some_and(|question| !question.multi_select)
                        && let Some(selected) = this.selections.get_mut(this.active)
                    {
                        selected.clear();
                    }
                    cx.notify();
                }
                InlineInputEvent::Submit => this.primary(cx),
            },
        )
        .detach();
        form
    }
    pub fn set_state(
        &mut self,
        responding: Option<String>,
        error: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.responding == responding && self.error == error {
            return;
        }
        self.responding = responding;
        self.error = error;
        cx.notify();
    }
    fn answered(&self, index: usize) -> bool {
        self.questions.get(index).is_some_and(|question| {
            is_answered(
                question,
                self.selections.get(index).unwrap_or(&BTreeSet::new()),
                self.others.get(index).map(String::as_str).unwrap_or(""),
            )
        })
    }
    fn all_answered(&self) -> bool {
        (0..self.questions.len()).all(|index| self.answered(index))
    }
    fn select(&mut self, option: usize, cx: &mut Context<Self>) {
        if self.responding.is_some() {
            return;
        }
        let Some(question) = self.questions.get(self.active).cloned() else {
            return;
        };
        let mut selected = self.selections[self.active].clone();
        if question.multi_select {
            if !selected.remove(&option) {
                selected.insert(option);
            }
        } else if selected.contains(&option) {
            selected.clear();
        } else {
            selected.clear();
            selected.insert(option);
        }
        self.selections[self.active] = selected;
        if !question.multi_select {
            if let Some(other) = self.others.get_mut(self.active) {
                other.clear();
            }
            self.input.update(cx, |input, cx| input.set_text("", cx));
            if !self.selections[self.active].is_empty() && self.active + 1 < self.questions.len() {
                self.switch_to(self.active + 1, cx);
            }
        }
        cx.notify();
    }
    fn switch_to(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.questions.len() {
            return;
        }
        self.active = index;
        let text = self.others.get(index).cloned().unwrap_or_default();
        let placeholder = self
            .questions
            .get(index)
            .and_then(|question| question.placeholder.clone())
            .unwrap_or_else(|| "Type your answer".into());
        self.input.update(cx, |input, cx| {
            input.set_placeholder(placeholder, cx);
            input.set_text(&text, cx);
        });
        cx.notify();
    }
    fn primary(&mut self, cx: &mut Context<Self>) {
        if self.responding.is_some() {
            return;
        }
        if self.active + 1 < self.questions.len() {
            if self.answered(self.active) {
                self.switch_to(self.active + 1, cx);
            }
            return;
        }
        if !self.all_answered() {
            return;
        }
        let answers = answers(&self.questions, &self.selections, &self.others);
        let mut input = self.request_input.clone();
        if let Some(object) = input.as_object_mut() {
            object.insert("answers".into(), Value::Object(answers));
        }
        self.responding = Some("submit".into());
        self.error = None;
        cx.notify();
        let result = self.board.update(cx, |board, cx| {
            board.answer_question(&self.request_id, "submit", Some(input), cx)
        });
        if let Err(error) = result {
            self.responding = None;
            self.error = Some(error.to_string());
            cx.notify();
        }
    }
    fn dismiss(&mut self, cx: &mut Context<Self>) {
        if self.responding.is_some() {
            return;
        }
        self.responding = Some("dismiss".into());
        self.error = None;
        cx.notify();
        if submits_empty_on_dismiss(&self.questions) {
            let answers = answers(&self.questions, &self.selections, &self.others);
            let mut input = self.request_input.clone();
            if let Some(object) = input.as_object_mut() {
                object.insert("answers".into(), Value::Object(answers));
            }
            let result = self.board.update(cx, |board, cx| {
                board.answer_question(&self.request_id, "dismiss", Some(input), cx)
            });
            if let Err(error) = result {
                self.responding = None;
                self.error = Some(error.to_string());
                cx.notify();
            }
        } else {
            let result = self.board.update(cx, |board, cx| {
                board.answer_question(&self.request_id, "dismiss", None, cx)
            });
            if let Err(error) = result {
                self.responding = None;
                self.error = Some(error.to_string());
                cx.notify();
            }
        }
    }
    fn root(&self, style: &MessageStyle, cx: &mut Context<Self>) -> gpui::AnyElement {
        div()
            .id(SharedString::from(format!(
                "question-form-{}",
                self.request_id
            )))
            .my_3()
            .w_full()
            .p_3()
            .rounded(px(radius::LG))
            .border_1()
            .border_color(rgb(style.palette.border))
            .bg(rgb(style.palette.surface1))
            .font_family(style.ui_family.clone())
            .flex()
            .flex_col()
            .gap_3()
            .when(self.questions.len() > 1, |root| {
                root.child(nav(self, style, cx))
            })
            .when_some(
                self.questions.get(self.active).cloned(),
                |root, question| {
                    root.child(
                        div()
                            .px_3()
                            .text_size(px(font_size::BASE))
                            .line_height(px(22.))
                            .text_color(rgb(style.palette.foreground))
                            .child(StyledText::new(SharedString::from(
                                question.question.clone(),
                            ))),
                    )
                    .child(div().flex().flex_col().gap_1().children(
                        question.options.iter().enumerate().map(|(index, option)| {
                            self.option_row(&question, index, option, style, cx)
                        }),
                    ))
                    .when(shows_text_input(&question), |root| {
                        root.child(div().px_3().child(self.input.clone()))
                    })
                },
            )
            .when_some(self.error.clone(), |root, error| {
                root.child(
                    div()
                        .px_3()
                        .text_size(px(font_size::SM))
                        .line_height(px(18.))
                        .text_color(rgb(style.palette.destructive))
                        .child(error),
                )
            })
            .child(self.actions(style, cx))
            .into_any_element()
    }
    fn option_row(
        &self,
        question: &Question,
        index: usize,
        option: &Choice,
        style: &MessageStyle,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let selected = self
            .selections
            .get(self.active)
            .is_some_and(|selection| selection.contains(&index));
        let disabled = self.responding.is_some();
        let multi = question.multi_select;
        let id = SharedString::from(format!("question-option-{}-{index}", self.request_id));
        div()
            .id(id)
            .w_full()
            .min_w_0()
            .flex()
            .items_start()
            .gap_2()
            .px_3()
            .py_2()
            .rounded(px(radius::MD))
            .when(!disabled, |row| {
                row.cursor_pointer()
                    .hover(|hovered| hovered.bg(rgb(style.palette.surface2)))
            })
            .when(selected, |row| row.bg(rgb(style.palette.surface2)))
            .when(disabled, |row| row.opacity(0.7))
            .child(
                div()
                    .mt(px(2.))
                    .size(px(18.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .border_1()
                    .border_color(rgb(if selected {
                        style.palette.accent
                    } else {
                        style.palette.foreground_extra_muted
                    }))
                    .when(selected && multi, |control| {
                        control.bg(rgb(style.palette.accent))
                    })
                    .when(multi, |control| control.rounded(px(radius::BASE)))
                    .when(!multi, |control| control.rounded(px(radius::FULL)))
                    .when(selected && multi, |control| {
                        control.child(
                            chrome::icon("check", 12.)
                                .text_color(rgb(style.palette.accent_foreground)),
                        )
                    })
                    .when(selected && !multi, |control| {
                        control.child(
                            div()
                                .size(px(8.))
                                .rounded_full()
                                .bg(rgb(style.palette.accent)),
                        )
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(px(font_size::BASE))
                            .line_height(px(22.))
                            .text_color(rgb(if selected {
                                style.palette.foreground
                            } else {
                                style.palette.foreground_muted
                            }))
                            .child(StyledText::new(SharedString::from(option.label.clone()))),
                    )
                    .when_some(option.description.clone(), |text, description| {
                        text.child(
                            div()
                                .text_size(px(font_size::BASE))
                                .line_height(px(20.))
                                .text_color(rgb(style.palette.foreground_muted))
                                .child(StyledText::new(SharedString::from(description))),
                        )
                    })
                    .when_some(option.preview.clone(), |text, preview| {
                        text.child(
                            div()
                                .id(SharedString::from(format!(
                                    "option-preview-{}-{index}",
                                    self.request_id
                                )))
                                .max_h(px(240.))
                                .overflow_y_scroll()
                                .rounded(px(radius::MD))
                                .bg(rgb(style.palette.surface0))
                                .p_2()
                                .text_size(px(style.geometry.code_size))
                                .line_height(px(style.geometry.code_line))
                                .font_family(style.mono_family.clone())
                                .text_color(rgb(style.palette.foreground))
                                .child(StyledText::new(SharedString::from(preview))),
                        )
                    }),
            )
            .when(!disabled, |row| {
                row.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.select(index, cx);
                    }),
                )
            })
            .into_any_element()
    }
    fn actions(&self, style: &MessageStyle, cx: &mut Context<Self>) -> gpui::AnyElement {
        let last = self.active + 1 >= self.questions.len();
        let label = if last { "Submit" } else { "Next" };
        let enabled = !(self.responding.is_some()
            || if last {
                !self.all_answered()
            } else {
                !self.answered(self.active)
            });
        let dismiss_label = self
            .questions
            .iter()
            .find_map(|question| question.dismiss_label.clone())
            .unwrap_or_else(|| "Dismiss".into());
        let mut row = div().flex().flex_wrap().items_center().gap_2().px_3();
        row = row.child(
            div()
                .id("question-dismiss")
                .flex()
                .items_center()
                .gap_2()
                .py_2()
                .px_3()
                .rounded(px(radius::MD))
                .border_1()
                .border_color(rgb(style.palette.border_accent))
                .bg(rgb(style.palette.surface1))
                .when(self.responding.is_none(), |button| {
                    button
                        .cursor_pointer()
                        .hover(|hovered| hovered.bg(rgb(style.palette.surface2)))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.dismiss(cx);
                            }),
                        )
                })
                .child(chrome::icon("x", 14.).text_color(rgb(style.palette.foreground_muted)))
                .child(
                    div()
                        .text_size(px(font_size::BASE))
                        .text_color(rgb(style.palette.foreground_muted))
                        .child(dismiss_label),
                ),
        );
        row = row.child(
            div()
                .id("question-primary")
                .flex()
                .items_center()
                .gap_2()
                .py_2()
                .px_3()
                .rounded(px(radius::MD))
                .border_1()
                .border_color(rgb(style.palette.accent))
                .bg(rgb(style.palette.accent))
                .opacity(if enabled { 1. } else { 0.5 })
                .when(enabled, |button| {
                    button.cursor_pointer().on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.primary(cx);
                        }),
                    )
                })
                .when(self.responding.as_deref() == Some("submit"), |button| {
                    button.child(chrome::loading_spinner(
                        14.,
                        style.palette.accent_foreground,
                    ))
                })
                .when(self.responding.as_deref() != Some("submit"), |button| {
                    button
                        .child(
                            chrome::icon("check", 14.)
                                .text_color(rgb(style.palette.accent_foreground)),
                        )
                        .child(
                            div()
                                .text_size(px(font_size::BASE))
                                .text_color(rgb(style.palette.accent_foreground))
                                .child(label),
                        )
                }),
        );
        row.into_any_element()
    }
}

fn nav(
    form: &QuestionForm,
    style: &MessageStyle,
    cx: &mut Context<QuestionForm>,
) -> gpui::AnyElement {
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_1()
        .px_3()
        .children((0..form.questions.len()).map(|index| {
            let active = index == form.active;
            let answered = form.answered(index);
            div()
                .id(SharedString::from(format!(
                    "question-nav-{}-{index}",
                    form.request_id
                )))
                .flex()
                .items_center()
                .gap_1()
                .min_h(px(28.))
                .px_2()
                .py_1()
                .rounded(px(radius::MD))
                .border_1()
                .border_color(rgb(if active {
                    style.palette.foreground_muted
                } else {
                    style.palette.border
                }))
                .bg(rgb(if active {
                    style.palette.surface2
                } else {
                    style.palette.surface1
                }))
                .when(answered, |button| {
                    button.child(chrome::icon("check", 12.).text_color(rgb(if active {
                        style.palette.foreground
                    } else {
                        style.palette.muted_foreground
                    })))
                })
                .child(
                    div()
                        .text_size(px(font_size::BASE))
                        .text_color(rgb(if active {
                            style.palette.foreground
                        } else {
                            style.palette.foreground_muted
                        }))
                        .child(form.questions[index].header.clone()),
                )
                .when(form.responding.is_none(), |button| {
                    button.on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.switch_to(index, cx);
                        }),
                    )
                })
                .into_any_element()
        }))
        .into_any_element()
}

impl Render for QuestionForm {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let style = MessageStyle::resolve(&self.shared.get(), active_palette(&self.shared, window));
        self.root(&style, cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn form() -> Vec<Question> {
        parse(&json!({"questions":[{"header":"Mode","question":"Pick","options":[{"label":"Fast"},{"label":"Safe","description":"slower"}],"multiSelect":false,"allowOther":true,"placeholder":"Other"},{"header":"Notes","question":"Anything?","options":[],"allowEmpty":true,"dismissLabel":"Skip"}]})).unwrap()
    }

    #[test]
    fn answers_follow_single_multi_and_custom_text_rules() {
        let questions = form();
        let selections = vec![BTreeSet::from([1]), BTreeSet::new()];
        let others = vec!["custom".to_string(), String::new()];
        let built = answers(&questions, &selections, &others);
        assert_eq!(built["Mode"], "custom");
        assert_eq!(built["Notes"], "");
        assert!(is_answered(&questions[1], &BTreeSet::new(), ""));

        let selections = vec![BTreeSet::from([0, 1]), BTreeSet::new()];
        let others = vec!["also".to_string(), "note".into()];
        let built = answers(&questions, &selections, &others);
        assert_eq!(built["Mode"], "also");
        assert_eq!(built["Notes"], "note");
        assert!(!submits_empty_on_dismiss(&questions));

        let multi = parse(&json!({"questions":[{"header":"Pick","question":"Q","options":[{"label":"A"},{"label":"B"}],"multiSelect":true,"allowOther":true}]})).unwrap();
        let built = answers(&multi, &[BTreeSet::from([0, 1])], &["C".into()]);
        assert_eq!(built["Pick"], "A, B, C");
    }

    #[test]
    fn empty_allow_empty_questions_submit_on_dismiss_and_unanswered_required_blocks() {
        let questions = form();
        let empty = vec![BTreeSet::new(), BTreeSet::new()];
        assert!(!is_answered(&questions[0], &empty[0], ""));
        assert!(is_answered(&questions[1], &empty[1], ""));
        let only_empty = parse(
            &json!({"questions":[{"header":"X","question":"Q","options":[],"allowEmpty":true}]}),
        )
        .unwrap();
        assert!(submits_empty_on_dismiss(&only_empty));
        assert_eq!(
            answers(&only_empty, &[BTreeSet::new()], &[String::new()])["X"],
            ""
        );
    }

    #[test]
    fn malformed_question_payload_is_rejected_not_panicked() {
        assert!(parse(&json!({"questions":"nope"})).is_none());
        assert!(parse(&json!({"questions":[{"header":"X"}]})).is_none());
        assert!(parse(&json!({})).is_none());
    }
}

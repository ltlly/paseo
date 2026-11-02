use gpui::{
    Anchor, App, Context, Div, FocusHandle, Focusable, FontWeight, MouseButton, Render,
    ScrollHandle, SharedString, Stateful, Window, canvas, div, point, prelude::*, px, rgb,
};
use paseo_desktop::appearance::Palette;
use serde_json::{Value, json};

use crate::{
    chrome::{icon, loading_spinner},
    schedules_view::{SCHEDULE_CREATE_REQUEST, SCHEDULE_UPDATE_REQUEST, ScheduleSummary},
    settings_view::SharedSettings,
    text_field::{FieldEvent, TextField},
    theme::font_size,
};

/// Cadence presets, verbatim from the app's `CADENCE_PRESET_OPTIONS`.
const CADENCE_PRESETS: [(&str, &str, &str); 5] = [
    ("every-minute", "Every minute", "* * * * *"),
    ("every-hour", "Every hour", "0 * * * *"),
    ("daily-9", "Daily 9:00", "0 9 * * *"),
    ("weekdays-9", "Weekdays 9:00", "0 9 * * 1-5"),
    ("mondays-9", "Mondays 9:00", "0 9 * * 1"),
];
const CUSTOM_CADENCE_ID: &str = "custom";
/// The app labels agent-target schedules heartbeats and new-agent schedules
/// schedules; the form offers both target kinds on create.
const NEW_AGENT_TARGET: &str = "new-agent";
const AGENT_TARGET: &str = "agent";

impl gpui::EventEmitter<ScheduleFormEvent> for ScheduleFormPane {}

pub enum ScheduleFormEvent {
    Close,
    /// The create/update was acknowledged by the daemon; the caller should
    /// refresh the list.
    Saved,
}

struct Target {
    kind: &'static str,
    agent_id: Option<String>,
    provider: Option<String>,
}

pub struct ScheduleFormPane {
    connection: Option<std::sync::Arc<paseo_desktop::client::Connection>>,
    shared: SharedSettings,
    editing: Option<ScheduleSummary>,
    providers: Vec<String>,
    agents: Vec<(String, String)>,
    name: gpui::Entity<TextField>,
    prompt: gpui::Entity<TextField>,
    cwd: gpui::Entity<TextField>,
    cron: gpui::Entity<TextField>,
    cadence_id: String,
    target: Target,
    provider_menu: Option<(
        gpui::Entity<crate::choice_menu::ChoiceMenu>,
        gpui::Subscription,
    )>,
    cadence_menu: Option<(
        gpui::Entity<crate::choice_menu::ChoiceMenu>,
        gpui::Subscription,
    )>,
    target_menu: Option<(
        gpui::Entity<crate::choice_menu::ChoiceMenu>,
        gpui::Subscription,
    )>,
    menu_anchor: gpui::Bounds<gpui::Pixels>,
    submit_request: Option<String>,
    submit_error: Option<String>,
    scroll: ScrollHandle,
    focus: FocusHandle,
}

impl ScheduleFormPane {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        connection: Option<std::sync::Arc<paseo_desktop::client::Connection>>,
        shared: SharedSettings,
        editing: Option<ScheduleSummary>,
        providers: Vec<String>,
        agents: Vec<(String, String)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut field = |value: &str, cx: &mut Context<Self>| {
            let entity = cx.new(|cx| {
                crate::text_field::TextField::new(value, "", false, 360., false, shared.clone(), cx)
            });
            entity.update(cx, |field_view, cx| {
                field_view.arm_commit_on_blur(window, cx)
            });
            entity
        };
        let (name, prompt, cwd, cron, cadence_id, target) = match &editing {
            Some(schedule) => {
                let cron = match schedule.cadence["type"].as_str() {
                    Some("cron") => schedule.cadence["expression"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                    _ => String::new(),
                };
                let preset = CADENCE_PRESETS
                    .iter()
                    .find(|(_, _, expression)| Some(*expression) == Some(cron.as_str()))
                    .map(|(id, _, _)| id.to_string());
                let target = match schedule.target["type"].as_str() {
                    Some("agent") => Target {
                        kind: AGENT_TARGET,
                        agent_id: schedule.target["agentId"].as_str().map(str::to_owned),
                        provider: None,
                    },
                    _ => Target {
                        kind: NEW_AGENT_TARGET,
                        agent_id: None,
                        provider: schedule.target["config"]["provider"]
                            .as_str()
                            .map(str::to_owned),
                    },
                };
                (
                    field(schedule.name.as_deref().unwrap_or_default(), cx),
                    field(&schedule.prompt, cx),
                    field(
                        schedule.target["config"]["cwd"]
                            .as_str()
                            .unwrap_or_default(),
                        cx,
                    ),
                    field(&cron, cx),
                    preset.unwrap_or_else(|| CUSTOM_CADENCE_ID.to_owned()),
                    target,
                )
            }
            None => (
                field("", cx),
                field("", cx),
                field("", cx),
                field("", cx),
                "daily-9".to_owned(),
                Target {
                    kind: NEW_AGENT_TARGET,
                    agent_id: None,
                    provider: providers.first().cloned(),
                },
            ),
        };
        let pane = Self {
            connection,
            shared,
            editing,
            providers,
            agents,
            name,
            prompt,
            cwd,
            cron,
            cadence_id,
            target,
            provider_menu: None,
            cadence_menu: None,
            target_menu: None,
            menu_anchor: gpui::Bounds::default(),
            submit_request: None,
            submit_error: None,
            scroll: ScrollHandle::new(),
            focus: cx.focus_handle(),
        };
        for entity in [
            pane.name.clone(),
            pane.prompt.clone(),
            pane.cwd.clone(),
            pane.cron.clone(),
        ] {
            cx.subscribe_in(&entity, window, |this, _, event: &FieldEvent, _, cx| {
                if matches!(event, FieldEvent::Committed) {
                    this.validate(cx);
                }
            })
            .detach();
        }
        pane
    }

    fn validate(&mut self, cx: &mut Context<Self>) {
        // Re-evaluate submit availability; a hidden implementation error would
        // be a fake toggle, so the button state is always derived live.
        cx.notify();
    }

    fn prompt(&self, cx: &Context<Self>) -> String {
        self.prompt.read(cx).text().trim().to_owned()
    }

    fn cadence(&self, cx: &Context<Self>) -> Value {
        if self.cadence_id == CUSTOM_CADENCE_ID {
            let expression = self.cron.read(cx).text().trim().to_owned();
            return json!({"type": "cron", "expression": expression});
        }
        let (_, _, expression) = CADENCE_PRESETS
            .iter()
            .find(|(id, _, _)| *id == self.cadence_id)
            .expect("preset id comes from the menu");
        json!({"type": "cron", "expression": expression})
    }

    fn cadence_label(&self, cx: &Context<Self>) -> String {
        if self.cadence_id == CUSTOM_CADENCE_ID {
            let expression = self.cron.read(cx).text().trim().to_owned();
            if expression.is_empty() {
                return "Custom".to_owned();
            }
            return expression;
        }
        CADENCE_PRESETS
            .iter()
            .find(|(id, _, _)| *id == self.cadence_id)
            .map(|(_, label, _)| (*label).to_owned())
            .expect("preset id comes from the menu")
    }

    /// A cadence selection only reaches the wire when it differs from the
    /// schedule's stored cadence, so editing an interval schedule without
    /// touching cadence never rewrites it to cron.
    fn cadence_changed(&self, cx: &Context<Self>) -> bool {
        let Some(schedule) = &self.editing else {
            return true;
        };
        self.cadence(cx) != schedule.cadence
    }

    fn valid(&self, cx: &Context<Self>) -> bool {
        if self.prompt(cx).is_empty() {
            return false;
        }
        if self.cadence_id == CUSTOM_CADENCE_ID && self.cron.read(cx).text().trim().is_empty() {
            return false;
        }
        if self.submit_request.is_some() {
            return false;
        }
        if self.editing.is_none() && self.target.kind == NEW_AGENT_TARGET {
            let provider = self.target.provider.as_deref().unwrap_or_default();
            let cwd = self.cwd.read(cx).text().trim().to_owned();
            if provider.is_empty() || cwd.is_empty() {
                return false;
            }
        }
        if self.editing.is_none()
            && self.target.kind == AGENT_TARGET
            && self.target.agent_id.is_none()
        {
            return false;
        }
        true
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        let Some(connection) = &self.connection else {
            self.submit_error = Some("Connect to the host to save schedules.".into());
            cx.notify();
            return;
        };
        if !self.valid(cx) {
            return;
        }
        let name = self.name.read(cx).text().trim().to_owned();
        let prompt = self.prompt(cx);
        let message = match (&self.editing, self.editing.as_ref().map(|s| s.id.clone())) {
            (Some(schedule), Some(id)) => {
                let mut message = json!({
                    "scheduleId": id,
                    "prompt": prompt,
                });
                if name != schedule.name.as_deref().unwrap_or_default() {
                    message["name"] = if name.is_empty() {
                        Value::Null
                    } else {
                        json!(name)
                    };
                }
                if self.cadence_changed(cx) {
                    message["cadence"] = self.cadence(cx);
                }
                message
            }
            _ => {
                let mut message = json!({
                    "prompt": prompt,
                    "cadence": self.cadence(cx),
                });
                if !name.is_empty() {
                    message["name"] = json!(name);
                }
                if self.target.kind == AGENT_TARGET {
                    message["target"] = json!({
                        "type": "agent",
                        "agentId": self.target.agent_id.clone().expect("validated"),
                    });
                } else {
                    message["target"] = json!({
                        "type": "new-agent",
                        "config": {
                            "provider": self.target.provider.clone().expect("validated"),
                            "cwd": self.cwd.read(cx).text().trim(),
                        },
                    });
                }
                message
            }
        };
        let request_kind = if self.editing.is_some() {
            SCHEDULE_UPDATE_REQUEST
        } else {
            SCHEDULE_CREATE_REQUEST
        };
        match connection.request(request_kind, message) {
            Ok(request_id) => {
                self.submit_request = Some(request_id);
                self.submit_error = None;
            }
            Err(error) => self.submit_error = Some(error.to_string()),
        }
        cx.notify();
    }

    /// Consume a session event owned by this pane; `true` means handled.
    pub fn receive(&mut self, kind: &str, payload: &Value, cx: &mut Context<Self>) -> bool {
        let Some(request_id) = payload["requestId"].as_str() else {
            return false;
        };
        if Some(request_id) != self.submit_request.as_deref() {
            return false;
        }
        self.submit_request = None;
        let succeeded = matches!(
            kind,
            "schedule/create/response" | "schedule/update/response"
        ) && payload["schedule"].is_object()
            && payload["error"].as_str().is_none_or(str::is_empty);
        if succeeded {
            cx.emit(ScheduleFormEvent::Saved);
        } else {
            self.submit_error = Some(if let Some(error) = payload["error"].as_str() {
                if error.is_empty() {
                    "The host rejected this schedule.".to_owned()
                } else {
                    error.to_owned()
                }
            } else {
                format!("The host rejected this schedule ({}).", kind)
            });
        }
        cx.notify();
        true
    }

    pub fn disconnected(&mut self, cx: &mut Context<Self>) {
        if self.submit_request.take().is_some() {
            self.submit_error = Some("The host disconnected before this finished.".into());
        }
        cx.notify();
    }

    fn open_cadence_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_menu(
            "cadence",
            CADENCE_PRESETS
                .iter()
                .map(|(id, label, _)| (id.to_string(), label.to_string()))
                .chain(std::iter::once((
                    CUSTOM_CADENCE_ID.to_owned(),
                    "Custom".to_owned(),
                )))
                .collect(),
            Some(self.cadence_id.clone()),
            |this, value| {
                this.cadence_id = value;
            },
            window,
            cx,
        );
    }

    fn open_provider_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_menu(
            "provider",
            self.providers
                .iter()
                .map(|provider| (provider.clone(), provider.clone()))
                .collect(),
            self.target.provider.clone(),
            |this, value| {
                this.target.provider = Some(value);
            },
            window,
            cx,
        );
    }

    fn open_target_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut rows = vec![(NEW_AGENT_TARGET.to_owned(), "New agent".to_owned())];
        rows.extend(
            self.agents
                .iter()
                .map(|(id, title)| (id.clone(), title.clone())),
        );
        self.open_menu(
            "target",
            rows,
            self.target.kind_identifier(),
            |this, value| {
                if value == NEW_AGENT_TARGET {
                    this.target = Target {
                        kind: NEW_AGENT_TARGET,
                        agent_id: None,
                        provider: this
                            .target
                            .provider
                            .clone()
                            .or_else(|| this.providers.first().cloned()),
                    };
                } else {
                    this.target = Target {
                        kind: AGENT_TARGET,
                        agent_id: Some(value),
                        provider: None,
                    };
                }
            },
            window,
            cx,
        );
    }

    fn open_menu(
        &mut self,
        title: &str,
        rows: Vec<(String, String)>,
        selected: Option<String>,
        apply: fn(&mut Self, String),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = title.to_owned();
        match title.as_str() {
            "cadence" => self.cadence_menu = None,
            "provider" => self.provider_menu = None,
            _ => self.target_menu = None,
        }
        let rows = rows
            .into_iter()
            .map(|(id, label)| paseo_desktop::agent_controls::OptionRow {
                id,
                label,
                description: None,
                icon: None,
                is_default: false,
            })
            .collect();
        let menu = cx.new(|cx| {
            crate::choice_menu::ChoiceMenu::new(
                rows,
                selected,
                &title,
                false,
                self.shared.clone(),
                cx,
            )
        });
        let title_key = title.clone();
        let subscription = cx.subscribe_in(
            &menu,
            window,
            move |this, _, event: &crate::choice_menu::Pick, window, cx| {
                match title_key.as_str() {
                    "cadence" => this.cadence_menu = None,
                    "provider" => this.provider_menu = None,
                    _ => this.target_menu = None,
                }
                this.focus.focus(window, cx);
                if let Some(value) = &event.0 {
                    apply(this, value.clone());
                }
                cx.notify();
            },
        );
        menu.read(cx).focus_handle(cx).focus(window, cx);
        match title.as_str() {
            "cadence" => self.cadence_menu = Some((menu, subscription)),
            "provider" => self.provider_menu = Some((menu, subscription)),
            _ => self.target_menu = Some((menu, subscription)),
        }
        cx.notify();
    }

    fn open_menu_trigger(
        &self,
        key: &'static str,
        label: String,
        palette: &Palette,
        base: f32,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let entity = cx.entity();
        div()
            .id(SharedString::from(key.to_owned()))
            .h(px(32.))
            .px(px(12.))
            .flex()
            .items_center()
            .gap_2()
            .rounded(px(12.))
            .border_1()
            .border_color(rgb(palette.surface3))
            .bg(rgb(palette.surface3))
            .text_size(px(base))
            .cursor_pointer()
            .hover(|trigger| trigger.opacity(0.85))
            .child(label)
            .child(icon("chevron-down", 14.))
            .child(
                canvas(
                    move |bounds, _, cx| {
                        entity.update(cx, |this, _| this.menu_anchor = bounds);
                    },
                    |_, (), _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    match key {
                        "cadence-trigger" => this.open_cadence_menu(window, cx),
                        "provider-trigger" => this.open_provider_menu(window, cx),
                        _ => this.open_target_menu(window, cx),
                    }
                }),
            )
    }
}

impl Target {
    fn kind_identifier(&self) -> Option<String> {
        match self.kind {
            AGENT_TARGET => self.agent_id.clone(),
            _ => Some(NEW_AGENT_TARGET.to_owned()),
        }
    }
}

impl Focusable for ScheduleFormPane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ScheduleFormPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = crate::settings_view::active_palette(&self.shared, window);
        let settings = self.shared.get();
        let base =
            paseo_desktop::appearance::scale_ui_font(font_size::BASE, settings.ui_base_font_size);
        let sm =
            paseo_desktop::appearance::scale_ui_font(font_size::SM, settings.ui_base_font_size);
        let lg =
            paseo_desktop::appearance::scale_ui_font(font_size::LG, settings.ui_base_font_size);
        let editing_title = self
            .editing
            .as_ref()
            .map(|schedule| {
                if schedule.target["type"] == "agent" {
                    "Edit heartbeat"
                } else {
                    "Edit schedule"
                }
            })
            .unwrap_or("New schedule");

        let header = div()
            .h(px(44.))
            .flex_shrink_0()
            .px(px(16.))
            .flex()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(
                div()
                    .font_weight(FontWeight::LIGHT)
                    .text_size(px(lg))
                    .child(editing_title),
            )
            .child(
                div()
                    .id("close-schedule-form")
                    .size(px(28.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .cursor_pointer()
                    .hover(|button| button.bg(rgb(palette.surface2)))
                    .child(icon("x", 16.))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|_, _, _, cx| {
                            cx.stop_propagation();
                            cx.emit(ScheduleFormEvent::Close);
                        }),
                    ),
            );

        let row = |title: &str, control: gpui::AnyElement| {
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap(px(12.))
                .px(px(16.))
                .py(px(16.))
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .text_size(px(base))
                        .child(title.to_owned()),
                )
                .child(control)
        };

        let mut card = div()
            .rounded(px(8.))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface1))
            .overflow_hidden();
        let mut rows: Vec<Div> = vec![row("Name", self.name.clone().into_any_element())];
        rows.push(row("Prompt", self.prompt.clone().into_any_element()));
        rows.push(row(
            "Cadence",
            self.open_menu_trigger(
                "cadence-trigger",
                self.cadence_label(cx),
                &palette,
                base,
                cx,
            )
            .into_any_element(),
        ));
        if self.cadence_id == CUSTOM_CADENCE_ID {
            rows.push(row("Cron expression", self.cron.clone().into_any_element()));
        }
        if self.editing.is_none() {
            rows.push(row(
                "Runs",
                self.open_menu_trigger(
                    "target-trigger",
                    if self.target.kind == AGENT_TARGET {
                        self.target
                            .agent_id
                            .as_ref()
                            .and_then(|id| {
                                self.agents
                                    .iter()
                                    .find(|(agent_id, _)| agent_id == id)
                                    .map(|(_, title)| title.clone())
                            })
                            .unwrap_or_else(|| "Heartbeat".to_owned())
                    } else {
                        "New agent".to_owned()
                    },
                    &palette,
                    base,
                    cx,
                )
                .into_any_element(),
            ));
            if self.target.kind == NEW_AGENT_TARGET {
                rows.push(row(
                    "Provider",
                    self.open_menu_trigger(
                        "provider-trigger",
                        self.target.provider.clone().unwrap_or_default(),
                        &palette,
                        base,
                        cx,
                    )
                    .into_any_element(),
                ));
                rows.push(row("Directory", self.cwd.clone().into_any_element()));
            }
        }
        for (index, element) in rows.into_iter().enumerate() {
            let wrapped = if index == 0 {
                element
            } else {
                div()
                    .border_t_1()
                    .border_color(rgb(palette.border))
                    .child(element)
            };
            card = card.child(wrapped);
        }

        let mut column = div()
            .w(px(720.))
            .max_w_full()
            .flex()
            .flex_col()
            .px(px(24.))
            .py(px(24.))
            .gap(px(24.))
            .child(card);

        if let Some(error) = self.submit_error.clone() {
            column = column.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .p(px(16.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.surface1))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .text_color(rgb(palette.status_danger))
                            .text_size(px(sm))
                            .child(error),
                    )
                    .child(
                        div()
                            .id("retry-schedule-submit")
                            .text_size(px(sm))
                            .text_color(rgb(palette.foreground_muted))
                            .cursor_pointer()
                            .hover(|button| button.text_color(rgb(palette.foreground)))
                            .child("Retry")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.submit(cx);
                                }),
                            ),
                    ),
            );
        }

        let pending = self.submit_request.is_some();
        let can_submit = self.valid(cx);
        column = column.child(
            div()
                .flex()
                .justify_end()
                .gap(px(12.))
                .child(
                    div()
                        .id("cancel-schedule-form")
                        .h(px(32.))
                        .px(px(12.))
                        .flex()
                        .items_center()
                        .rounded(px(12.))
                        .border_1()
                        .border_color(rgb(palette.border_accent))
                        .text_size(px(base))
                        .cursor_pointer()
                        .hover(|button| button.bg(rgb(palette.surface2)))
                        .child("Cancel")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|_, _, _, cx| {
                                cx.stop_propagation();
                                cx.emit(ScheduleFormEvent::Close);
                            }),
                        ),
                )
                .child(
                    div()
                        .id("submit-schedule-form")
                        .h(px(32.))
                        .px(px(12.))
                        .flex()
                        .items_center()
                        .gap_2()
                        .rounded(px(12.))
                        .text_size(px(base))
                        .when(!pending && can_submit, |button| {
                            button
                                .cursor_pointer()
                                .bg(rgb(palette.accent))
                                .text_color(rgb(palette.success_foreground))
                                .hover(|button| button.opacity(0.9))
                        })
                        .when(!pending && !can_submit, |button| {
                            button
                                .bg(rgb(palette.surface2))
                                .text_color(rgb(palette.foreground_extra_muted))
                        })
                        .when(pending, |button| {
                            button
                                .bg(rgb(palette.surface2))
                                .text_color(rgb(palette.foreground_extra_muted))
                                .child(loading_spinner(16., palette.foreground_extra_muted))
                        })
                        .child(if self.editing.is_some() {
                            "Save changes"
                        } else {
                            "Create schedule"
                        })
                        .when(!pending && can_submit, |button| {
                            button.on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.submit(cx);
                                }),
                            )
                        }),
                ),
        );

        let body = div()
            .id("schedule-form-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .flex()
            .justify_center()
            .child(column);

        let mut root = div()
            .track_focus(&self.focus)
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .text_color(rgb(palette.foreground))
            .bg(rgb(palette.surface0))
            .child(header)
            .child(body);

        let menu = self
            .cadence_menu
            .as_ref()
            .or(self.provider_menu.as_ref())
            .or(self.target_menu.as_ref())
            .map(|(menu, _)| menu.clone());
        if let Some(menu) = menu {
            let bounds = self.menu_anchor;
            root = root.child(gpui::deferred(
                gpui::anchored()
                    .position(point(bounds.left(), bounds.bottom() + px(4.)))
                    .anchor(Anchor::TopLeft)
                    .snap_to_window_with_margin(px(8.))
                    .child(div().w(px(220.)).child(menu)),
            ));
        }
        root
    }
}

use chrono::{DateTime, Datelike, Local, Utc};
use gpui::{
    App, Context, Div, FocusHandle, Focusable, FontWeight, MouseButton, Render, ScrollHandle,
    SharedString, Stateful, Window, div, prelude::*, px, rgb,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::pane_actions;

use paseo_desktop::appearance::Palette;

use crate::{
    chrome::{Tooltip, icon, loading_spinner},
    theme::font_size,
};

/// One daemon schedule. Heartbeats are schedules whose target is an existing
/// agent, so this pane is the heartbeat surface too: the app derives both
/// products from the same `schedule/list` payload.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleSummary {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub cadence: Value,
    #[serde(default)]
    pub target: Value,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub next_run_at: Option<String>,
    #[serde(default)]
    pub last_run_at: Option<String>,
    #[serde(default)]
    pub expires_at: Option<String>,
}

/// Session message names from `packages/protocol/src/schedule/rpc-schemas.ts`.
pub(crate) const SCHEDULE_CREATE_REQUEST: &str = "schedule/create";
pub(crate) const SCHEDULE_LIST_REQUEST: &str = "schedule/list";
pub(crate) const SCHEDULE_PAUSE_REQUEST: &str = "schedule/pause";
pub(crate) const SCHEDULE_RESUME_REQUEST: &str = "schedule/resume";
pub(crate) const SCHEDULE_RUN_ONCE_REQUEST: &str = "schedule/run-once";
pub(crate) const SCHEDULE_DELETE_REQUEST: &str = "schedule/delete";
pub(crate) const SCHEDULE_UPDATE_REQUEST: &str = "schedule/update";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActionKind {
    Pause,
    Resume,
    RunOnce,
    Delete,
}

impl ActionKind {
    fn pending_label(self) -> &'static str {
        match self {
            Self::Pause => "Pausing...",
            Self::Resume => "Resuming...",
            Self::RunOnce => "Starting...",
            Self::Delete => "Deleting...",
        }
    }

    fn request_kind(self) -> &'static str {
        match self {
            Self::Pause => SCHEDULE_PAUSE_REQUEST,
            Self::Resume => SCHEDULE_RESUME_REQUEST,
            Self::RunOnce => SCHEDULE_RUN_ONCE_REQUEST,
            Self::Delete => SCHEDULE_DELETE_REQUEST,
        }
    }
}

pub enum SchedulesPaneEvent {
    Close,
    NewSchedule,
}

/// Open the form to edit one schedule row.
pub struct SchedulesEditRequest(pub ScheduleSummary);

pub struct SchedulesPane {
    connection: Option<std::sync::Arc<paseo_desktop::client::Connection>>,
    shared: crate::settings_view::SharedSettings,
    schedules: Vec<ScheduleSummary>,
    load_request: Option<String>,
    load_error: Option<String>,
    loaded: bool,
    actions: pane_actions::PendingActions<ActionKind>,
    confirm_delete: Option<String>,
    scroll: ScrollHandle,
    focus: FocusHandle,
}

const MS_PER_MINUTE: i64 = 60_000;
const MS_PER_HOUR: i64 = 60 * MS_PER_MINUTE;
const MS_PER_DAY: i64 = 24 * MS_PER_HOUR;

impl SchedulesPane {
    pub fn new(
        connection: Option<std::sync::Arc<paseo_desktop::client::Connection>>,
        shared: crate::settings_view::SharedSettings,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut pane = Self {
            connection,
            shared,
            schedules: Vec::new(),
            load_request: None,
            load_error: None,
            loaded: false,
            actions: pane_actions::PendingActions::default(),
            confirm_delete: None,
            scroll: ScrollHandle::new(),
            focus: cx.focus_handle(),
        };
        pane.reload(cx);
        pane
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let Some(connection) = &self.connection else {
            self.load_error = Some("Connect to the host to load schedules.".into());
            self.loaded = true;
            cx.notify();
            return;
        };
        match connection.request(SCHEDULE_LIST_REQUEST, json!({})) {
            Ok(request_id) => {
                self.load_request = Some(request_id);
                self.load_error = None;
            }
            Err(error) => self.load_error = Some(error.to_string()),
        }
        cx.notify();
    }

    fn act(&mut self, id: &str, kind: ActionKind, cx: &mut Context<Self>) {
        let Some(connection) = &self.connection else {
            return;
        };
        match connection.request(kind.request_kind(), json!({"scheduleId": id})) {
            Ok(request_id) => self.actions.start(id.to_owned(), kind, request_id),
            Err(error) => self.actions.fail(id, error.to_string()),
        }
        cx.notify();
    }

    /// The request this pane can still receive an `rpc_error` for, if the
    /// payload names one of this pane's in-flight requests.
    fn owns_request(&self, payload: &Value) -> bool {
        let request_id = payload["requestId"].as_str();
        let request_type = payload["requestType"].as_str().unwrap_or_default();
        (request_type.starts_with("schedule/")
            && self
                .actions
                .find_by_request(request_id.unwrap_or_default())
                .is_some())
            || (request_type == SCHEDULE_LIST_REQUEST && self.load_request.as_deref() == request_id)
    }

    /// Consume a session event owned by this pane; `true` means handled.
    pub fn receive(&mut self, kind: &str, payload: &Value, cx: &mut Context<Self>) -> bool {
        let request_id = payload["requestId"].as_str();
        if kind == "schedule/list/response" && request_id == self.load_request.as_deref() {
            self.load_request = None;
            self.loaded = true;
            if let Some(error) = payload["error"].as_str().filter(|error| !error.is_empty()) {
                self.load_error = Some(error.to_owned());
            } else {
                match serde_json::from_value::<Vec<ScheduleSummary>>(payload["schedules"].clone()) {
                    Ok(schedules) => {
                        self.schedules = schedules;
                        self.load_error = None;
                        self.actions
                            .retain_known(|id| self.schedules.iter().any(|s| s.id == id));
                    }
                    Err(_) => {
                        self.load_error = Some(
                            "The host sent a schedules response this client cannot read.".into(),
                        )
                    }
                }
            }
            cx.notify();
            return true;
        }
        if kind == "rpc_error" && self.owns_request(payload) {
            let error = payload["error"]
                .as_str()
                .unwrap_or("unknown error")
                .to_owned();
            match self
                .actions
                .find_by_request(request_id.unwrap_or_default())
                .map(|(id, _)| id.to_owned())
            {
                Some(id) => self.actions.fail(&id, error),
                None => {
                    self.load_request = None;
                    self.loaded = true;
                    self.load_error = Some(error);
                }
            }
            cx.notify();
            return true;
        }
        for response_kind in [
            "schedule/pause/response",
            "schedule/resume/response",
            "schedule/run-once/response",
            "schedule/delete/response",
        ] {
            if kind != response_kind {
                continue;
            }
            let Some(request_id) = request_id else {
                continue;
            };
            let Some((owned, _)) = self.actions.find_by_request(request_id) else {
                continue;
            };
            let id = owned.to_owned();
            let failed = payload["error"]
                .as_str()
                .is_some_and(|error| !error.is_empty());
            if failed {
                self.actions
                    .fail(&id, payload["error"].as_str().unwrap().to_owned());
            } else {
                self.actions.complete(&id);
                if kind == "schedule/delete/response" {
                    self.schedules.retain(|schedule| schedule.id != id);
                    if self.confirm_delete.as_deref() == Some(id.as_str()) {
                        self.confirm_delete = None;
                    }
                } else if let Ok(schedule) =
                    serde_json::from_value::<ScheduleSummary>(payload["schedule"].clone())
                    && let Some(existing) = self
                        .schedules
                        .iter_mut()
                        .find(|existing| existing.id == schedule.id)
                {
                    *existing = schedule;
                }
            }
            cx.notify();
            return true;
        }
        false
    }

    /// A disconnect is an unknown outcome, never an automatic retry: every
    /// in-flight action fails visibly and waits for a manual retry.
    pub fn disconnected(&mut self, error: &str, cx: &mut Context<Self>) {
        self.load_request = None;
        if !self.loaded {
            self.load_error = Some(error.to_owned());
        }
        self.actions.fail_all_unknown();
        cx.notify();
    }

    /// Port of the app's `scheduleProductName`.
    fn product_name(schedule: &ScheduleSummary) -> &'static str {
        if schedule.target["type"] == "agent" {
            "Heartbeat"
        } else {
            "Schedule"
        }
    }

    /// Port of the app's `resolveScheduleTitle`.
    fn title(schedule: &ScheduleSummary) -> String {
        if let Some(name) = schedule
            .name
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty())
        {
            return name.to_owned();
        }
        if schedule.target["type"] == "agent"
            && let Some(title) = schedule.target["config"]["title"]
                .as_str()
                .map(str::trim)
                .filter(|title| !title.is_empty())
        {
            return title.to_owned();
        }
        schedule
            .prompt
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("Untitled {}", Self::product_name(schedule).to_lowercase()))
    }

    /// Port of the app's `formatTimeAgo`.
    fn time_ago(iso: &str, now: DateTime<Local>) -> Option<String> {
        let parsed = DateTime::parse_from_rfc3339(iso).ok()?;
        let elapsed = (now.timestamp_millis() - parsed.timestamp_millis()).max(0);
        Some(if elapsed < MS_PER_MINUTE {
            "just now".to_owned()
        } else if elapsed < MS_PER_HOUR {
            format!("{}m ago", elapsed / MS_PER_MINUTE)
        } else if elapsed < MS_PER_DAY {
            format!("{}h ago", elapsed / MS_PER_HOUR)
        } else if elapsed < 7 * MS_PER_DAY {
            format!("{}d ago", elapsed / MS_PER_DAY)
        } else {
            let date: DateTime<Local> = parsed.into();
            format!("{} {}", date.format("%b"), date.day())
        })
    }

    /// Port of the app's `formatNextRun`.
    fn next_run(iso: Option<&str>, now: DateTime<Local>) -> String {
        let Some(iso) = iso else {
            return String::new();
        };
        let Ok(parsed) = DateTime::parse_from_rfc3339(iso) else {
            return String::new();
        };
        let diff = parsed.timestamp_millis() - now.timestamp_millis();
        if diff < MS_PER_MINUTE {
            "soon".into()
        } else if diff < MS_PER_HOUR {
            format!("in {}m", diff / MS_PER_MINUTE)
        } else if diff < MS_PER_DAY {
            format!("in {}h", diff / MS_PER_HOUR)
        } else {
            format!("in {}d", diff / MS_PER_DAY)
        }
    }

    /// Port of the app's `formatCadence` including its common-shape cron
    /// humanization; unrecognized crons show the raw expression.
    fn cadence(cadence: &Value) -> String {
        match cadence["type"].as_str() {
            Some("every") => {
                // The app renders non-positive intervals as "Every hour".
                let every_ms = cadence["everyMs"].as_i64().unwrap_or(0);
                if every_ms <= 0 {
                    return "Every hour".to_owned();
                }
                let (value, unit) = if every_ms > 0 && every_ms % MS_PER_DAY == 0 {
                    (every_ms / MS_PER_DAY, "day")
                } else if every_ms > 0 && every_ms % MS_PER_HOUR == 0 {
                    (every_ms / MS_PER_HOUR, "hour")
                } else {
                    // JS Math.round rounds halves away from zero for positives.
                    let minutes = (every_ms as f64 / MS_PER_MINUTE as f64).round() as i64;
                    (minutes.max(1), "minute")
                };
                if value == 1 {
                    format!("Every {unit}")
                } else {
                    format!("Every {value} {unit}s")
                }
            }
            Some("cron") => {
                let expression = cadence["expression"].as_str().unwrap_or_default().trim();
                Self::describe_cron(expression, cadence["timezone"].as_str())
                    .unwrap_or_else(|| expression.to_owned())
            }
            _ => String::new(),
        }
    }

    /// Port of the app's `describeCron` for the fixed-time family.
    fn describe_cron(expression: &str, timezone: Option<&str>) -> Option<String> {
        let fields: Vec<&str> = expression.split_whitespace().collect();
        let [minute, hour, day_of_month, month, day_of_week] = fields[..] else {
            return None;
        };
        if minute == "*" && hour == "*" && month == "*" && day_of_month == "*" && day_of_week == "*"
        {
            return Some("Every minute".into());
        }
        // The app falls back to the raw expression unless the fixed fields are
        // in cron range.
        let minute_num: u32 = minute.parse().unwrap_or(u32::MAX);
        let hour_num: u32 = hour.parse().unwrap_or(u32::MAX);
        if !minute.chars().all(|ch| ch.is_ascii_digit())
            || minute_num > 59
            || month != "*"
            || day_of_month != "*"
        {
            return None;
        }
        if hour != "*" && (hour_num > 23) {
            return None;
        }
        if hour == "*" {
            if day_of_week != "*" {
                return None;
            }
            return Some(if minute == "0" {
                "Every hour".into()
            } else {
                format!("Every hour at :{minute:0>2}")
            });
        }
        if !hour.chars().all(|ch| ch.is_ascii_digit()) {
            return None;
        }
        let time = format!("{hour:0>2}:{minute:0>2}");
        let timezone = timezone.unwrap_or("UTC");
        let day = match day_of_week {
            "*" => "Daily",
            "1-5" => "Weekdays",
            "0,6" | "6,0" => "Weekends",
            "0" => "Sundays",
            "1" => "Mondays",
            "2" => "Tuesdays",
            "3" => "Wednesdays",
            "4" => "Thursdays",
            "5" => "Fridays",
            "6" => "Saturdays",
            _ => return None,
        };
        Some(format!("{day} at {time} {timezone}"))
    }

    /// Port of the app's derived state badge; `targetGone` needs the agent
    /// directory, which this pane does not read.
    fn state(schedule: &ScheduleSummary, now: DateTime<Local>) -> (&'static str, bool) {
        if let Some(expires_at) = &schedule.expires_at
            && let Ok(expires) = DateTime::parse_from_rfc3339(expires_at)
            && expires.with_timezone(&Utc) <= now.with_timezone(&Utc)
        {
            return ("Expired", false);
        }
        match schedule.status.as_str() {
            "completed" => ("Finished", false),
            "paused" => ("Paused", false),
            _ => ("Active", true),
        }
    }

    fn target_label(schedule: &ScheduleSummary) -> String {
        if schedule.target["type"] == "agent" {
            return format!(
                "Agent {}",
                schedule.target["agentId"].as_str().unwrap_or("")
            );
        }
        let cwd = schedule.target["config"]["cwd"]
            .as_str()
            .unwrap_or_default();
        std::path::Path::new(cwd)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| cwd.to_owned())
    }

    fn pending_button(label: &str, palette: &Palette, size: f32) -> Div {
        div()
            .px_2()
            .h(px(24.))
            .flex()
            .items_center()
            .rounded(px(6.))
            .text_size(px(size))
            .text_color(rgb(palette.foreground_extra_muted))
            .child(label.to_owned())
    }

    fn text_button(
        key: String,
        label: String,
        destructive: bool,
        palette: &Palette,
        size: f32,
    ) -> Stateful<Div> {
        div()
            .id(SharedString::from(key))
            .px_2()
            .h(px(24.))
            .flex()
            .items_center()
            .rounded(px(6.))
            .text_size(px(size))
            .cursor_pointer()
            .text_color(rgb(if destructive {
                palette.status_danger
            } else {
                palette.foreground_muted
            }))
            .hover(|button| button.bg(rgb(palette.surface2)))
            .child(label)
    }
}

impl gpui::EventEmitter<SchedulesPaneEvent> for SchedulesPane {}
impl gpui::EventEmitter<SchedulesEditRequest> for SchedulesPane {}

impl Focusable for SchedulesPane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for SchedulesPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = crate::settings_view::active_palette(&self.shared, window);
        let settings = self.shared.get();
        let base =
            paseo_desktop::appearance::scale_ui_font(font_size::BASE, settings.ui_base_font_size);
        let sm =
            paseo_desktop::appearance::scale_ui_font(font_size::SM, settings.ui_base_font_size);
        let lg =
            paseo_desktop::appearance::scale_ui_font(font_size::LG, settings.ui_base_font_size);
        let now = Local::now();

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
                    .child("Schedules"),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .id("new-schedule")
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
                            .child("New schedule")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|_, _, _, cx| {
                                    cx.stop_propagation();
                                    cx.emit(SchedulesPaneEvent::NewSchedule);
                                }),
                            ),
                    )
                    .child(
                        div()
                            .id("close-schedules")
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
                                    cx.emit(SchedulesPaneEvent::Close);
                                }),
                            ),
                    ),
            );

        let body: gpui::AnyElement = if let Some(error) = self.load_error.clone() {
            div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(12.))
                .child(
                    div()
                        .text_color(rgb(palette.foreground_muted))
                        .text_size(px(base))
                        .child("Unable to load schedules"),
                )
                .child(
                    div()
                        .id("retry-schedules")
                        .px_3()
                        .h(px(32.))
                        .flex()
                        .items_center()
                        .rounded(px(12.))
                        .border_1()
                        .border_color(rgb(palette.border_accent))
                        .text_size(px(base))
                        .cursor_pointer()
                        .hover(|button| button.bg(rgb(palette.surface2)))
                        .child("Try again")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.reload(cx);
                            }),
                        ),
                )
                .child(
                    div()
                        .text_color(rgb(palette.status_danger))
                        .text_size(px(sm))
                        .child(error),
                )
                .into_any_element()
        } else if !self.loaded {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .child(loading_spinner(36., palette.foreground_muted))
                .into_any_element()
        } else if self.schedules.is_empty() {
            div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(8.))
                .child(icon("calendar-clock", 20.))
                .child(div().text_size(px(base)).child("No active schedules"))
                .child(
                    div()
                        .text_color(rgb(palette.foreground_muted))
                        .text_size(px(base))
                        .child("Schedules run agents on a cadence."),
                )
                .into_any_element()
        } else {
            let mut schedules = self.schedules.clone();
            schedules.sort_by(|a, b| b.created_at.cmp(&a.created_at));
            let rows = schedules
                .into_iter()
                .map(|schedule| self.schedule_row(&schedule, now, &palette, base, sm, cx))
                .collect::<Vec<_>>();
            div()
                .id("schedules-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .px(px(24.))
                .py(px(16.))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(12.))
                        // Buttons stop their own propagation; a press on the row
                        // body cancels an armed delete.
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                if this.confirm_delete.take().is_some() {
                                    cx.notify();
                                }
                            }),
                        )
                        .children(rows),
                )
                .into_any_element()
        };

        div()
            .track_focus(&self.focus)
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(palette.surface0))
            .child(header)
            .child(body)
    }
}

impl SchedulesPane {
    fn schedule_row(
        &self,
        schedule: &ScheduleSummary,
        now: DateTime<Local>,
        palette: &Palette,
        base: f32,
        sm: f32,
        cx: &mut Context<Self>,
    ) -> Div {
        let id = schedule.id.clone();
        let (state_label, active) = Self::state(schedule, now);
        let action = self.actions.get(&id);
        let can_run = schedule.target["type"] == "new-agent"
            && (schedule.status == "active" || schedule.status == "paused");
        let product = Self::product_name(schedule);
        let created = Self::time_ago(&schedule.created_at, now);

        let mut meta = vec![Self::cadence(&schedule.cadence)];
        if let Some(created) = &created {
            meta.push(format!("Created {created}"));
        }
        if let Some(last_run_at) = &schedule.last_run_at
            && let Some(last_run) = Self::time_ago(last_run_at, now)
        {
            meta.push(format!("Last run {last_run}"));
        } else {
            meta.push("Never run".into());
        }
        if active {
            let next = Self::next_run(schedule.next_run_at.as_deref(), now);
            if !next.is_empty() {
                meta.push(format!("Runs {next}"));
            }
        }

        let mut row = div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .p(px(16.))
            .rounded(px(8.))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface1))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .text_size(px(base))
                            .child(Self::title(schedule)),
                    )
                    .child(
                        div()
                            .px_2()
                            .py(px(1.))
                            .rounded(px(9999.))
                            .text_size(px(sm))
                            .text_color(rgb(if active {
                                palette.status_success
                            } else {
                                palette.foreground_muted
                            }))
                            .bg(rgb(if active {
                                crate::chrome::control_color(
                                    palette.status_success,
                                    palette.surface1,
                                    0.16,
                                )
                            } else {
                                palette.surface2
                            }))
                            .child(state_label),
                    ),
            )
            .child(
                div()
                    .text_color(rgb(palette.foreground_muted))
                    .text_size(px(sm))
                    .child(Self::target_label(schedule)),
            )
            .child(
                div()
                    .text_color(rgb(palette.foreground_extra_muted))
                    .text_size(px(sm))
                    .child(meta.join(" · ")),
            );

        if let Some(action) = action
            && let Some(error) = &action.error
        {
            let retry_kind = action.kind;
            let retry_id = id.clone();
            row = row.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .text_color(rgb(palette.status_danger))
                            .text_size(px(sm))
                            .child(error.clone()),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("retry-{id}")))
                            .text_color(rgb(palette.foreground_muted))
                            .text_size(px(sm))
                            .cursor_pointer()
                            .hover(|button| button.text_color(rgb(palette.foreground)))
                            .child("Retry")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.act(&retry_id, retry_kind, cx);
                                }),
                            ),
                    ),
            );
        }

        let mut controls = div().flex().items_center().gap(px(8.));
        if let Some(action) = action.filter(|action| action.error.is_none()) {
            controls = controls.child(Self::pending_button(
                action.kind.pending_label(),
                palette,
                sm,
            ));
        } else {
            let schedule_for_edit = schedule.clone();
            controls = controls.child(
                Self::text_button(
                    format!("edit-{id}"),
                    format!("Edit {}", product.to_lowercase()),
                    false,
                    palette,
                    sm,
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |_, _, _, cx| {
                        cx.stop_propagation();
                        cx.emit(SchedulesEditRequest(schedule_for_edit.clone()));
                    }),
                ),
            );
            if can_run {
                let run_id = id.clone();
                controls = controls.child(
                    Self::text_button(format!("run-{id}"), "Run now".into(), false, palette, sm)
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.act(&run_id, ActionKind::RunOnce, cx);
                            }),
                        ),
                );
            }
            let cadence_id = id.clone();
            let (cadence_label, cadence_kind) = if schedule.status == "paused" {
                ("Resume schedule", ActionKind::Resume)
            } else {
                ("Pause schedule", ActionKind::Pause)
            };
            controls = controls.child(
                Self::text_button(
                    format!("cadence-{id}"),
                    cadence_label.into(),
                    false,
                    palette,
                    sm,
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.act(&cadence_id, cadence_kind, cx);
                    }),
                ),
            );
            let confirm_delete = self.confirm_delete.as_deref() == Some(id.as_str());
            if confirm_delete {
                let confirm_id = id.clone();
                controls = controls
                    .child(
                        Self::text_button(
                            format!("confirm-{id}"),
                            format!("Delete {}", product.to_lowercase()),
                            true,
                            palette,
                            sm,
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.act(&confirm_id, ActionKind::Delete, cx);
                            }),
                        ),
                    )
                    .child(
                        Self::text_button(
                            format!("cancel-{id}"),
                            "Cancel".into(),
                            false,
                            palette,
                            sm,
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.confirm_delete = None;
                                cx.notify();
                            }),
                        ),
                    );
            } else {
                let arm_id = id.clone();
                controls = controls.child(
                    Self::text_button(
                        format!("delete-{id}"),
                        format!("Delete {}…", product.to_lowercase()),
                        true,
                        palette,
                        sm,
                    )
                    .tooltip(move |_, cx| cx.new(|_| Tooltip(format!("Delete {product}"))).into())
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.confirm_delete = Some(arm_id.clone());
                            cx.notify();
                        }),
                    ),
                );
            }
        }
        row = row.child(controls);
        row
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schedule(payload: Value) -> ScheduleSummary {
        serde_json::from_value(payload).expect("fixture parses")
    }

    #[test]
    fn titles_follow_the_app_derivation() {
        let named = schedule(json!({
            "id": "a1b2c3d4", "name": "Nightly build", "prompt": "run build",
            "target": {"type": "new-agent", "config": {"provider": "claude", "cwd": "/tmp/x"}}
        }));
        assert_eq!(SchedulesPane::title(&named), "Nightly build");

        let heartbeat = schedule(json!({
            "id": "a1b2c3d4", "prompt": "\n  check the flaky test\n",
            "target": {"type": "agent", "agentId": "0197uuid"}
        }));
        assert_eq!(SchedulesPane::title(&heartbeat), "check the flaky test");

        let untitled = schedule(json!({
            "id": "a1b2c3d4", "prompt": "   ",
            "target": {"type": "agent", "agentId": "0197uuid"}
        }));
        assert_eq!(SchedulesPane::title(&untitled), "Untitled heartbeat");

        let untitled_schedule = schedule(json!({
            "id": "a1b2c3d4", "prompt": "",
            "target": {"type": "new-agent", "config": {"provider": "claude", "cwd": "/tmp/x"}}
        }));
        assert_eq!(
            SchedulesPane::title(&untitled_schedule),
            "Untitled schedule"
        );
    }

    #[test]
    fn cadence_matches_the_app_humanization() {
        assert_eq!(
            SchedulesPane::cadence(&json!({"type": "every", "everyMs": 3_600_000})),
            "Every hour"
        );
        assert_eq!(
            SchedulesPane::cadence(&json!({"type": "every", "everyMs": 90_000})),
            "Every 2 minutes"
        );
        assert_eq!(
            SchedulesPane::cadence(&json!({"type": "cron", "expression": "* * * * *"})),
            "Every minute"
        );
        assert_eq!(
            SchedulesPane::cadence(
                &json!({"type": "cron", "expression": "0 9 * * 1-5", "timezone": "Asia/Shanghai"})
            ),
            "Weekdays at 09:00 Asia/Shanghai"
        );
        assert_eq!(
            SchedulesPane::cadence(&json!({"type": "cron", "expression": "30 * * * *"})),
            "Every hour at :30"
        );
        assert_eq!(
            SchedulesPane::cadence(&json!({"type": "cron", "expression": "*/5 * * * *"})),
            "*/5 * * * *"
        );
    }

    #[test]
    fn relative_times_match_the_app_wording() {
        let now = Local::now();
        let created = (now - chrono::Duration::hours(3)).to_rfc3339();
        assert_eq!(
            SchedulesPane::time_ago(&created, now).as_deref(),
            Some("3h ago")
        );
        assert_eq!(SchedulesPane::time_ago("not-a-date", now), None);
        let soon = (now + chrono::Duration::hours(2)).to_rfc3339();
        assert_eq!(SchedulesPane::next_run(Some(&soon), now), "in 2h");
        assert_eq!(SchedulesPane::next_run(Some("garbage"), now), "");
        assert_eq!(SchedulesPane::next_run(None, now), "");
    }

    #[test]
    fn state_derives_from_status_and_expiry() {
        let now = Local::now();
        let active = schedule(json!({"id": "a1b2c3d4", "status": "active"}));
        assert_eq!(SchedulesPane::state(&active, now), ("Active", true));
        let paused = schedule(json!({"id": "a1b2c3d4", "status": "paused"}));
        assert_eq!(SchedulesPane::state(&paused, now), ("Paused", false));
        let finished = schedule(json!({"id": "a1b2c3d4", "status": "completed"}));
        assert_eq!(SchedulesPane::state(&finished, now), ("Finished", false));
        let expired = schedule(json!({
            "id": "a1b2c3d4",
            "status": "active",
            "expiresAt": (now - chrono::Duration::days(1)).to_rfc3339()
        }));
        assert_eq!(SchedulesPane::state(&expired, now), ("Expired", false));
    }

    /// The flat schedule message names must match the protocol rpc schemas;
    /// a real isolated daemon round trip lives in
    /// tests/daemon-rpc-contract.mts.
    #[test]
    fn schedule_request_names_match_the_protocol_schema() {
        let schema = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../packages/protocol/src/schedule/rpc-schemas.ts"
        ))
        .expect("schedule rpc schema source is readable from the workspace");
        for name in [
            SCHEDULE_CREATE_REQUEST,
            SCHEDULE_LIST_REQUEST,
            SCHEDULE_PAUSE_REQUEST,
            SCHEDULE_RESUME_REQUEST,
            SCHEDULE_RUN_ONCE_REQUEST,
            SCHEDULE_DELETE_REQUEST,
        ] {
            assert!(
                schema.contains(&format!("z.literal(\"{name}\")")),
                "schedule rpc schema must declare {name}"
            );
        }
    }

    #[test]
    fn target_label_prefers_the_directory_name() {
        let agent_target = schedule(json!({
            "id": "a1b2c3d4",
            "target": {"type": "agent", "agentId": "0197uuid"}
        }));
        assert_eq!(SchedulesPane::target_label(&agent_target), "Agent 0197uuid");
        let workspace_target = schedule(json!({
            "id": "a1b2c3d4",
            "target": {"type": "new-agent", "config": {"provider": "claude", "cwd": "/home/me/project"}}
        }));
        assert_eq!(SchedulesPane::target_label(&workspace_target), "project");
    }
}

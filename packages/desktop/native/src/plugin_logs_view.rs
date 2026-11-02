use chrono::{DateTime, Local};
use gpui::{
    App, Context, FocusHandle, Focusable, FontWeight, MouseButton, Render, ScrollHandle, Window,
    div, prelude::*, px, rgb,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    chrome::{icon, loading_spinner},
    settings_view::SharedSettings,
    theme::font_size,
};

/// One plugin log line from `plugin.logs.get`.
#[derive(Clone, Debug, Deserialize)]
pub struct PluginLogEntry {
    #[serde(default)]
    pub timestamp: String,
    #[serde(default)]
    pub stream: String,
    #[serde(default)]
    pub message: String,
}

impl gpui::EventEmitter<PluginLogsPaneEvent> for PluginLogsPane {}

pub enum PluginLogsPaneEvent {
    Close,
}

pub struct PluginLogsPane {
    connection: Option<std::sync::Arc<paseo_desktop::client::Connection>>,
    shared: SharedSettings,
    plugin_id: String,
    entries: Vec<PluginLogEntry>,
    load_request: Option<String>,
    load_error: Option<String>,
    loaded: bool,
    refresh_error: Option<String>,
    scroll: ScrollHandle,
    focus: FocusHandle,
}

impl PluginLogsPane {
    pub fn new(
        connection: Option<std::sync::Arc<paseo_desktop::client::Connection>>,
        shared: SharedSettings,
        plugin_id: String,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut pane = Self {
            connection,
            shared,
            plugin_id,
            entries: Vec::new(),
            load_request: None,
            load_error: None,
            loaded: false,
            refresh_error: None,
            scroll: ScrollHandle::new(),
            focus: cx.focus_handle(),
        };
        pane.reload(cx);
        pane
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let Some(connection) = &self.connection else {
            self.load_error = Some("Connect to this host to open plugin settings.".into());
            self.loaded = true;
            cx.notify();
            return;
        };
        match connection.request(
            "plugin.logs.get.request",
            json!({"pluginId": self.plugin_id}),
        ) {
            Ok(request_id) => {
                self.load_request = Some(request_id);
                self.load_error = None;
                self.refresh_error = None;
            }
            Err(error) => self.load_error = Some(error.to_string()),
        }
        cx.notify();
    }

    fn owns_request(&self, payload: &Value) -> bool {
        payload["requestType"].as_str() == Some("plugin.logs.get.request")
            && self.load_request.as_deref() == payload["requestId"].as_str()
    }

    /// Consume a session event owned by this pane; `true` means handled.
    pub fn receive(&mut self, kind: &str, payload: &Value, cx: &mut Context<Self>) -> bool {
        if kind == "plugin.logs.get.response" {
            if payload["requestId"].as_str() != self.load_request.as_deref() {
                return false;
            }
            self.load_request = None;
            self.loaded = true;
            match serde_json::from_value::<Vec<PluginLogEntry>>(payload["entries"].clone()) {
                Ok(entries) => {
                    self.entries = entries;
                    self.load_error = None;
                    self.refresh_error = None;
                }
                Err(_) => {
                    self.load_error =
                        Some("The host sent plugin logs this client cannot read.".into())
                }
            }
            cx.notify();
            return true;
        }
        if kind == "rpc_error" && self.owns_request(payload) {
            self.load_request = None;
            self.loaded = true;
            self.load_error = Some(
                payload["error"]
                    .as_str()
                    .unwrap_or("unknown error")
                    .to_owned(),
            );
            cx.notify();
            return true;
        }
        false
    }

    pub fn disconnected(&mut self, error: &str, cx: &mut Context<Self>) {
        self.load_request = None;
        if !self.loaded {
            self.load_error = Some(error.to_owned());
        } else {
            self.refresh_error = Some("The host disconnected before this finished.".into());
        }
        cx.notify();
    }
}

impl Focusable for PluginLogsPane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for PluginLogsPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = crate::settings_view::active_palette(&self.shared, window);
        let settings = self.shared.get();
        let base =
            paseo_desktop::appearance::scale_ui_font(font_size::BASE, settings.ui_base_font_size);
        let sm =
            paseo_desktop::appearance::scale_ui_font(font_size::SM, settings.ui_base_font_size);
        let lg =
            paseo_desktop::appearance::scale_ui_font(font_size::LG, settings.ui_base_font_size);
        let title = format!("Logs: {}", self.plugin_id);

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
                    .child(title),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .id("refresh-plugin-logs")
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
                            .child(if self.load_request.is_some() {
                                "Refreshing…"
                            } else {
                                "Refresh"
                            })
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
                            .id("close-plugin-logs")
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
                                    cx.emit(PluginLogsPaneEvent::Close);
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
                        .child("Unable to load plugin logs"),
                )
                .child(
                    div()
                        .text_color(rgb(palette.status_danger))
                        .text_size(px(sm))
                        .child(error),
                )
                .child(
                    div()
                        .id("retry-plugin-logs")
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
                .into_any_element()
        } else if !self.loaded {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .child(loading_spinner(36., palette.foreground_muted))
                .into_any_element()
        } else if self.entries.is_empty() {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .text_color(rgb(palette.foreground_muted))
                        .text_size(px(base))
                        .child("No plugin output yet"),
                )
                .into_any_element()
        } else {
            let rows = self
                .entries
                .iter()
                .map(|entry| {
                    let timestamp = DateTime::parse_from_rfc3339(&entry.timestamp)
                        .map(|parsed| parsed.with_timezone(&Local).format("%H:%M:%S").to_string())
                        .unwrap_or_default();
                    div()
                        .flex()
                        .items_start()
                        .gap(px(12.))
                        .py(px(2.))
                        .font_family(crate::fonts::TERMINAL_FAMILY)
                        .text_size(px(sm))
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_color(rgb(palette.foreground_extra_muted))
                                .child(timestamp),
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .w(px(44.))
                                .text_color(rgb(if entry.stream == "stderr" {
                                    palette.status_danger
                                } else {
                                    palette.foreground_extra_muted
                                }))
                                .child(entry.stream.clone()),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .text_color(rgb(palette.foreground))
                                .child(entry.message.clone()),
                        )
                })
                .collect::<Vec<_>>();
            div()
                .id("plugin-logs-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .px(px(24.))
                .py(px(16.))
                .child(div().flex().flex_col().children(rows))
                .into_any_element()
        };

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
        if let Some(error) = self.refresh_error.clone() {
            root = root.child(
                div()
                    .flex_shrink_0()
                    .px(px(16.))
                    .py(px(8.))
                    .border_t_1()
                    .border_color(rgb(palette.border))
                    .text_color(rgb(palette.status_danger))
                    .text_size(px(sm))
                    .child(error),
            );
        }
        root
    }
}

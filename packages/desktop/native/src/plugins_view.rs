use gpui::{
    App, Context, Div, FocusHandle, Focusable, FontWeight, MouseButton, Render, ScrollHandle,
    SharedString, Window, div, prelude::*, px, rgb,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::pane_actions;
use paseo_desktop::appearance::Palette;

use crate::{
    chrome::{icon, loading_spinner},
    theme::font_size,
};

/// One installed plugin from `plugin.list`.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginListItem {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    pub enabled: bool,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub error: Option<String>,
}

/// Dotted session message names from `packages/protocol/src/messages.ts`;
/// the daemon's inbound union rejects anything else at the transport.
pub(crate) const PLUGIN_LIST_REQUEST: &str = "plugin.list.request";
pub(crate) const PLUGIN_ENABLE_REQUEST: &str = "plugin.enable.request";
pub(crate) const PLUGIN_DISABLE_REQUEST: &str = "plugin.disable.request";
pub(crate) const PLUGIN_RELOAD_REQUEST: &str = "plugin.reload.request";
pub(crate) const PLUGIN_REMOVE_REQUEST: &str = "plugin.remove.request";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActionKind {
    Enable,
    Disable,
    Reload,
    Remove,
}

impl ActionKind {
    fn pending_label(self) -> &'static str {
        match self {
            Self::Enable => "Enabling…",
            Self::Disable => "Disabling…",
            Self::Reload => "Reloading…",
            Self::Remove => "Removing…",
        }
    }

    fn request_kind(self) -> &'static str {
        match self {
            Self::Enable => PLUGIN_ENABLE_REQUEST,
            Self::Disable => PLUGIN_DISABLE_REQUEST,
            Self::Reload => PLUGIN_RELOAD_REQUEST,
            Self::Remove => PLUGIN_REMOVE_REQUEST,
        }
    }
}

impl gpui::EventEmitter<PluginsPaneEvent> for PluginsPane {}
impl gpui::EventEmitter<PluginLogsRequest> for PluginsPane {}

pub enum PluginsPaneEvent {
    Close,
}

/// Open the log viewer for one plugin.
pub struct PluginLogsRequest(pub String);

pub struct PluginsPane {
    connection: Option<std::sync::Arc<paseo_desktop::client::Connection>>,
    shared: crate::settings_view::SharedSettings,
    plugins: Vec<PluginListItem>,
    confirm_remove: Option<String>,
    load_request: Option<String>,
    load_error: Option<String>,
    loaded: bool,
    actions: pane_actions::PendingActions<ActionKind>,
    scroll: ScrollHandle,
    focus: FocusHandle,
}

impl PluginsPane {
    pub fn new(
        connection: Option<std::sync::Arc<paseo_desktop::client::Connection>>,
        shared: crate::settings_view::SharedSettings,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut pane = Self {
            connection,
            shared,
            plugins: Vec::new(),
            confirm_remove: None,
            load_request: None,
            load_error: None,
            loaded: false,
            actions: pane_actions::PendingActions::default(),
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
        match connection.request(PLUGIN_LIST_REQUEST, json!({})) {
            Ok(request_id) => {
                self.load_request = Some(request_id);
                self.load_error = None;
            }
            Err(error) => self.load_error = Some(error.to_string()),
        }
        cx.notify();
    }

    fn run_action(&mut self, id: &str, kind: ActionKind, cx: &mut Context<Self>) {
        let Some(connection) = &self.connection else {
            return;
        };
        match connection.request(kind.request_kind(), json!({"pluginId": id})) {
            Ok(request_id) => self.actions.start(id.to_owned(), kind, request_id),
            Err(error) => self.actions.fail(id, error.to_string()),
        }
        cx.notify();
    }

    fn owns_request(&self, payload: &Value) -> bool {
        let request_id = payload["requestId"].as_str();
        let request_type = payload["requestType"].as_str().unwrap_or_default();
        (request_type.starts_with("plugin.")
            && request_type != PLUGIN_LIST_REQUEST
            && self
                .actions
                .find_by_request(request_id.unwrap_or_default())
                .is_some())
            || (request_type == PLUGIN_LIST_REQUEST && self.load_request.as_deref() == request_id)
    }

    /// Consume a session event owned by this pane; `true` means handled.
    pub fn receive(&mut self, kind: &str, payload: &Value, cx: &mut Context<Self>) -> bool {
        let request_id = payload["requestId"].as_str();
        if kind == "plugin.list.response" && request_id == self.load_request.as_deref() {
            self.load_request = None;
            self.loaded = true;
            match serde_json::from_value::<Vec<PluginListItem>>(payload["plugins"].clone()) {
                Ok(plugins) => {
                    self.plugins = plugins;
                    self.load_error = None;
                    self.actions
                        .retain_known(|id| self.plugins.iter().any(|plugin| plugin.id == id));
                }
                Err(_) => {
                    self.load_error =
                        Some("The host sent a plugins response this client cannot read.".into())
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
            "plugin.enable.response",
            "plugin.disable.response",
            "plugin.reload.response",
            "plugin.remove.response",
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
                if kind == "plugin.remove.response" {
                    self.plugins.retain(|plugin| plugin.id != id);
                    if self.confirm_remove.as_deref() == Some(id.as_str()) {
                        self.confirm_remove = None;
                    }
                } else if let Ok(plugin) =
                    serde_json::from_value::<PluginListItem>(payload["plugin"].clone())
                {
                    if let Some(existing) =
                        self.plugins.iter_mut().find(|existing| existing.id == id)
                    {
                        *existing = plugin;
                    }
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
}

impl Focusable for PluginsPane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for PluginsPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = crate::settings_view::active_palette(&self.shared, window);
        let settings = self.shared.get();
        let base =
            paseo_desktop::appearance::scale_ui_font(font_size::BASE, settings.ui_base_font_size);
        let sm =
            paseo_desktop::appearance::scale_ui_font(font_size::SM, settings.ui_base_font_size);
        let lg =
            paseo_desktop::appearance::scale_ui_font(font_size::LG, settings.ui_base_font_size);

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
                    .child("Plugins"),
            )
            .child(
                div()
                    .id("close-plugins")
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
                            cx.emit(PluginsPaneEvent::Close);
                        }),
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
                        .child("Couldn't load plugins"),
                )
                .when(!error.is_empty(), |view| {
                    view.child(
                        div()
                            .text_color(rgb(palette.status_danger))
                            .text_size(px(sm))
                            .child(error),
                    )
                })
                .child(
                    div()
                        .id("retry-plugins")
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
        } else if self.plugins.is_empty() {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .text_color(rgb(palette.foreground_muted))
                        .text_size(px(base))
                        .child("No plugins installed"),
                )
                .into_any_element()
        } else {
            let mut plugins = self.plugins.clone();
            plugins.sort_by(|a, b| a.id.cmp(&b.id));
            let rows = plugins
                .into_iter()
                .map(|plugin| self.plugin_row(&plugin, &palette, base, sm, cx))
                .collect::<Vec<_>>();
            div()
                .id("plugins-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .px(px(24.))
                .py(px(16.))
                .child(div().flex().flex_col().gap(px(12.)).children(rows))
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

impl PluginsPane {
    fn status_label(status: &str) -> &'static str {
        match status {
            "running" => "running",
            "disabled" => "disabled",
            _ => "failed",
        }
    }

    fn plugin_row(
        &self,
        plugin: &PluginListItem,
        palette: &Palette,
        base: f32,
        sm: f32,
        cx: &mut Context<Self>,
    ) -> Div {
        let id = plugin.id.clone();
        let action = self.actions.get(&id);
        let status = Self::status_label(&plugin.status);
        let failed = status == "failed";

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
                            .child(plugin.name.clone().unwrap_or_else(|| plugin.id.clone())),
                    )
                    .child(
                        div()
                            .px_2()
                            .py(px(1.))
                            .rounded(px(9999.))
                            .text_size(px(sm))
                            .text_color(rgb(if failed {
                                palette.status_danger
                            } else if status == "disabled" {
                                palette.foreground_muted
                            } else {
                                palette.status_success
                            }))
                            .child(status),
                    ),
            )
            .when_some(
                plugin.description.clone().filter(|d| !d.is_empty()),
                |row, description| {
                    row.child(
                        div()
                            .text_color(rgb(palette.foreground_muted))
                            .text_size(px(sm))
                            .child(description),
                    )
                },
            )
            .child(
                div()
                    .text_color(rgb(palette.foreground_extra_muted))
                    .text_size(px(sm))
                    .child(plugin.id.clone()),
            );

        if failed && let Some(error) = &plugin.error {
            row = row.child(
                div()
                    .text_color(rgb(palette.status_danger))
                    .text_size(px(sm))
                    .child(error.clone()),
            );
        }

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
                                    this.run_action(&retry_id, retry_kind, cx);
                                }),
                            ),
                    ),
            );
        }

        let mut controls = div().flex().items_center().gap(px(8.));
        if let Some(action) = action.filter(|action| action.error.is_none()) {
            controls = controls.child(
                div()
                    .px_2()
                    .h(px(24.))
                    .flex()
                    .items_center()
                    .rounded(px(6.))
                    .text_size(px(sm))
                    .text_color(rgb(palette.foreground_extra_muted))
                    .child(action.kind.pending_label()),
            );
        } else {
            let next_kind = if plugin.enabled {
                ActionKind::Disable
            } else {
                ActionKind::Enable
            };
            let label = if plugin.enabled { "Disable" } else { "Enable" };
            let action_id = id.clone();
            controls = controls.child(
                div()
                    .id(SharedString::from(format!("toggle-{id}")))
                    .px_2()
                    .h(px(24.))
                    .flex()
                    .items_center()
                    .rounded(px(6.))
                    .text_size(px(sm))
                    .cursor_pointer()
                    .text_color(rgb(palette.foreground_muted))
                    .hover(|button| button.bg(rgb(palette.surface2)))
                    .child(label)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.run_action(&action_id, next_kind, cx);
                        }),
                    ),
            );
            let reload_id = id.clone();
            controls = controls.child(
                div()
                    .id(SharedString::from(format!("reload-{id}")))
                    .px_2()
                    .h(px(24.))
                    .flex()
                    .items_center()
                    .rounded(px(6.))
                    .text_size(px(sm))
                    .cursor_pointer()
                    .text_color(rgb(palette.foreground_muted))
                    .hover(|button| button.bg(rgb(palette.surface2)))
                    .child("Reload")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.run_action(&reload_id, ActionKind::Reload, cx);
                        }),
                    ),
            );
            let logs_id = id.clone();
            controls = controls.child(
                div()
                    .id(SharedString::from(format!("logs-{id}")))
                    .px_2()
                    .h(px(24.))
                    .flex()
                    .items_center()
                    .rounded(px(6.))
                    .text_size(px(sm))
                    .cursor_pointer()
                    .text_color(rgb(palette.foreground_muted))
                    .hover(|button| button.bg(rgb(palette.surface2)))
                    .child("Logs")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |_, _, _, cx| {
                            cx.stop_propagation();
                            cx.emit(PluginLogsRequest(logs_id.clone()));
                        }),
                    ),
            );
            let confirm_remove = self.confirm_remove.as_deref() == Some(id.as_str());
            if confirm_remove {
                let remove_id = id.clone();
                controls = controls
                    .child(
                        div()
                            .id(SharedString::from(format!("confirm-remove-{id}")))
                            .px_2()
                            .h(px(24.))
                            .flex()
                            .items_center()
                            .rounded(px(6.))
                            .text_size(px(sm))
                            .cursor_pointer()
                            .text_color(rgb(palette.status_danger))
                            .hover(|button| button.bg(rgb(palette.surface2)))
                            .child(format!("Remove {}", id))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.run_action(&remove_id, ActionKind::Remove, cx);
                                }),
                            ),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("cancel-remove-{id}")))
                            .px_2()
                            .h(px(24.))
                            .flex()
                            .items_center()
                            .rounded(px(6.))
                            .text_size(px(sm))
                            .cursor_pointer()
                            .text_color(rgb(palette.foreground_muted))
                            .hover(|button| button.bg(rgb(palette.surface2)))
                            .child("Cancel")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.confirm_remove = None;
                                    cx.notify();
                                }),
                            ),
                    );
            } else {
                controls = controls.child(
                    div()
                        .id(SharedString::from(format!("remove-{id}")))
                        .px_2()
                        .h(px(24.))
                        .flex()
                        .items_center()
                        .rounded(px(6.))
                        .text_size(px(sm))
                        .cursor_pointer()
                        .text_color(rgb(palette.status_danger))
                        .hover(|button| button.bg(rgb(palette.surface2)))
                        .child("Remove")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.confirm_remove = Some(id.clone());
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

    /// The panes must send the dotted request names the protocol schema
    /// declares; a real isolated daemon rejects anything else at the transport
    /// (see tests/daemon-rpc-contract.mts for the live check).
    #[test]
    fn plugin_request_names_match_the_protocol_schema() {
        let schema = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../packages/protocol/src/messages.ts"
        ))
        .expect("protocol messages source is readable from the workspace");
        for name in [
            PLUGIN_LIST_REQUEST,
            PLUGIN_ENABLE_REQUEST,
            PLUGIN_DISABLE_REQUEST,
            "plugin.list.response",
            "plugin.enable.response",
            "plugin.disable.response",
        ] {
            assert!(
                schema.contains(&format!("\"{name}\"")),
                "protocol schema must declare {name}"
            );
        }
    }
}

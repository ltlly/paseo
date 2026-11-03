mod attachments;
use std::{collections::BTreeMap, ops::Range, sync::Arc};

use gpui::{
    App, Bounds, ClipboardItem, Context, ElementInputHandler, EntityInputHandler, FocusHandle,
    Focusable, KeyDownEvent, MouseButton, Pixels, Point, Render, TextAlign, TextRun,
    UTF16Selection, UnderlineStyle, Window, WrappedLine, canvas, div, fill, font, point,
    prelude::*, px, rgb, size,
};
use paseo_desktop::{
    agent_controls::{Control, Controls},
    client::Connection,
    composer::ComposerState,
};
use serde_json::{Value, json};
use uuid::Uuid;

struct Line {
    start: usize,
    top: Pixels,
    shaped: WrappedLine,
}

pub struct Composer {
    agent_id: String,
    connection: Arc<Connection>,
    shared: crate::settings_view::SharedSettings,
    font_size: f32,
    line_height: f32,
    focus: FocusHandle,
    state: ComposerState,
    running: bool,
    connected: bool,
    cancel_request: Option<String>,
    lines: Vec<Line>,
    bounds: Bounds<Pixels>,
    scroll: Pixels,
    selecting: bool,
    controls: Controls,
    config_request: Option<String>,
    catalog_request: Option<String>,
    modes_request: Option<String>,
    picker: Option<(
        Control,
        gpui::Entity<crate::choice_menu::ChoiceMenu>,
        gpui::Subscription,
    )>,
    control_bounds: BTreeMap<Control, Bounds<Pixels>>,
    attachments: attachments::Attachments,
}

impl Composer {
    pub fn new(
        agent_id: String,
        connection: Arc<Connection>,
        shared: crate::settings_view::SharedSettings,
        running: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let settings = shared.get();
        let font_size = settings.content_font_size as f32;
        let mut view = Self {
            agent_id,
            connection,
            shared,
            font_size,
            line_height: (font_size * 1.4).round(),
            focus: cx.focus_handle(),
            state: ComposerState::default(),
            running,
            connected: true,
            cancel_request: None,
            lines: Vec::new(),
            bounds: Bounds::default(),
            scroll: px(0.),
            selecting: false,
            controls: Controls::default(),
            config_request: None,
            catalog_request: None,
            modes_request: None,
            picker: None,
            control_bounds: BTreeMap::new(),
            attachments: attachments::Attachments::default(),
        };
        view.refresh_config(cx);
        view
    }

    /// The optimistic user row shown above the composer until the host acknowledges the send.
    pub fn pending_sent(
        &self,
    ) -> Option<(String, String, Vec<crate::message_view::PreviewAttachment>)> {
        let attempt = self.state.pending.as_ref()?;
        let attachments = attempt
            .attachments
            .iter()
            .map(|(_, attachment)| match attachment {
                paseo_desktop::attachments::ReadyAttachment::Image(image) => {
                    crate::message_view::PreviewAttachment {
                        name: "Image".into(),
                        subtitle: image.mime_type.clone(),
                        image: None,
                    }
                }
                paseo_desktop::attachments::ReadyAttachment::File(file) => {
                    crate::message_view::PreviewAttachment {
                        name: file.file_name.clone(),
                        subtitle: format_size(file.size),
                        image: None,
                    }
                }
            })
            .collect();
        Some((
            attempt.message_id.clone(),
            attempt.text.clone(),
            attachments,
        ))
    }

    fn refresh_config(&mut self, cx: &mut Context<Self>) {
        match self
            .connection
            .request("fetch_agent_request", json!({"agentId":self.agent_id}))
        {
            Ok(id) => {
                self.config_request = Some(id.clone());
                self.config_timeout(id, cx);
            }
            Err(error) => self.controls.error = Some(error.to_string()),
        }
    }

    fn config_timeout(&self, request_id: String, cx: &mut Context<Self>) {
        let timer = cx
            .background_executor()
            .timer(std::time::Duration::from_secs(30));
        cx.spawn(async move |this, cx| {
            timer.await;
            let _ = this.update(cx, |this, cx| {
                let mut expired = false;
                if this.config_request.as_deref() == Some(&request_id) {
                    this.config_request = None;
                    expired = true;
                }
                if this.modes_request.as_deref() == Some(&request_id) {
                    this.modes_request = None;
                    expired = true;
                }
                if this.catalog_request.as_deref() == Some(&request_id) {
                    this.catalog_request = None;
                    expired = true;
                }
                expired |= this.controls.acknowledge(
                    &request_id,
                    false,
                    Some("No configuration acknowledgement. Refresh before retrying."),
                );
                if expired {
                    this.controls.error = Some("Agent configuration request timed out".into());
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub fn status(&mut self, payload: &Value, cx: &mut Context<Self>) {
        let revision = self.controls.revision;
        match self
            .controls
            .apply_snapshot(&self.agent_id, &payload["info"])
        {
            Ok(true) if self.controls.revision != revision => cx.notify(),
            Err(error) => {
                self.controls.error = Some(error.to_string());
                cx.notify();
            }
            _ => {}
        }
    }

    fn receive_config(&mut self, kind: &str, payload: &Value, cx: &mut Context<Self>) -> bool {
        let Some(id) = payload["requestId"].as_str() else {
            return false;
        };
        let error = payload["error"].as_str();
        if self.config_request.as_deref() == Some(id)
            && matches!(kind, "fetch_agent_response" | "rpc_error")
        {
            self.config_request = None;
            if let Some(error) = error {
                self.controls.error = Some(error.into());
            } else {
                match self
                    .controls
                    .apply_snapshot(&self.agent_id, &payload["agent"])
                {
                    Ok(true) => {
                        self.controls.error = None;
                        if self.controls.mode_visuals.is_none() && self.modes_request.is_none() {
                            let snapshot = self.controls.snapshot.as_ref().unwrap();
                            match self.connection.request(
                                "list_provider_modes_request",
                                json!({"provider":snapshot.provider,"cwd":snapshot.cwd}),
                            ) {
                                Ok(id) => {
                                    self.modes_request = Some(id.clone());
                                    self.config_timeout(id, cx);
                                }
                                Err(error) => self.controls.error = Some(error.to_string()),
                            }
                        }
                        if self.controls.models.is_none() && self.catalog_request.is_none() {
                            let snapshot = self.controls.snapshot.as_ref().unwrap();
                            match self.connection.request(
                                "list_provider_models_request",
                                json!({"provider":snapshot.provider,"cwd":snapshot.cwd}),
                            ) {
                                Ok(id) => {
                                    self.catalog_request = Some(id.clone());
                                    self.config_timeout(id, cx);
                                }
                                Err(error) => self.controls.error = Some(error.to_string()),
                            }
                        }
                    }
                    Ok(false) => {
                        self.controls.error = Some("Agent configuration was not returned".into())
                    }
                    Err(error) => self.controls.error = Some(error.to_string()),
                }
            }
        } else if self.modes_request.as_deref() == Some(id)
            && matches!(kind, "list_provider_modes_response" | "rpc_error")
        {
            self.modes_request = None;
            if let Some(error) = error {
                self.controls.error = Some(error.into());
            } else {
                match serde_json::from_value(payload["modes"].clone()) {
                    Ok(modes) => self.controls.mode_visuals = Some(modes),
                    Err(error) => self.controls.error = Some(format!("Cannot load modes: {error}")),
                }
            }
        } else if self.catalog_request.as_deref() == Some(id)
            && matches!(kind, "list_provider_models_response" | "rpc_error")
        {
            self.catalog_request = None;
            if let Some(error) = error {
                self.controls.error = Some(error.into());
            } else if let Err(error) = self.controls.apply_models(&payload["models"]) {
                self.controls.error = Some(error.to_string());
            }
        } else if matches!(
            kind,
            "set_agent_model_response"
                | "set_agent_thinking_response"
                | "set_agent_mode_response"
                | "rpc_error"
        ) && self.controls.acknowledge(
            id,
            payload["accepted"] == true && error.is_none(),
            error,
        ) {
            if payload["accepted"] == true && error.is_none() {
                self.refresh_config(cx);
            }
        } else {
            return false;
        }
        cx.notify();
        true
    }

    fn open_picker(&mut self, control: Control, window: &mut Window, cx: &mut Context<Self>) {
        if !self.connected || self.controls.pending.is_some() {
            return;
        }
        if self
            .picker
            .as_ref()
            .is_some_and(|(open, _, _)| *open == control)
        {
            self.picker = None;
            self.focus.focus(window, cx);
            cx.notify();
            return;
        }
        let rows = self.controls.options(control);
        if rows.is_empty() {
            self.refresh_config(cx);
            cx.notify();
            return;
        }
        let searchable = control != Control::Thinking || rows.len() > 6;
        let selected = self.controls.selected(control).map(str::to_owned);
        let menu = cx.new(|cx| {
            crate::choice_menu::ChoiceMenu::new(
                rows,
                selected,
                control.title(),
                searchable,
                self.shared.clone(),
                cx,
            )
        });
        let subscription = cx.subscribe_in(
            &menu,
            window,
            move |this, _, event: &crate::choice_menu::Pick, window, cx| {
                this.picker = None;
                this.focus.focus(window, cx);
                if let Some(value) = &event.0 {
                    // The host owns effective configuration. Never optimistically relabel the trigger.
                    let (kind, payload) = control.request(&this.agent_id, value);
                    match this.connection.request(kind, payload) {
                        Ok(id) => {
                            this.controls.error = None;
                            this.controls.pending = Some((id.clone(), control));
                            this.config_timeout(id, cx);
                        }
                        Err(error) => this.controls.error = Some(error.to_string()),
                    }
                }
                cx.notify();
            },
        );
        menu.read(cx).focus_handle(cx).focus(window, cx);
        self.picker = Some((control, menu, subscription));
        cx.notify();
    }

    fn control_trigger(
        &self,
        control: Control,
        palette: &paseo_desktop::appearance::Palette,
        base: f32,
        ui_family: &gpui::SharedString,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let entity = cx.entity();
        let label = self.controls.label(control);
        let pending = self.controls.pending.is_some() || !self.connected;
        let glyph = match control {
            Control::Model => None,
            Control::Thinking => Some("brain"),
            Control::Mode => Some(self.controls.mode_icon()),
        };
        let selected = self
            .picker
            .as_ref()
            .is_some_and(|(kind, _, _)| *kind == control);
        let mut button = div()
            .id(("agent-control", control as usize))
            .relative()
            .h(px(28.))
            .min_w_0()
            .px_2()
            .rounded(px(16.))
            .flex()
            .items_center()
            .gap_1()
            .font_family(ui_family.clone())
            .text_size(px(base))
            .text_color(rgb(palette.foreground_muted))
            .when(pending, |v| v.opacity(0.5))
            .when(!pending, |v| v.cursor_pointer())
            .when(selected, |v| v.bg(rgb(palette.surface0)))
            .hover(|v| v.bg(rgb(palette.surface2)))
            .child(
                canvas(
                    move |bounds, _, cx| {
                        entity.update(cx, |this, _| {
                            this.control_bounds.insert(control, bounds);
                        })
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
                    this.open_picker(control, window, cx);
                }),
            );
        if control == Control::Model {
            if let Some(snapshot) = &self.controls.snapshot {
                button = button.child(crate::chrome::provider_icon(&snapshot.provider, 16.));
            }
        } else if let Some(glyph) = glyph {
            button = button.child(crate::chrome::icon(glyph, 16.));
        }
        button
            .child(div().min_w_0().truncate().child(label))
            .child(crate::chrome::icon("chevron-down", 14.))
            .into_any_element()
    }

    pub fn set_running(&mut self, running: bool, cx: &mut Context<Self>) {
        if self.running != running {
            self.running = running;
            cx.notify();
        }
    }

    pub fn reconnected(&mut self, cx: &mut Context<Self>) {
        self.connected = true;
        self.refresh_config(cx);
        cx.notify();
    }

    pub fn reload_agent_config(&mut self, cx: &mut Context<Self>) {
        self.controls.models = None;
        self.controls.mode_visuals = None;
        self.refresh_config(cx);
        cx.notify();
    }

    /// Appearance or content-size change. Draft shaping depends on the content
    /// size and family, so the cached lines are rebuilt on the next layout.
    pub fn settings_changed(&mut self, cx: &mut Context<Self>) {
        let settings = self.shared.get();
        let font_size = settings.content_font_size as f32;
        self.font_size = font_size;
        self.line_height = (font_size * 1.4).round();
        self.lines.clear();
        cx.notify();
    }

    pub fn disconnected(&mut self, cx: &mut Context<Self>) {
        self.connected = false;
        self.attachment_disconnected();
        self.state.disconnected();
        self.cancel_request = None;
        self.picker = None;
        self.config_request = None;
        self.catalog_request = None;
        self.modes_request = None;
        self.controls.pending = None;
        cx.notify();
    }

    pub fn is_disposable(&self) -> bool {
        self.state.is_disposable()
            && self.cancel_request.is_none()
            && self.controls.pending.is_none()
    }

    pub fn release_layout(&mut self) {
        // Navigation retains text and delivery state, not the hidden editor's glyph layouts.
        self.lines = Vec::new();
        self.picker = None;
        self.release_attachment_layout();
        self.control_bounds.clear();
        self.selecting = false;
    }

    pub fn receive(&mut self, kind: &str, payload: &Value, cx: &mut Context<Self>) -> bool {
        self.prune_attachment_previews();
        if self.receive_attachment(kind, payload, cx) {
            return true;
        }
        if self.receive_config(kind, payload, cx) {
            return true;
        }
        let Some(request_id) = payload["requestId"].as_str() else {
            return false;
        };
        if kind == "rpc_error" || payload["agentId"].as_str() == Some(&self.agent_id) {
            let send = self
                .state
                .pending
                .as_ref()
                .is_some_and(|a| a.request_id == request_id);
            let cancel = self.cancel_request.as_deref() == Some(request_id);
            if send && matches!(kind, "send_agent_message_response" | "rpc_error") {
                self.state.acknowledge(
                    request_id,
                    payload["accepted"] == true,
                    payload["deliveryStatus"] == "queued",
                    payload["error"].as_str().map(str::to_owned),
                );
                self.prune_attachment_previews();
                cx.notify();
                return true;
            }
            if cancel && matches!(kind, "cancel_agent_response" | "rpc_error") {
                self.cancel_request = None;
                self.state.error = payload["error"].as_str().map(str::to_owned);
                if let Some(status) = payload["agent"]["status"].as_str() {
                    self.running = status == "running";
                }
                cx.notify();
                return true;
            }
        }
        false
    }

    fn can_send(&self) -> bool {
        self.connected && !self.running && self.state.can_send()
    }

    fn send(&mut self, cx: &mut Context<Self>) {
        if !self.can_send() {
            return;
        }
        let request_id = Uuid::new_v4().to_string();
        let attempt = self.state.begin(request_id.clone()).unwrap().clone();
        let connection = self.connection.clone();
        let agent_id = self.agent_id.clone();
        let task = cx.background_spawn(async move {
            connection.send_session_data(&attempt.message(&agent_id))
        });
        self.timeout(request_id.clone(), false, cx);
        cx.spawn(async move |this, cx| {
            if let Err(error) = task.await {
                let _ = this.update(cx, |this, cx| {
                    this.state
                        .acknowledge(&request_id, false, false, Some(error.to_string()));
                    cx.notify();
                });
            }
        })
        .detach();
        cx.notify();
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        if !self.connected || !self.running || self.cancel_request.is_some() {
            return;
        }
        match self
            .connection
            .request("cancel_agent_request", json!({"agentId":self.agent_id}))
        {
            Ok(id) => {
                self.cancel_request = Some(id.clone());
                self.timeout(id, true, cx);
            }
            Err(error) => self.state.error = Some(error.to_string()),
        }
        cx.notify();
    }

    fn timeout(&self, request_id: String, cancel: bool, cx: &mut Context<Self>) {
        let timer = cx
            .background_executor()
            .timer(std::time::Duration::from_secs(30));
        cx.spawn(async move |this, cx| {
            timer.await;
            let _ = this.update(cx, |this, cx| {
                if cancel && this.cancel_request.as_deref() == Some(&request_id) {
                    this.cancel_request = None;
                    this.state.error = Some("No cancellation acknowledgement. Check the agent before retrying.".into());
                } else if !cancel {
                    this.state.acknowledge(&request_id, false, false, Some("No delivery acknowledgement. Your draft is preserved; retry uses the same message ID.".into()));
                }
                cx.notify();
            });
        }).detach();
    }

    fn edited(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.notify_text_input_changed(&self.focus);
        cx.notify();
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = &event.keystroke;
        // IME owns navigation and Enter until its marked text has committed.
        if self.state.editing.marked.is_some() {
            return;
        }
        let shortcut = if cfg!(target_os = "macos") {
            key.modifiers.platform
        } else {
            key.modifiers.control
        };
        let extend = key.modifiers.shift;
        if shortcut && !key.modifiers.alt {
            match key.key.as_str() {
                "a" => {
                    self.state.editing.select(0, false);
                    self.state
                        .editing
                        .select(self.state.editing.text.len(), true);
                }
                "c" | "x" => {
                    let selection = self.state.editing.selection.clone();
                    if !selection.is_empty() {
                        cx.write_to_clipboard(ClipboardItem::new_string(
                            self.state.editing.text[selection].to_owned(),
                        ));
                        if key.key == "x" {
                            self.state.editing.replace(None, "", false, None);
                        }
                    }
                }
                "v" => {
                    if let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) {
                        self.state
                            .editing
                            .replace(None, &text.replace("\r\n", "\n"), false, None);
                    }
                }
                "enter" => {
                    self.send(cx);
                }
                _ => return,
            }
        } else if key.modifiers.control || key.modifiers.platform || key.modifiers.alt {
            return;
        } else {
            match key.key.as_str() {
                "enter" if !extend => {
                    self.send(cx);
                }
                "enter" => self.state.editing.replace(None, "\n", false, None),
                "backspace" => self.state.editing.delete(false),
                "delete" => self.state.editing.delete(true),
                "left" | "right" => self.state.editing.horizontal(key.key == "right", extend),
                "home" => {
                    let cursor = self.state.editing.cursor();
                    let start = self.state.editing.text[..cursor]
                        .rfind('\n')
                        .map_or(0, |i| i + 1);
                    self.state.editing.select(start, extend);
                }
                "end" => {
                    let cursor = self.state.editing.cursor();
                    let end = self.state.editing.text[cursor..]
                        .find('\n')
                        .map_or(self.state.editing.text.len(), |i| cursor + i);
                    self.state.editing.select(end, extend);
                }
                "up" | "down" => {
                    if let Some(position) = self.cursor_point(self.state.editing.cursor()) {
                        let target = position
                            + point(
                                px(0.),
                                px(if key.key == "up" {
                                    -self.line_height
                                } else {
                                    self.line_height
                                }),
                            );
                        let index = self.index_at(target);
                        self.state.editing.select(index, extend);
                    }
                }
                _ => return, // Printable text goes through the native text service exactly once.
            }
        }
        self.edited(window, cx);
        window.prevent_default();
        cx.stop_propagation();
    }

    fn cursor_point(&self, byte: usize) -> Option<Point<Pixels>> {
        let line = self.lines.iter().rev().find(|l| l.start <= byte)?;
        let local = line.shaped.position_for_index(
            (byte - line.start).min(line.shaped.len()),
            px(self.line_height),
        )?;
        Some(self.bounds.origin + point(px(0.), line.top - self.scroll) + local)
    }

    fn index_at(&self, position: Point<Pixels>) -> usize {
        let local = position - self.bounds.origin + point(px(0.), self.scroll);
        if local.y < px(0.) {
            return 0;
        }
        for line in &self.lines {
            if local.y < line.top + line.shaped.size(px(self.line_height)).height {
                let index = line
                    .shaped
                    .closest_index_for_position(
                        local - point(px(0.), line.top),
                        px(self.line_height),
                    )
                    .unwrap_or_else(|i| i);
                return (line.start + index).min(self.state.editing.text.len());
            }
        }
        self.state.editing.text.len()
    }

    fn layout(&mut self, bounds: Bounds<Pixels>, window: &mut Window) {
        self.bounds = bounds;
        let settings = self.shared.get();
        let palette = crate::settings_view::active_palette(&self.shared, window);
        let ui_family = crate::fonts::ui_family_for(&settings.ui_font_family);
        let editing = &self.state.editing;
        let placeholder = editing.text.is_empty();
        let text = if placeholder {
            "Message the agent, tag @files, or use /commands and /skills"
        } else {
            &editing.text
        };
        self.lines.clear();
        let mut top = px(0.);
        let mut start = 0;
        for hard_line in text.split('\n') {
            let end = start + hard_line.len();
            let mut cuts = vec![start, end];
            if !placeholder {
                cuts.extend([
                    editing.selection.start.clamp(start, end),
                    editing.selection.end.clamp(start, end),
                ]);
                if let Some(range) = &editing.marked {
                    cuts.extend([range.start.clamp(start, end), range.end.clamp(start, end)]);
                }
            }
            cuts.sort_unstable();
            cuts.dedup();
            let runs = cuts
                .windows(2)
                .map(|span| TextRun {
                    len: span[1] - span[0],
                    font: font(ui_family.clone()),
                    color: rgb(if placeholder {
                        palette.foreground_extra_muted
                    } else {
                        palette.foreground
                    })
                    .into(),
                    background_color: (!placeholder && editing.selection.contains(&span[0]))
                        .then(|| rgb(palette.surface3).into()),
                    underline: editing
                        .marked
                        .as_ref()
                        .is_some_and(|m| m.contains(&span[0]))
                        .then(|| UnderlineStyle {
                            thickness: px(1.),
                            color: None,
                            wavy: false,
                        }),
                    strikethrough: None,
                })
                .collect::<Vec<_>>();
            match window.text_system().shape_text(
                hard_line.to_owned().into(),
                px(self.font_size),
                &runs,
                Some(bounds.size.width),
                None,
            ) {
                Ok(lines) => {
                    for shaped in lines {
                        let height = shaped.size(px(self.line_height)).height;
                        self.lines.push(Line { start, top, shaped });
                        top += height;
                    }
                }
                Err(error) => self.state.error = Some(format!("Cannot display draft: {error}")),
            }
            start = end + 1;
        }
        if let Some(caret) = self.cursor_point(self.state.editing.cursor()) {
            if caret.y < bounds.top() {
                self.scroll -= bounds.top() - caret.y;
            }
            if caret.y + px(self.line_height) > bounds.bottom() {
                self.scroll += caret.y + px(self.line_height) - bounds.bottom();
            }
        }
        self.scroll = self
            .scroll
            .max(px(0.))
            .min((top - bounds.size.height).max(px(0.)));
    }
}

impl Focusable for Composer {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EntityInputHandler for Composer {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.state.editing.byte_range(range);
        *actual = Some(self.state.editing.utf16_range(range.clone()));
        Some(self.state.editing.text[range].to_owned())
    }
    fn selected_text_range(
        &mut self,
        _ignore: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self
                .state
                .editing
                .utf16_range(self.state.editing.selection.clone()),
            reversed: self.state.editing.reversed,
        })
    }
    fn set_selected_text_range(
        &mut self,
        range: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.editing.selection = self.state.editing.byte_range(range);
        self.state.editing.reversed = false;
        self.edited(window, cx);
    }
    fn text_length_utf16(
        &mut self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.state.editing.text.encode_utf16().count())
    }
    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.state
            .editing
            .marked
            .clone()
            .map(|r| self.state.editing.utf16_range(r))
    }
    fn unmark_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.state.editing.marked = None;
        self.edited(window, cx);
    }
    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.editing.replace(range, text, false, None);
        self.edited(window, cx);
    }
    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.editing.replace(range, text, true, selected);
        self.edited(window, cx);
    }
    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        _bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let byte = self.state.editing.byte_range(range).start;
        Some(Bounds::new(
            self.cursor_point(byte)?,
            size(px(1.), px(self.line_height)),
        ))
    }
    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.state.editing.utf16_offset(self.index_at(point)))
    }
}

impl Render for Composer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use crate::chrome::icon;
        let palette = crate::settings_view::active_palette(&self.shared, window);
        let settings = self.shared.get();
        let content_width = settings
            .content_max_width
            .unwrap_or(paseo_desktop::settings::DEFAULT_CONTENT_MAX_WIDTH)
            as f32;
        let font_base = paseo_desktop::appearance::scale_ui_font(
            crate::theme::font_size::BASE,
            settings.ui_base_font_size,
        );
        let font_sm = paseo_desktop::appearance::scale_ui_font(
            crate::theme::font_size::SM,
            settings.ui_base_font_size,
        );
        let ui_family = crate::fonts::ui_family_for(&settings.ui_font_family);
        let measure = cx.entity();
        let paint = cx.entity();
        let enabled = self.can_send();
        let controls = div()
            .h(px(28.))
            .mx(px(-6.))
            .flex()
            .items_center()
            .justify_between()
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .child(self.attachment_button(cx))
                    .child(
                        div().min_w_0().flex().items_center().gap_1().children(
                            [Control::Model, Control::Thinking, Control::Mode]
                                .into_iter()
                                .filter(|control| {
                                    *control == Control::Model && self.controls.models.is_some()
                                        || !self.controls.options(*control).is_empty()
                                })
                                .map(|control| {
                                    self.control_trigger(
                                        control, &palette, font_base, &ui_family, cx,
                                    )
                                }),
                        ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap_1()
                    .when(self.running, |row| {
                        row.child(
                            div()
                                .id("stop-agent")
                                .size(px(28.))
                                .rounded_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .bg(rgb(palette.accent))
                                .child(
                                    icon("square", 16.).text_color(rgb(palette.accent_foreground)),
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.cancel(cx)
                                    }),
                                ),
                        )
                    })
                    .child(
                        div()
                            .id("send-message")
                            .size(px(28.))
                            .rounded_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(rgb(palette.accent))
                            .when(!enabled, |button| button.opacity(0.5))
                            .when(enabled, |button| button.cursor_pointer())
                            .child(icon("arrow-up", 16.).text_color(rgb(palette.accent_foreground)))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.send(cx)
                                }),
                            ),
                    ),
            );
        let input = div()
            .id("message-input")
            .track_focus(&self.focus)
            .cursor_text()
            .rounded(px(crate::theme::radius::S_2XL))
            .font_family(ui_family.clone())
            .bg(rgb(palette.surface1))
            .border_1()
            .border_color(rgb(palette.border_accent))
            .w_full()
            .flex()
            .flex_col()
            .gap_3()
            .px_4()
            .pt_4()
            .pb_3()
            .on_drop(cx.listener(|this, paths: &gpui::ExternalPaths, _, cx| {
                this.add_attachment_paths(paths.0.iter().cloned().collect(), None, cx);
            }))
            .on_key_down(cx.listener(Self::key_down))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.focus.focus(window, cx);
                    this.selecting = true;
                    let index = this.index_at(event.position);
                    this.state.editing.select(index, event.modifiers.shift);
                    this.edited(window, cx);
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.selecting = false),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.selecting = false),
            )
            .on_mouse_move(
                cx.listener(|this, event: &gpui::MouseMoveEvent, window, cx| {
                    if this.selecting {
                        let index = this.index_at(event.position);
                        this.state.editing.select(index, true);
                        this.edited(window, cx);
                    }
                }),
            )
            .when(!self.state.attachments.is_empty(), |input| {
                input.child(self.attachment_tray(cx))
            })
            .child(
                canvas(
                    move |bounds, window, cx| {
                        measure.update(cx, |this, _| this.layout(bounds, window));
                    },
                    move |bounds, (), window, cx| {
                        paint.update(cx, |view, cx| {
                            window.handle_input(
                                &view.focus,
                                ElementInputHandler::new(bounds, paint.clone()),
                                cx,
                            );
                            window.with_content_mask(
                                Some(gpui::ContentMask { bounds }),
                                |window| {
                                    for line in &view.lines {
                                        let origin =
                                            bounds.origin + point(px(0.), line.top - view.scroll);
                                        if origin.y + line.shaped.size(px(view.line_height)).height
                                            < bounds.top()
                                            || origin.y > bounds.bottom()
                                        {
                                            continue;
                                        }
                                        let _ = line.shaped.paint_background(
                                            origin,
                                            px(view.line_height),
                                            TextAlign::Left,
                                            Some(bounds),
                                            window,
                                            cx,
                                        );
                                        let _ = line.shaped.paint(
                                            origin,
                                            px(view.line_height),
                                            TextAlign::Left,
                                            Some(bounds),
                                            window,
                                            cx,
                                        );
                                    }
                                    if view.focus.is_focused(window)
                                        && let Some(origin) =
                                            view.cursor_point(view.state.editing.cursor())
                                    {
                                        window.paint_quad(fill(
                                            Bounds::new(origin, size(px(1.), px(view.line_height))),
                                            rgb(palette.foreground),
                                        ));
                                    }
                                },
                            );
                        });
                    },
                )
                .w_full()
                .h(px(46.)),
            )
            .child(controls);
        div()
            .id("agent-composer")
            .relative()
            .w_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .items_center()
            .font_family(ui_family.clone())
            .px_4()
            .pb_4()
            .child(
                div()
                    .w_full()
                    .max_w(px(content_width))
                    .child(input)
                    .when_some(
                        self.state
                            .error
                            .clone()
                            .or(self.controls.error.clone())
                            .or(self.attachments.error.clone()),
                        |view, error| {
                            view.child(
                                div()
                                    .text_size(px(font_sm))
                                    .text_color(rgb(palette.status_danger))
                                    .child(error)
                                    .when(self.attachments.error.is_some(), |line| {
                                        line.child(
                                            div()
                                                .id("dismiss-attachment-error")
                                                .ml_2()
                                                .cursor_pointer()
                                                .text_color(rgb(palette.foreground_muted))
                                                .child("Dismiss")
                                                .on_mouse_down(
                                                    MouseButton::Left,
                                                    cx.listener(|this, _, _, cx| {
                                                        cx.stop_propagation();
                                                        this.attachments.error = None;
                                                        cx.notify();
                                                    }),
                                                ),
                                        )
                                    })
                                    .when(
                                        self.controls.error.is_some() && self.connected,
                                        |line| {
                                            line.child(
                                                div()
                                                    .id("retry-agent-controls")
                                                    .ml_2()
                                                    .cursor_pointer()
                                                    .text_color(rgb(palette.foreground_muted))
                                                    .child("Retry")
                                                    .on_mouse_down(
                                                        MouseButton::Left,
                                                        cx.listener(|this, _, _, cx| {
                                                            cx.stop_propagation();
                                                            this.controls.error = None;
                                                            this.refresh_config(cx);
                                                            cx.notify();
                                                        }),
                                                    ),
                                            )
                                        },
                                    ),
                            )
                        },
                    ),
            )
            .children(self.attachment_menu(cx))
            .children(self.attachment_preview(window, cx))
            .when_some(self.picker.as_ref(), |root, (control, menu, _)| {
                let bounds = self
                    .control_bounds
                    .get(control)
                    .copied()
                    .unwrap_or_default();
                root.child(gpui::deferred(
                    gpui::anchored()
                        .position(point(bounds.left(), bounds.top() - px(8.)))
                        .anchor(gpui::Anchor::BottomLeft)
                        .snap_to_window_with_margin(px(8.))
                        .child(
                            div()
                                .w(px(match control {
                                    Control::Model => 360.,
                                    Control::Thinking => 200.,
                                    Control::Mode => 260.,
                                }))
                                .child(menu.clone()),
                        ),
                ))
            })
    }
}

fn format_size(bytes: u64) -> String {
    const KB: f64 = 1024.;
    const MB: f64 = KB * 1024.;
    let bytes = bytes as f64;
    if bytes >= MB {
        format!("{:.1} MB", bytes / MB)
    } else if bytes >= KB {
        format!("{:.0} KB", bytes / KB)
    } else {
        format!("{bytes:.0} B")
    }
}

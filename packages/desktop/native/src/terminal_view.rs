mod glyphs;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use std::{ops::Range, sync::Arc, time::Instant};

use alacritty_terminal::{term::TermMode, vte::ansi::CursorShape};
use anyhow::{Result, ensure};
use gpui::{
    App, Bounds, Context, ElementInputHandler, EntityInputHandler, FocusHandle, Focusable,
    KeyDownEvent, MouseButton, PathBuilder, Pixels, Point, Render, ScrollEvent, TextRun,
    UTF16Selection, Window, canvas, div, fill, point, prelude::*, px, rgb, rgba, size,
};
use serde_json::{Value, json};

use paseo_desktop::{
    client::Connection,
    mouse::{
        Action as MouseAction, Button as PointerButton, Event as PointerEvent,
        Reporter as MouseReporter,
    },
    terminal::{GridSize, TerminalBuffer},
    wire::{TerminalFrame, TerminalOpcode},
};

use crate::settings_view::{SharedSettings, system_dark};

const SCROLLBAR_WIDTH: f32 = 8.;

/// Links and scrollbar geometry belong to the visible grid, so a cache is only
/// valid for the row it was built for, the scroll offset and the buffer state it
/// was derived from.
struct LinkCache {
    row: usize,
    display_offset: usize,
    generation: u64,
    links: Vec<paseo_desktop::terminal_links::Link>,
}

struct ScrollDrag {
    origin: Point<Pixels>,
    scroll_top: f32,
}

struct FindState {
    query: String,
    /// IME pre-edit for the query. It is painted in the find bar, never in the
    /// grid, so composing CJK does not underline the shell cursor.
    marked: String,
    matches: Vec<paseo_desktop::terminal_find::Match>,
    limited: bool,
    active: Option<usize>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MenuAction {
    Copy,
    Paste,
    SelectAll,
}

struct FindBar {
    bar: Bounds<Pixels>,
    previous: Bounds<Pixels>,
    next: Bounds<Pixels>,
    close: Bounds<Pixels>,
}

pub struct TerminalView {
    terminal_id: String,
    connection: Arc<Connection>,
    shared: SharedSettings,
    focus: FocusHandle,
    buffer: TerminalBuffer,
    scrollback: usize,
    slot: Option<u8>,
    request_id: Option<String>,
    replay_ready: bool,
    pending_input: Vec<u8>,
    desired_size: GridSize,
    bounds: Bounds<Pixels>,
    cell_width: Pixels,
    row_height: f32,
    wheel: paseo_desktop::mouse::Wheel,
    marked: String,
    error: Option<String>,
    visible: bool,
    opening: bool,
    needs_claim: bool,
    selecting: bool,
    selection_pointer: Point<Pixels>,
    selection_scroll_task: Option<gpui::Task<()>>,
    mouse_buttons: u8,
    mouse_reporter: MouseReporter,
    mouse_error: bool,
    raw_input: bool,
    raw_requests: paseo_desktop::terminal_retention::PendingInput,
    glyphs: glyphs::Glyphs,
    blink: paseo_desktop::terminal_geometry::CursorBlink,
    blink_task: Option<gpui::Task<()>>,
    focused: bool,
    generation: u64,
    hover_cell: Option<(usize, usize)>,
    hover_link: Option<paseo_desktop::terminal_links::Link>,
    link_cache: Option<LinkCache>,
    pressed_link: Option<paseo_desktop::terminal_links::Link>,
    pressed_link_at: Option<Point<Pixels>>,
    link_dragged: bool,
    scroll_revealed: Option<Instant>,
    scroll_dragging: Option<ScrollDrag>,
    scroll_hovered: bool,
    scroll_fade_task: Option<gpui::Task<()>>,
    find: Option<FindState>,
    find_bar: Option<FindBar>,
    find_refresh_task: Option<gpui::Task<()>>,
    menu: Option<Point<Pixels>>,
    menu_items: Vec<(MenuAction, Bounds<Pixels>)>,
    _subscriptions: Vec<gpui::Subscription>,
}

impl TerminalView {
    pub fn new(
        terminal_id: String,
        connection: Arc<Connection>,
        shared: SharedSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        let initial = GridSize { rows: 24, cols: 80 };
        let scrollback = shared.get().terminal_scrollback_lines.max(0) as usize;
        let subscriptions = vec![
            cx.on_focus(&focus, window, |this, window, cx| {
                this.sync_cursor(window, cx)
            }),
            cx.on_blur(&focus, window, |this, window, cx| {
                this.sync_cursor(window, cx)
            }),
            cx.observe_window_activation(window, |this, window, cx| this.sync_cursor(window, cx)),
        ];
        let focused = focus.is_focused(window) && window.is_window_active();
        Self {
            terminal_id,
            connection,
            shared,
            focus,
            buffer: TerminalBuffer::new(initial, scrollback),
            scrollback,
            slot: None,
            request_id: None,
            replay_ready: false,
            pending_input: Vec::new(),
            desired_size: initial,
            bounds: Bounds::default(),
            cell_width: px(8.),
            row_height: 17.,
            wheel: paseo_desktop::mouse::Wheel::default(),
            marked: String::new(),
            error: None,
            visible: true,
            opening: false,
            needs_claim: true,
            selecting: false,
            selection_pointer: Point::default(),
            selection_scroll_task: None,
            mouse_buttons: 0,
            mouse_reporter: MouseReporter::default(),
            mouse_error: false,
            raw_input: false,
            raw_requests: paseo_desktop::terminal_retention::PendingInput::default(),
            glyphs: glyphs::Glyphs::default(),
            blink: paseo_desktop::terminal_geometry::CursorBlink::new(Instant::now()),
            blink_task: None,
            focused,
            generation: 0,
            hover_cell: None,
            hover_link: None,
            link_cache: None,
            pressed_link: None,
            pressed_link_at: None,
            link_dragged: false,
            scroll_revealed: None,
            scroll_dragging: None,
            scroll_hovered: false,
            scroll_fade_task: None,
            find: None,
            find_bar: None,
            find_refresh_task: None,
            menu: None,
            menu_items: Vec::new(),
            _subscriptions: subscriptions,
        }
    }

    fn sync_cursor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focused = self.focus.is_focused(window) && window.is_window_active();
        if !self.focused {
            self.selecting = false;
            self.selection_scroll_task = None;
        }
        let screen = self.buffer.screen();
        let enabled =
            self.visible && self.focused && screen.cursor.is_some() && screen.cursor_style.blinking;
        self.blink.enable(enabled, Instant::now());
        self.schedule_blink(cx);
        self.notify_visible(cx);
    }

    fn schedule_blink(&mut self, cx: &mut Context<Self>) {
        let Some(delay) = self.blink.next(Instant::now()) else {
            self.blink_task = None;
            return;
        };
        if self.blink_task.is_some() {
            return;
        }
        self.blink_task = Some(cx.spawn(async move |this, cx| {
            let mut delay = delay;
            loop {
                cx.background_executor().timer(delay).await;
                let next = this.update(cx, |this, cx| {
                    let was_visible = this.blink.visible();
                    this.blink.tick(Instant::now());
                    if was_visible != this.blink.visible() {
                        this.notify_visible(cx);
                    }
                    let next = this.blink.next(Instant::now());
                    if next.is_none() {
                        this.blink_task = None;
                    }
                    next
                });
                match next {
                    Ok(Some(next)) => delay = next,
                    _ => break,
                }
            }
        }));
    }

    // Slot binding can still be in flight when a workspace is left. The daemon's
    // ID-addressed input lane preserves the already-committed bytes before unsubscribe.
    pub fn prepare_detach(&mut self) -> Result<()> {
        self.raw_requests.prepare_detach()?;
        if !self.pending_input.is_empty() {
            let text = std::str::from_utf8(&self.pending_input)?;
            self.connection.send_session(json!({"type":"terminal_input","terminalId":self.terminal_id,"message":{"type":"input","data":text}}))?;
            self.pending_input.clear();
        }
        Ok(())
    }
    pub fn slot(&self) -> Option<u8> {
        self.slot
    }
    pub fn can_release(&self) -> bool {
        !self.opening && self.pending_input.is_empty() && self.raw_requests.is_empty()
    }
    pub fn owns_request(&self, payload: &Value) -> bool {
        self.opening
            && self
                .request_id
                .as_ref()
                .is_some_and(|id| payload["requestId"].as_str() == Some(id.as_str()))
    }
    pub fn visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible != visible {
            self.visible = visible;
            if !visible {
                self.selecting = false;
                self.selection_scroll_task = None;
                self.mouse_buttons = 0;
                self.glyphs.clear();
                self.blink.enable(false, Instant::now());
                self.blink_task = None;
                self.scroll_dragging = None;
                self.scroll_hovered = false;
                self.scroll_fade_task = None;
                self.hover_cell = None;
                self.hover_link = None;
            }
            if visible {
                cx.notify();
            }
        }
    }
    /// Appearance, terminal font, or scrollback settings changed. Colors and
    /// glyph metrics are re-derived from the shared settings in render; the
    /// scrollback cap is applied now because a hidden retained terminal keeps
    /// receiving output under the old bound.
    pub fn settings_changed(&mut self, cx: &mut Context<Self>) {
        self.sync_history();
        cx.notify();
    }
    fn sync_history(&mut self) {
        let scrollback = self.shared.get().terminal_scrollback_lines.max(0) as usize;
        if self.scrollback != scrollback {
            self.scrollback = scrollback;
            self.buffer.set_history(scrollback);
        }
    }
    pub fn request_claim(&mut self, cx: &mut Context<Self>) {
        self.needs_claim = true;
        self.notify_visible(cx);
    }
    pub fn activate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.error.is_some() && self.slot.is_none() && !self.opening {
            self.connection
                .unsubscribe(&format!("terminal:{}", self.terminal_id));
            self.request_id = None;
            self.error = None;
            self.replay_ready = false;
        }
        self.visible = window.is_visible();
        self.needs_claim = true;
        self.focus.focus(window, cx);
        self.blink.restart(Instant::now());
        self.sync_cursor(window, cx);
        cx.notify();
    }
    fn notify_visible(&self, cx: &mut Context<Self>) {
        if self.visible {
            cx.notify();
        }
    }
    pub fn bind(&mut self, payload: &Value, cx: &mut Context<Self>) -> bool {
        if !self.owns_request(payload) {
            return false;
        }
        self.opening = false;
        if let Some(error) = payload["error"].as_str() {
            self.error = Some(error.into());
        } else {
            // This host-owned field is the capability boundary: old workers and
            // ConPTY remain usable through the established UTF-8 input lane.
            self.raw_input = payload["rawByteInput"].as_bool() == Some(true);
            self.slot = payload["slot"]
                .as_u64()
                .and_then(|slot| u8::try_from(slot).ok());
        }
        self.notify_visible(cx);
        true
    }

    pub fn owns_raw_request(&self, payload: &Value) -> bool {
        payload["requestId"]
            .as_str()
            .is_some_and(|id| self.raw_requests.contains(id))
    }

    pub fn receive_raw(&mut self, payload: &Value, cx: &mut Context<Self>) {
        let Some(id) = payload["requestId"].as_str() else {
            return;
        };
        if !self.raw_requests.settle(id) {
            return;
        }
        if payload["terminalId"].as_str() != Some(self.terminal_id.as_str())
            || payload["success"].as_bool() != Some(true)
        {
            self.error = Some(
                payload["error"]
                    .as_str()
                    .unwrap_or("Raw terminal input was not accepted.")
                    .into(),
            );
            self.mouse_error = false;
        }
        self.notify_visible(cx);
    }

    fn send_raw(&mut self, bytes: &[u8], repeat: usize, cx: &mut Context<Self>) -> Result<()> {
        ensure!(
            self.raw_input,
            "This terminal does not support raw-byte input. Update its host and open a terminal on the new worker; existing terminals remain usable with SGR mouse mode."
        );
        self.raw_requests.admit()?;
        let count = repeat.min(1000);
        ensure!(
            bytes.len().saturating_mul(count) <= 64 * 1024,
            "Raw terminal input exceeds 64 KiB."
        );
        if self.needs_claim {
            self.claim(cx);
        }
        self.flush_input()?;
        self.selecting = false;
        if self.buffer.prepare_input() {
            self.notify_visible(cx);
        }
        let id = self.connection.request("terminal.input.write_raw.request", json!({"terminalId":self.terminal_id,"dataBase64":STANDARD.encode(bytes.repeat(count))}))?;
        self.raw_requests.insert(id.clone());
        let timer = cx
            .background_executor()
            .timer(std::time::Duration::from_secs(30));
        cx.spawn(async move |this, cx| {
            timer.await;
            let _ = this.update(cx, |this, cx| {
                if this.raw_requests.settle(&id) {
                    this.error = Some("Terminal input acceptance timed out. It may have reached the program and was not retried.".into());
                    this.mouse_error = false;
                    this.notify_visible(cx);
                }
            });
        }).detach();
        Ok(())
    }

    pub fn disconnected(&mut self, cx: &mut Context<Self>) {
        self.slot = None;
        self.request_id = None;
        self.replay_ready = false;
        self.buffer.replaying = false;
        self.opening = false;
        self.pending_input.clear();
        self.raw_input = false;
        self.raw_requests = paseo_desktop::terminal_retention::PendingInput::default();
        self.selecting = false;
        self.selection_scroll_task = None;
        self.blink.enable(false, Instant::now());
        self.blink_task = None;
        self.error = Some("Host disconnected. Submitted input may have reached the program; it will not be retried.".into());
        self.notify_visible(cx);
    }

    pub fn reconnect(&mut self, cx: &mut Context<Self>) {
        let had_selection = self.buffer.has_selection();
        let had_scroll = self.buffer.screen().cursor.is_none();
        self.slot = None;
        self.request_id = None;
        self.replay_ready = false;
        self.opening = false;
        self.error = (had_selection || had_scroll).then(|| "Reconnecting terminal; screen replay replaces the old selection and scroll position.".into());
        self.needs_claim = false;
        let rows = self.desired_size.rows;
        let cols = self.desired_size.cols;
        match self.connection.subscribe(&format!("terminal:{}", self.terminal_id), "subscribe_terminal_request", json!({"terminalId":self.terminal_id,"restore":{"mode":"full-snapshot","scrollbackLines":self.scrollback,"size":{"rows":rows,"cols":cols}}})) {
            Ok(id) => { self.request_id = Some(id); self.opening = true; }
            Err(error) => self.error = Some(error.to_string()),
        }
        self.notify_visible(cx);
        let id = self.request_id.clone();
        if let Some(id) = id {
            let timer = cx
                .background_executor()
                .timer(std::time::Duration::from_secs(30));
            cx.spawn(async move |this, cx| {
                timer.await;
                let _ = this.update(cx, |this, cx| {
                    if this.opening && this.request_id.as_ref() == Some(&id) {
                        this.opening = false;
                        this.error = Some(
                            "Timed out reconnecting terminal. Select this tab to retry.".into(),
                        );
                        this.notify_visible(cx);
                    }
                });
            })
            .detach();
        }
    }

    pub fn receive(&mut self, frame: TerminalFrame, cx: &mut Context<Self>) {
        if self.slot != Some(frame.slot) {
            return;
        }
        // Hidden terminals still apply frames; keep the buffer bound current
        // before output lands instead of relying on the next render.
        self.sync_history();
        let completes_replay = matches!(
            frame.opcode,
            TerminalOpcode::ReplayEnd | TerminalOpcode::Restore
        );
        let pointer_revision = self.buffer.mouse_protocol_revision();
        let reset_revision = self.buffer.mouse_reset_revision();
        let replaces_buffer = matches!(
            frame.opcode,
            TerminalOpcode::ReplayBegin | TerminalOpcode::Restore
        );
        let cursor_before = self.buffer.cursor_state();
        let gesture_before = self.buffer.selection_is_gesture();
        let result = self.buffer.apply(frame).and_then(|()| {
            if gesture_before && !self.buffer.selection_is_gesture() {
                self.selecting = false;
                self.selection_scroll_task = None;
            }
            if replaces_buffer || self.buffer.cursor_state() != cursor_before {
                self.blink.restart(Instant::now());
            }
            if replaces_buffer || self.buffer.mouse_reset_revision() != reset_revision {
                self.mouse_reporter.reset();
            }
            if replaces_buffer || self.buffer.mouse_protocol_revision() != pointer_revision {
                self.mouse_buttons = 0;
                self.selecting = false;
            }
            if self.mouse_error
                && (!self.buffer.mouse_modes().active() || self.buffer.mouse_modes().encoding != 0)
            {
                self.error = None;
                self.mouse_error = false;
            }
            self.generation += 1;
            self.hover_cell = None;

            if completes_replay {
                self.replay_ready = true;
            }
            self.resize_if_ready()?;
            if self.needs_claim {
                self.claim(cx);
            }
            self.flush_input()
        });
        self.finish(result, cx);
        self.schedule_find_rescan(cx);
        self.notify_visible(cx);
    }

    fn measure(
        &mut self,
        bounds: Bounds<Pixels>,
        metrics: paseo_desktop::terminal_geometry::Metrics,
        cx: &mut Context<Self>,
    ) {
        let width = px(metrics.width);
        self.row_height = metrics.height;
        self.bounds = bounds;
        self.cell_width = width;
        let rows = (f32::from(bounds.size.height) / self.row_height)
            .floor()
            .clamp(1., 1000.) as u16;
        let cols = ((bounds.size.width - px(SCROLLBAR_WIDTH)) / width)
            .floor()
            .clamp(2., 1000.) as u16;
        self.desired_size = GridSize { rows, cols };
        if self.request_id.is_none() {
            self.buffer.resize(self.desired_size);
            let result = self.connection.subscribe(&format!("terminal:{}", self.terminal_id), "subscribe_terminal_request", json!({"terminalId":self.terminal_id,"restore":{"mode":"full-snapshot","scrollbackLines":self.scrollback,"size":{"rows":rows,"cols":cols}}}));
            match result {
                Ok(id) => {
                    self.request_id = Some(id.clone());
                    self.opening = true;
                    let timer = cx
                        .background_executor()
                        .timer(std::time::Duration::from_secs(30));
                    cx.spawn(async move |this,cx|{timer.await;let _=this.update(cx,|this,cx|{
                        if this.opening && this.request_id.as_ref()==Some(&id){
                            this.opening=false;this.error=Some("Timed out opening this terminal. Select the tab again to retry.".into());this.notify_visible(cx);
                        }
                    });}).detach();
                }
                Err(error) => self.error = Some(error.to_string()),
            }
        } else {
            let result = self.resize_if_ready();
            self.finish(result, cx);
            if self.needs_claim {
                self.claim(cx);
            }
        }
    }

    fn resize_if_ready(&mut self) -> Result<()> {
        let Some(slot) = self.slot else {
            return Ok(());
        };
        if !self.visible
            || !self.replay_ready
            || self.buffer.replaying
            || self.desired_size == self.buffer.size
        {
            return Ok(());
        }
        self.selecting = false;
        self.selection_scroll_task = None;
        self.buffer.resize(self.desired_size);
        self.connection.send_terminal(TerminalFrame { opcode: TerminalOpcode::Resize, slot, data: serde_json::to_vec(&json!({"rows":self.desired_size.rows,"cols":self.desired_size.cols,"intent":"update"}))? })
    }

    fn claim(&mut self, cx: &mut Context<Self>) {
        if !self.visible || !self.replay_ready || self.buffer.replaying {
            return;
        }
        let result = self.connection.send_session(json!({"type":"terminal_input","terminalId":self.terminal_id,"message":{"type":"resize","rows":self.desired_size.rows,"cols":self.desired_size.cols,"intent":"claim"}}));
        if result.is_ok() {
            self.needs_claim = false;
        }
        self.finish(result, cx);
    }

    fn send_input(&mut self, text: &str, cx: &mut Context<Self>) {
        if text.is_empty() {
            return;
        }
        if self.needs_claim {
            self.claim(cx);
        }
        let result = (|| {
            ensure!(
                self.pending_input.len() + text.len() <= 1024 * 1024,
                "terminal pending input exceeds 1 MiB"
            );
            self.selecting = false;
            if self.buffer.prepare_input() {
                self.notify_visible(cx);
            }
            self.pending_input.extend_from_slice(text.as_bytes());
            self.flush_input()
        })();
        self.finish(result, cx);
    }

    fn flush_input(&mut self) -> Result<()> {
        let Some(slot) = self.slot else {
            return Ok(());
        };
        if !self.replay_ready || self.pending_input.is_empty() {
            return Ok(());
        }
        self.connection.send_terminal(TerminalFrame {
            opcode: TerminalOpcode::Input,
            slot,
            data: self.pending_input.clone(),
        })?;
        self.pending_input.clear();
        Ok(())
    }

    fn finish(&mut self, result: Result<()>, cx: &mut Context<Self>) {
        if let Err(error) = result {
            self.error = Some(error.to_string());
            self.mouse_error = false;
            self.notify_visible(cx);
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = &event.keystroke;
        let plain_modifiers = !key.modifiers.alt;
        let clipboard_shortcut = plain_modifiers
            && if cfg!(target_os = "macos") {
                key.modifiers.platform && !key.modifiers.control
            } else {
                key.modifiers.control && !key.modifiers.platform
            };
        if clipboard_shortcut && key.key.eq_ignore_ascii_case("f") {
            self.open_find(cx);
            cx.stop_propagation();
            return;
        }
        if self.find.is_some() {
            if clipboard_shortcut && key.key.eq_ignore_ascii_case("v") {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    self.find_input(&text, cx);
                }
                cx.stop_propagation();
                return;
            }
            match key.key.as_str() {
                "escape" => self.close_find(cx),
                "enter" => self.step_find(!key.modifiers.shift, cx),
                "backspace" => self.find_backspace(cx),
                // GPUI dispatches committed text only after an unconsumed key
                // event. Stopping every event leaves an open, untypeable bar.
                // The input handler routes that text to find_input, not the PTY.
                _ if key.key_char.is_some()
                    || (!key.modifiers.control && !key.modifiers.platform) =>
                {
                    return;
                }
                _ => {}
            }
            cx.stop_propagation();
            return;
        }
        if self.menu.is_some() {
            if key.key == "escape" {
                self.close_menu(cx);
            }
            cx.stop_propagation();
            return;
        }
        if clipboard_shortcut
            && key.key.eq_ignore_ascii_case("c")
            && (self.buffer.has_selection() || cfg!(target_os = "macos") || key.modifiers.shift)
        {
            if let Some(text) = self.buffer.selected_text() {
                let text =
                    paseo_desktop::keyboard::clipboard_text(text, cfg!(target_os = "windows"));
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
            }
            cx.stop_propagation();
            return;
        }
        if clipboard_shortcut && key.key.eq_ignore_ascii_case("v") {
            if let Some(text) = cx
                .read_from_clipboard()
                .and_then(|item| item.text())
                .filter(|text| !text.is_empty())
            {
                let text = paseo_desktop::keyboard::paste(
                    &text,
                    self.buffer.modes().contains(TermMode::BRACKETED_PASTE),
                );
                self.send_input(&text, cx);
                cx.notify();
            }
            cx.stop_propagation();
            return;
        }
        let modifiers = paseo_desktop::keyboard::Modifiers {
            shift: key.modifiers.shift,
            alt: key.modifiers.alt,
            control: key.modifiers.control,
            platform: key.modifiers.platform,
        };
        if key.key == "enter" && self.marked.is_empty() {
            let kitty = self.buffer.kitty_input();
            if let Some(text) =
                paseo_desktop::keyboard::enhanced_enter(modifiers, kitty, self.buffer.win32_input())
            {
                self.focus.focus(window, cx);
                self.send_input(&text, cx);
                cx.stop_propagation();
                return;
            }
        }
        if !self.marked.is_empty() {
            return;
        }
        let app = self.buffer.modes().contains(TermMode::APP_CURSOR);
        let text = paseo_desktop::keyboard::encode(&key.key, modifiers, app);
        if let Some(text) = text {
            self.focus.focus(window, cx);
            self.send_input(&text, cx);
            cx.stop_propagation();
        }
    }

    /// Visible height, total content height and the current scroll offset, in
    /// logical pixels. The scrollbar and its hit testing use the same values.
    fn grid_metrics(&self) -> (f32, f32, f32) {
        let rows = f32::from(self.buffer.size.rows);
        let history = self.buffer.history_size() as f32;
        let offset = self.buffer.display_offset() as f32;
        (
            rows * self.row_height,
            (rows + history) * self.row_height,
            (history - offset) * self.row_height,
        )
    }

    fn scrollbar_slider(&self) -> Option<paseo_desktop::terminal_scrollbar::Slider> {
        let (visible, content, scroll_top) = self.grid_metrics();
        paseo_desktop::terminal_scrollbar::slider(visible, content, scroll_top)
    }

    fn scrollbar_region(&self) -> Bounds<Pixels> {
        Bounds::new(
            point(self.bounds.right() - px(SCROLLBAR_WIDTH), self.bounds.top()),
            size(px(SCROLLBAR_WIDTH), self.bounds.size.height),
        )
    }

    fn reveal_scrollbar(&mut self, cx: &mut Context<Self>) {
        self.scroll_revealed = Some(Instant::now());
        self.schedule_scroll_fade(cx);
    }

    fn schedule_scroll_fade(&mut self, cx: &mut Context<Self>) {
        if self.scroll_fade_task.is_some() || self.scroll_hovered {
            return;
        }
        let Some(delay) = paseo_desktop::terminal_scrollbar::next_repaint(
            self.scroll_revealed.map(|at| at.elapsed()),
            self.scroll_hovered,
            self.scroll_dragging.is_some(),
        ) else {
            return;
        };
        self.scroll_fade_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let _ = this.update(cx, |this, cx| {
                this.scroll_fade_task = None;
                this.notify_visible(cx);
                this.schedule_scroll_fade(cx);
            });
        }));
    }

    fn set_scroll_top(&mut self, scroll_top: f32) {
        let history = self.buffer.history_size() as f32;
        let offset = (history - (scroll_top / self.row_height).round()).clamp(0., history) as usize;
        if self.buffer.scroll_to(offset) {
            self.generation += 1;
            self.hover_cell = None;
        }
    }

    /// Whether the scrollbar takes pointer input. The reference scrollbar is
    /// invisible to the pointer while it is faded out.
    /// The reference scrollbar takes pointer input whenever it is visible, not
    /// only when the pointer already rests on it: a slider that a wheel scroll
    /// just revealed must be grabbable on the first press instead of starting a
    /// text selection in the last column.
    fn scrollbar_interactive(&self) -> bool {
        self.scrollbar_slider().is_some()
            && paseo_desktop::terminal_scrollbar::opacity(
                self.scroll_revealed.map(|at| at.elapsed()),
                self.scroll_hovered,
                self.scroll_dragging.is_some(),
            ) > 0.
    }

    /// A press inside the reference scrollbar either grabs the slider or jumps to
    /// the clicked offset and then grabs it, like the reference scroll element.
    fn scrollbar_down(&mut self, event: &gpui::MouseDownEvent, cx: &mut Context<Self>) -> bool {
        if event.button != MouseButton::Left
            || !self.scrollbar_interactive()
            || !self.scrollbar_region().contains(&event.position)
        {
            return false;
        }
        let Some(slider) = self.scrollbar_slider() else {
            return false;
        };
        let (visible, content, scroll_top) = self.grid_metrics();
        let top = f32::from(event.position.y - self.bounds.top());
        let target = if top >= slider.top && top < slider.bottom() {
            scroll_top
        } else {
            paseo_desktop::terminal_scrollbar::scroll_from_offset(top, visible, content)
        };
        self.set_scroll_top(target);
        self.scroll_dragging = Some(ScrollDrag {
            origin: event.position,
            scroll_top: target,
        });
        self.reveal_scrollbar(cx);
        cx.notify();
        true
    }

    fn scrollbar_drag(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) -> bool {
        let Some(drag) = &self.scroll_dragging else {
            return false;
        };
        let (visible, content, _) = self.grid_metrics();
        let delta = f32::from(position.y - drag.origin.y);
        let target = paseo_desktop::terminal_scrollbar::scroll_from_drag(
            delta,
            visible,
            content,
            drag.scroll_top,
        );
        self.set_scroll_top(target);
        self.notify_visible(cx);
        true
    }

    fn update_scrollbar_hover(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let hovered =
            self.scrollbar_slider().is_some() && self.scrollbar_region().contains(&position);
        if hovered != self.scroll_hovered {
            self.scroll_hovered = hovered;
            if hovered {
                self.reveal_scrollbar(cx);
            }
            self.notify_visible(cx);
        }
    }

    /// Refresh the link under the pointer. The row's links are computed once per
    /// row and buffer state, not per pointer event.
    fn update_link_hover(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        if !self.pointer_in_terminal(position) {
            self.hover_cell = None;
            if self.hover_link.take().is_some() {
                self.notify_visible(cx);
            }
            return;
        }
        let (row, column) = self.selection_position(position);
        if self.hover_cell == Some((row, column)) {
            return;
        }
        self.hover_cell = Some((row, column));
        let display_offset = self.buffer.display_offset();
        let generation = self.generation;
        let link =
            paseo_desktop::terminal_links::hyperlink_at(&self.buffer, row, column).or_else(|| {
                let stale = !matches!(
                    &self.link_cache,
                    Some(cache)
                        if cache.row == row
                            && cache.display_offset == display_offset
                            && cache.generation == generation
                );
                if stale {
                    self.link_cache = Some(LinkCache {
                        row,
                        display_offset,
                        generation,
                        links: paseo_desktop::terminal_links::urls_at(&self.buffer, row),
                    });
                }
                self.link_cache.as_ref().and_then(|cache| {
                    cache
                        .links
                        .iter()
                        .find(|link| link.contains(row, column))
                        .cloned()
                })
            });
        if link != self.hover_link {
            self.hover_link = link;
            self.notify_visible(cx);
        }
    }

    fn open_find(&mut self, cx: &mut Context<Self>) {
        if self.find.is_none() {
            self.find = Some(FindState {
                query: String::new(),
                marked: String::new(),
                matches: Vec::new(),
                limited: false,
                active: None,
            });
        }
        self.menu = None;
        self.notify_visible(cx);
    }

    fn close_find(&mut self, cx: &mut Context<Self>) {
        self.find = None;
        self.find_bar = None;
        self.find_refresh_task = None;
        self.notify_visible(cx);
    }

    fn refresh_find(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(find) = &mut self.find else {
            return;
        };
        if find.query.is_empty() {
            find.matches.clear();
            find.active = None;
            find.limited = false;
            self.notify_visible(cx);
            return;
        }
        let (matches, limited) = paseo_desktop::terminal_find::find(
            &self.buffer,
            &find.query,
            paseo_desktop::terminal_find::HIGHLIGHT_LIMIT,
        );
        find.matches = matches;
        find.limited = limited;
        find.active = paseo_desktop::terminal_find::step(&find.matches, None, forward);
        self.reveal_find(cx);
    }

    fn step_find(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(find) = &mut self.find else {
            return;
        };
        if find.matches.is_empty() {
            return;
        }
        find.active = paseo_desktop::terminal_find::step(&find.matches, find.active, forward);
        self.reveal_find(cx);
    }

    /// Bring the active match into the viewport without touching the inspected
    /// viewport while the query is unchanged.
    fn reveal_find(&mut self, cx: &mut Context<Self>) {
        let Some(active) = self
            .find
            .as_ref()
            .and_then(|find| find.active)
            .and_then(|index| self.find.as_ref()?.matches.get(index))
            .copied()
        else {
            self.notify_visible(cx);
            return;
        };
        let rows = i32::from(self.buffer.size.rows);
        let row = active.line + self.buffer.display_offset() as i32;
        if row < 0 || row >= rows {
            let offset = (-active.line).clamp(0, self.buffer.history_size() as i32) as usize;
            self.buffer.scroll_to(offset);
            self.generation += 1;
        }
        self.hover_cell = None;
        self.notify_visible(cx);
    }

    fn find_input(&mut self, text: &str, cx: &mut Context<Self>) {
        if let Some(find) = &mut self.find {
            find.query.push_str(text);
            find.marked.clear();
        }
        self.refresh_find(true, cx);
    }

    /// The text the active IME composition edits: the find query while the bar
    /// is open, otherwise the grid pre-edit.
    fn composing_text(&self) -> String {
        match &self.find {
            Some(find) => format!("{}{}", find.query, find.marked),
            None => self.marked.clone(),
        }
    }

    fn find_mark(&mut self, text: &str, cx: &mut Context<Self>) {
        if let Some(find) = &mut self.find {
            find.marked = text.to_owned();
            cx.notify();
        }
    }

    /// Re-scan after new output without moving the active match or the viewport.
    /// Matches are absolute grid lines, so a stale scan would leave the
    /// outlines on the wrong text once the buffer scrolls; keeping the index
    /// keeps the blue outline on the same match while content scrolls past it.
    fn rescan_find(&mut self, cx: &mut Context<Self>) {
        let Some(find) = &mut self.find else {
            return;
        };
        if find.query.is_empty() {
            return;
        }
        let (matches, limited) = paseo_desktop::terminal_find::find(
            &self.buffer,
            &find.query,
            paseo_desktop::terminal_find::HIGHLIGHT_LIMIT,
        );
        find.active = paseo_desktop::terminal_find::reanchored(find.active, matches.len());
        find.matches = matches;
        find.limited = limited;
        self.notify_visible(cx);
    }

    /// The reference addon re-searches on `onWriteParsed` behind a 200 ms
    /// timeout. Replacing the task cancels the previous timer, so streaming
    /// output rescans once it settles instead of on every frame.
    fn schedule_find_rescan(&mut self, cx: &mut Context<Self>) {
        if self.find.as_ref().is_none_or(|find| find.query.is_empty()) {
            return;
        }
        self.find_refresh_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(200))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.find_refresh_task = None;
                this.rescan_find(cx);
            });
        }));
    }

    fn find_backspace(&mut self, cx: &mut Context<Self>) {
        if let Some(find) = &mut self.find {
            if find.marked.is_empty() {
                find.query.pop();
            } else {
                find.marked.clear();
            }
        }
        self.refresh_find(true, cx);
    }

    fn open_menu(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        self.menu = Some(position);
        self.menu_items.clear();
        self.notify_visible(cx);
    }

    fn close_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            self.menu_items.clear();
            self.notify_visible(cx);
        }
    }

    fn menu_action(&mut self, action: MenuAction, cx: &mut Context<Self>) {
        match action {
            MenuAction::Copy => {
                if let Some(text) = self.buffer.selected_text() {
                    let text =
                        paseo_desktop::keyboard::clipboard_text(text, cfg!(target_os = "windows"));
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                }
            }
            MenuAction::Paste => {
                if let Some(text) = cx
                    .read_from_clipboard()
                    .and_then(|item| item.text())
                    .filter(|text| !text.is_empty())
                {
                    let text = paseo_desktop::keyboard::paste(
                        &text,
                        self.buffer.modes().contains(TermMode::BRACKETED_PASTE),
                    );
                    self.send_input(&text, cx);
                }
            }
            MenuAction::SelectAll => self.buffer.select_all(),
        }
        self.close_menu(cx);
    }

    /// Outline every match with the addon's one-pixel decoration border: yellow
    /// for matches, blue for the active one.
    fn paint_find_matches(
        &self,
        window: &mut Window,
        bounds: Bounds<Pixels>,
        width: Pixels,
        row_height: f32,
        palette: paseo_desktop::appearance::TerminalPalette,
    ) {
        let Some(find) = &self.find else {
            return;
        };
        for (index, hit) in find.matches.iter().enumerate() {
            let color = if find.active == Some(index) {
                palette.ansi[4]
            } else {
                palette.ansi[3]
            };
            for segment in hit.segments(
                usize::from(self.buffer.size.cols),
                usize::from(self.buffer.size.rows),
                self.buffer.display_offset(),
            ) {
                let origin = bounds.origin
                    + point(
                        width * segment.column as f32,
                        px(row_height * segment.line as f32),
                    );
                let target =
                    Bounds::new(origin, size(width * segment.columns as f32, px(row_height)));
                for edge in border_quads(target) {
                    window.paint_quad(fill(edge, rgb(color)));
                }
            }
        }
    }

    fn paint_find_bar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.find_bar = None;
        let Some(find) = &self.find else {
            return;
        };
        let bar = Bounds::new(
            point(
                (self.bounds.right() - px(SCROLLBAR_WIDTH) - px(256.))
                    .max(self.bounds.left() + px(4.)),
                self.bounds.top() + px(8.),
            ),
            size(px(248.), px(30.)),
        );
        window.paint_quad(
            fill(bar, rgb(crate::theme::colors::POPOVER))
                .corner_radii(px(crate::theme::radius::MD))
                .border_widths(px(1.))
                .border_color(rgb(crate::theme::colors::BORDER)),
        );
        let button = px(24.);
        let close = Bounds::new(
            point(
                bar.origin.x + bar.size.width - px(4.) - button,
                bar.origin.y + px(3.),
            ),
            size(button, button),
        );
        let next = Bounds::new(
            point(close.origin.x - button, close.origin.y),
            size(button, button),
        );
        let previous = Bounds::new(
            point(next.origin.x - button, next.origin.y),
            size(button, button),
        );
        let text_origin = bar.origin + point(px(10.), px(6.));
        let (label, color) = if find.query.is_empty() && find.marked.is_empty() {
            (
                "Find".to_string(),
                crate::theme::colors::FOREGROUND_EXTRA_MUTED,
            )
        } else {
            (
                format!("{}{}", find.query, find.marked),
                crate::theme::colors::FOREGROUND,
            )
        };
        paint_label(window, cx, &label, text_origin, 13., color);
        if !find.marked.is_empty() {
            let start = if find.query.is_empty() {
                text_origin.x
            } else {
                text_origin.x + label_width(window, &find.query, 13.)
            };
            let end = text_origin.x + label_width(window, &label, 13.);
            window.paint_quad(fill(
                Bounds::new(
                    point(start, text_origin.y + px(16.)),
                    size(end - start, px(1.)),
                ),
                rgb(crate::theme::colors::FOREGROUND),
            ));
        }
        if !find.query.is_empty() {
            let total = if find.limited {
                format!("{}+", find.matches.len())
            } else {
                find.matches.len().to_string()
            };
            let status = match find.active {
                Some(index) => format!("{}/{total}", index + 1),
                None => format!("0/{total}"),
            };
            let width = label_width(window, &status, 13.);
            paint_label(
                window,
                cx,
                &status,
                point(previous.origin.x - width - px(6.), text_origin.y),
                13.,
                crate::theme::colors::FOREGROUND_MUTED,
            );
        }
        for (bounds, glyph) in [(previous, "<"), (next, ">"), (close, "x")] {
            paint_label(
                window,
                cx,
                glyph,
                bounds.origin + point(px(8.), px(4.)),
                13.,
                crate::theme::colors::FOREGROUND_MUTED,
            );
        }
        self.find_bar = Some(FindBar {
            bar,
            previous,
            next,
            close,
        });
    }

    fn paint_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.menu_items.clear();
        let Some(origin) = self.menu else {
            return;
        };
        let scrolled = Bounds::new(
            self.bounds.origin,
            size(
                self.bounds.size.width - px(SCROLLBAR_WIDTH),
                self.bounds.size.height,
            ),
        );
        let width = px(176.);
        let item = px(28.);
        let padding = px(4.);
        let separator = px(9.);
        let height = padding * 2. + item * 3. + separator;
        let x = origin.x.min(scrolled.right() - width).max(scrolled.left());
        let y = origin.y.min(scrolled.bottom() - height).max(scrolled.top());
        let surface = Bounds::new(point(x, y), size(width, height));
        window.paint_quad(
            fill(surface, rgb(crate::theme::colors::POPOVER))
                .corner_radii(px(crate::theme::radius::MD))
                .border_widths(px(1.))
                .border_color(rgb(crate::theme::colors::BORDER)),
        );
        let has_selection = self.buffer.has_selection();
        let rows = [
            (MenuAction::Copy, "Copy", has_selection, padding),
            (MenuAction::Paste, "Paste", true, padding + item),
            (
                MenuAction::SelectAll,
                "Select All",
                true,
                padding + item * 2. + separator,
            ),
        ];
        let divider = Bounds::new(
            point(x + px(8.), y + padding + item * 2. + separator / 2.),
            size(width - px(16.), px(1.)),
        );
        window.paint_quad(fill(divider, rgb(crate::theme::colors::BORDER)));
        for (action, label, enabled, top) in rows {
            let bounds = Bounds::new(point(x, y + top), size(width, item));
            let color = if enabled {
                crate::theme::colors::FOREGROUND
            } else {
                crate::theme::colors::FOREGROUND_EXTRA_MUTED
            };
            paint_label(
                window,
                cx,
                label,
                bounds.origin + point(px(10.), px(6.)),
                13.,
                color,
            );
            if enabled {
                self.menu_items.push((action, bounds));
            }
        }
    }

    fn selection_position(&self, position: Point<Pixels>) -> (usize, usize) {
        let row = (((position.y - self.bounds.top()) / px(self.row_height)).ceil() - 1.)
            .clamp(0., f32::from(self.buffer.size.rows - 1)) as usize;
        let column = (((position.x - self.bounds.left()) / self.cell_width + 0.5).ceil() - 1.)
            .clamp(0., f32::from(self.buffer.size.cols)) as usize;
        (row, column)
    }

    fn pointer_button(button: MouseButton) -> Option<(PointerButton, u8)> {
        match button {
            MouseButton::Left => Some((PointerButton::Left, 1)),
            MouseButton::Middle => Some((PointerButton::Middle, 2)),
            MouseButton::Right => Some((PointerButton::Right, 4)),
            _ => None,
        }
    }

    fn pointer_in_terminal(&self, position: Point<Pixels>) -> bool {
        // xterm clamps empty horizontal-gutter clicks to the final column, but
        // rejects the unused space below the final screen row.
        position.x >= self.bounds.left()
            && position.y >= self.bounds.top()
            && position.x < self.bounds.right()
            && position.y
                < self.bounds.top() + px(self.row_height * f32::from(self.buffer.size.rows))
    }

    fn send_pointer(
        &mut self,
        position: Point<Pixels>,
        button: PointerButton,
        action: MouseAction,
        modifiers: gpui::Modifiers,
        repeat: usize,
        cx: &mut Context<Self>,
    ) {
        if !self.visible || !self.replay_ready || self.buffer.replaying {
            return;
        }
        let size = self.buffer.size;
        let x = f32::from(position.x - self.bounds.left()).clamp(
            0.,
            (f32::from(self.cell_width) * f32::from(size.cols) - 1.).max(0.),
        );
        let y = f32::from(position.y - self.bounds.top())
            .clamp(0., (self.row_height * f32::from(size.rows) - 1.).max(0.));
        let event = PointerEvent {
            col: (x / f32::from(self.cell_width)).floor() as u32,
            row: (y / self.row_height).floor() as u32,
            x: x.floor() as u32,
            y: y.floor() as u32,
            button,
            action,
            shift: modifiers.shift,
            alt: modifiers.alt,
            control: modifiers.control,
        };
        if let Some(bytes) = self
            .mouse_reporter
            .report(self.buffer.mouse_modes(), event, size)
        {
            // The established host input lane is UTF-8. Never corrupt a legacy
            // high-bit mouse byte by converting it to a Unicode replacement.
            match std::str::from_utf8(&bytes) {
                Ok(text) => {
                    if self.mouse_error {
                        self.mouse_error = false;
                        self.error = None;
                        self.notify_visible(cx);
                    }
                    if !text.is_empty() {
                        self.send_input(&text.repeat(repeat.min(1000)), cx);
                    }
                }
                Err(_) => {
                    if self.raw_input {
                        let result = self.send_raw(&bytes, repeat, cx);
                        self.finish(result, cx);
                    } else {
                        self.error = Some("Legacy mouse reports at this position require raw-byte terminal input. This terminal's worker or platform does not support it; use SGR mouse mode in the terminal application.".into());
                        self.mouse_error = true;
                        self.notify_visible(cx);
                    }
                }
            }
        }
    }

    fn mouse_down(
        &mut self,
        event: &gpui::MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.menu.is_some() {
            let hit = self
                .menu_items
                .iter()
                .find(|(_, bounds)| bounds.contains(&event.position))
                .map(|(action, _)| *action);
            match hit {
                Some(action) => self.menu_action(action, cx),
                None => self.close_menu(cx),
            }
            cx.stop_propagation();
            return;
        }
        if let Some(bar) = self
            .find_bar
            .as_ref()
            .filter(|bar| bar.bar.contains(&event.position))
        {
            let close = bar.close.contains(&event.position);
            let previous = bar.previous.contains(&event.position);
            let next = bar.next.contains(&event.position);
            if close {
                self.close_find(cx);
            } else if previous {
                self.step_find(false, cx);
            } else if next {
                self.step_find(true, cx);
            }
            cx.stop_propagation();
            return;
        }
        if !self.pointer_in_terminal(event.position) {
            return;
        }
        if self.scrollbar_down(event, cx) {
            cx.stop_propagation();
            return;
        }
        if event.button == MouseButton::Right {
            // The reference application always shows the terminal menu on a right
            // click, and still reports the button to programs that enabled mouse
            // mode.
            self.open_menu(event.position, cx);
        }
        self.focus.focus(window, cx);
        self.blink.restart(Instant::now());
        self.sync_cursor(window, cx);
        self.claim(cx);
        let force_selection = !cfg!(target_os = "macos") && event.modifiers.shift;
        if self.buffer.mouse_modes().active() && !force_selection {
            if self.replay_ready
                && !self.buffer.replaying
                && let Some((button, bit)) = Self::pointer_button(event.button)
            {
                self.mouse_buttons |= bit;
                self.send_pointer(
                    event.position,
                    button,
                    MouseAction::Press,
                    event.modifiers,
                    1,
                    cx,
                );
            }
            cx.stop_propagation();
            return;
        }
        if event.button == MouseButton::Middle {
            #[cfg(any(target_os = "linux", target_os = "freebsd"))]
            if let Some(text) = cx
                .read_from_primary()
                .and_then(|item| item.text())
                .filter(|text| !text.is_empty())
            {
                let text = paseo_desktop::keyboard::paste(
                    &text,
                    self.buffer.modes().contains(TermMode::BRACKETED_PASTE),
                );
                self.send_input(&text, cx);
                cx.stop_propagation();
            }
            return;
        }
        if event.button != MouseButton::Left {
            return;
        }
        self.pressed_link = if event.click_count == 1 {
            self.hover_link.clone()
        } else {
            None
        };
        self.pressed_link_at = Some(event.position);
        self.link_dragged = false;
        let (row, column) = self.selection_position(event.position);
        if event.modifiers.shift && self.buffer.has_selection() {
            self.buffer.extend_selection(row, column);
        } else if event.click_count == 2 {
            self.buffer.begin_word_selection(row, column);
        } else if event.click_count == 3 {
            self.buffer.begin_line_selection(row, column);
        } else {
            self.buffer
                .begin_selection(row, column, event.modifiers.alt);
        }
        self.selecting = true;
        self.selection_pointer = event.position;
        self.start_selection_scroll(cx);
        cx.notify();
        cx.stop_propagation();
    }

    fn start_selection_scroll(&mut self, cx: &mut Context<Self>) {
        if self.selection_scroll_task.is_some() {
            return;
        }
        self.selection_scroll_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(50))
                    .await;
                let running = this.update(cx, |this, cx| {
                    if !this.selecting || !this.visible || !this.focused || this.buffer.replaying {
                        this.selection_scroll_task = None;
                        return false;
                    }
                    let top = f32::from(this.bounds.top());
                    let bottom = top + this.row_height * f32::from(this.buffer.size.rows);
                    let delta = paseo_desktop::terminal_geometry::selection_scroll(
                        f32::from(this.selection_pointer.y),
                        top,
                        bottom,
                    );
                    if delta != 0 {
                        this.link_dragged = true;
                        this.buffer.scroll(delta);
                        let (row, mut column) = this.selection_position(this.selection_pointer);
                        if !this.buffer.selection_is_block() {
                            if delta > 0 {
                                column = 0;
                            } else {
                                column = usize::from(this.buffer.size.cols);
                            }
                        }
                        this.buffer.extend_selection(row, column);
                        this.notify_visible(cx);
                    }
                    true
                });
                if !matches!(running, Ok(true)) {
                    break;
                }
            }
        }));
    }

    fn mouse_move(&mut self, event: &gpui::MouseMoveEvent, cx: &mut Context<Self>) {
        if self.scroll_dragging.is_some() {
            if event.pressed_button.is_none() {
                self.scroll_dragging = None;
                self.reveal_scrollbar(cx);
            } else if self.scrollbar_drag(event.position, cx) {
                cx.stop_propagation();
                return;
            }
        }
        self.update_scrollbar_hover(event.position, cx);
        self.update_link_hover(event.position, cx);
        if self.selecting && event.dragging() {
            if let Some(press) = self.pressed_link_at
                && ((event.position.x - press.x).abs() > self.cell_width / 2.
                    || (event.position.y - press.y).abs() > px(self.row_height / 2.))
            {
                self.link_dragged = true;
            }
            self.selection_pointer = event.position;
            let (row, column) = self.selection_position(event.position);
            self.buffer.extend_selection(row, column);
            cx.notify();
            cx.stop_propagation();
            return;
        }
        if !self.buffer.mouse_modes().active() {
            return;
        }
        let button = if self.mouse_buttons != 0 {
            if event.pressed_button.is_none() {
                self.mouse_buttons = 0;
                return;
            }
            if self.mouse_buttons & 1 != 0 {
                PointerButton::Left
            } else if self.mouse_buttons & 2 != 0 {
                PointerButton::Middle
            } else {
                PointerButton::Right
            }
        } else {
            if event.pressed_button.is_some() || !self.pointer_in_terminal(event.position) {
                return;
            }
            PointerButton::None
        };
        self.send_pointer(
            event.position,
            button,
            MouseAction::Move,
            event.modifiers,
            1,
            cx,
        );
    }

    fn mouse_up(&mut self, event: &gpui::MouseUpEvent, cx: &mut Context<Self>) {
        if self.scroll_dragging.take().is_some() {
            self.reveal_scrollbar(cx);
            cx.notify();
            return;
        }
        if event.button == MouseButton::Left && self.selecting {
            let (row, column) = self.selection_position(event.position);
            if !self.buffer.selection_is_gesture() {
                self.buffer.extend_selection(row, column);
            }
            self.selecting = false;
            self.selection_scroll_task = None;
            #[cfg(any(target_os = "linux", target_os = "freebsd"))]
            if let Some(text) = self.buffer.selected_text().filter(|text| !text.is_empty()) {
                cx.write_to_primary(gpui::ClipboardItem::new_string(
                    paseo_desktop::keyboard::clipboard_text(text, false),
                ));
            }
            cx.notify();
        }
        let pressed = self.pressed_link.take();
        let pressed_at = self.pressed_link_at.take();
        // A link opens on a plain single click that neither dragged nor became a
        // word/line selection, so those gestures stay usable on top of links.
        if event.button == MouseButton::Left
            && pressed_at.is_some()
            && pressed.is_some()
            && pressed == self.hover_link
            && !self.buffer.mouse_modes().active()
            && paseo_desktop::terminal_links::activates(
                event.click_count,
                self.link_dragged,
                self.buffer.selection_is_gesture(),
                true,
            )
            && let Some(link) = &self.hover_link
        {
            cx.open_url(&link.text);
            cx.stop_propagation();
            return;
        }
        if let Some((button, bit)) = Self::pointer_button(event.button)
            && self.mouse_buttons & bit != 0
        {
            self.mouse_buttons &= !bit;
            self.send_pointer(
                event.position,
                button,
                MouseAction::Release,
                event.modifiers,
                1,
                cx,
            );
        }
    }

    fn scroll(&mut self, event: &ScrollEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let delta = f32::from(event.delta.pixel_delta(px(self.row_height)).y) / self.row_height;
        if delta == 0. {
            return;
        }
        self.close_menu(cx);
        let modes = self.buffer.modes();
        let pointer = self.buffer.mouse_modes();
        let application =
            pointer.active() && pointer.protocol != 9 || modes.contains(TermMode::ALT_SCREEN);
        let route = (pointer.protocol, modes.contains(TermMode::ALT_SCREEN));
        let sensitivity = if application
            && !event.modifiers.shift
            && (event.modifiers.alt || event.modifiers.control)
        {
            5.
        } else {
            1.
        };
        let lines = self.wheel.rows(delta, sensitivity, Instant::now(), route);
        if lines == 0 {
            return;
        }
        if pointer.active() && pointer.protocol != 9 {
            self.claim(cx);
            let button = if lines > 0 {
                PointerButton::WheelUp
            } else {
                PointerButton::WheelDown
            };
            self.send_pointer(
                event.position,
                button,
                MouseAction::Press,
                event.modifiers,
                lines.unsigned_abs() as usize,
                cx,
            );
        } else if modes.contains(TermMode::ALT_SCREEN) {
            self.claim(cx);
            let key = if lines > 0 { "up" } else { "down" };
            if let Some(text) = paseo_desktop::keyboard::encode(
                key,
                paseo_desktop::keyboard::Modifiers::default(),
                modes.contains(TermMode::APP_CURSOR),
            ) {
                self.send_input(&text.repeat(lines.unsigned_abs() as usize), cx);
            }
        } else {
            self.buffer.scroll(lines);
            self.generation += 1;
            self.hover_cell = None;
            self.reveal_scrollbar(cx);
            cx.notify();
        }
        cx.stop_propagation();
    }
}

impl Drop for TerminalView {
    fn drop(&mut self) {
        self.connection
            .unsubscribe(&format!("terminal:{}", self.terminal_id));
    }
}

/// One-pixel border made of four quads, matching the search decoration outline.
fn border_quads(bounds: Bounds<Pixels>) -> [Bounds<Pixels>; 4] {
    let one = px(1.);
    [
        Bounds::new(bounds.origin, size(bounds.size.width, one)),
        Bounds::new(
            point(bounds.origin.x, bounds.bottom() - one),
            size(bounds.size.width, one),
        ),
        Bounds::new(bounds.origin, size(one, bounds.size.height)),
        Bounds::new(
            point(bounds.right() - one, bounds.origin.y),
            size(one, bounds.size.height),
        ),
    ]
}

fn label_width(window: &mut Window, text: &str, size: f32) -> Pixels {
    let run = TextRun {
        len: text.len(),
        font: gpui::font(crate::fonts::ui_family()),
        color: rgb(0).into(),
        ..Default::default()
    };
    window
        .text_system()
        .shape_line(text.to_owned().into(), px(size), &[run], None)
        .width()
}

fn paint_label(
    window: &mut Window,
    cx: &mut App,
    text: &str,
    origin: Point<Pixels>,
    size: f32,
    color: u32,
) {
    if text.is_empty() {
        return;
    }
    let run = TextRun {
        len: text.len(),
        font: gpui::font(crate::fonts::ui_family()),
        color: rgb(color).into(),
        ..Default::default()
    };
    let line = window
        .text_system()
        .shape_line(text.to_owned().into(), px(size), &[run], None);
    let _ = line.paint(origin, px(size), gpui::TextAlign::Left, None, window, cx);
}

/// Paint one cell's underline or strikethrough stripes. The geometry is in device
/// pixels so a one-pixel stroke stays one device row at any scale.
fn paint_stripes(
    window: &mut Window,
    origin: Point<Pixels>,
    scale: f32,
    stripes: &paseo_desktop::terminal_decorations::Stripes,
    color: u32,
    opacity: u8,
) {
    for stripe in stripes.iter() {
        window.paint_quad(fill(
            Bounds::new(
                origin + point(px(stripe.x / scale), px(stripe.y / scale)),
                size(px(stripe.width / scale), px(stripe.height / scale)),
            ),
            rgba((color << 8) | u32::from(opacity)),
        ));
    }
}

fn paint_curly(
    window: &mut Window,
    origin: Point<Pixels>,
    scale: f32,
    polygons: [[[f32; 2]; 4]; 3],
    color: u32,
    opacity: u8,
) {
    let color = rgba((color << 8) | u32::from(opacity));
    for polygon in polygons {
        let mut path = PathBuilder::fill();
        let mut points = polygon.into_iter();
        let first = points.next().expect("curly underline has four points");
        path.move_to(origin + point(px(first[0] / scale), px(first[1] / scale)));
        for [x, y] in points {
            path.line_to(origin + point(px(x / scale), px(y / scale)));
        }
        path.close();
        if let Ok(path) = path.build() {
            window.paint_path(path, color);
        }
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.focused = self.focus.is_focused(window) && window.is_window_active();
        let settings = self.shared.get();
        let palette =
            paseo_desktop::appearance::resolve(settings.theme, system_dark(window)).terminal;
        self.buffer.set_palette(palette);
        self.sync_history();
        let font = crate::fonts::terminal_font_for(&settings.mono_font_family);
        let font_size = settings.code_font_size as f32;
        let screen = self.buffer.screen();
        self.blink.enable(
            self.visible && self.focused && screen.cursor.is_some() && screen.cursor_style.blinking,
            Instant::now(),
        );
        self.schedule_blink(cx);
        let measure_view = cx.entity();
        let paint_view = cx.entity();
        div()
            .size_full()
            .flex()
            .flex_col()
            .min_h_0()
            .min_w_0()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key_down))
            .on_scroll(cx.listener(Self::scroll))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::mouse_down))
            .when_some(self.error.clone(), |view, error| {
                view.child(div().px_3().py_1().text_color(rgb(0xf07178)).child(error))
            })
            .child(
                canvas(
                    move |bounds, window, cx| {
                        let run = TextRun {
                            len: 1,
                            font: font.clone(),
                            color: rgb(palette.foreground).into(),
                            ..Default::default()
                        };
                        let line = window.text_system().shape_line(
                            "W".into(),
                            px(font_size),
                            &[run],
                            None,
                        );
                        let metrics = paseo_desktop::terminal_geometry::Metrics::new(
                            line.width().into(),
                            line.ascent.into(),
                            line.descent.into(),
                            window.scale_factor(),
                        );
                        measure_view.update(cx, |view, cx| view.measure(bounds, metrics, cx));
                        (font, px(metrics.width))
                    },
                    move |bounds, (base_font, width), window, cx| {
                        let focus = paint_view.read(cx).focus.clone();
                        window.handle_input(
                            &focus,
                            ElementInputHandler::new(bounds, paint_view.clone()),
                            cx,
                        );
                        window.on_mouse_event({
                            let view = paint_view.downgrade();
                            move |event: &gpui::MouseMoveEvent, phase, _, cx| {
                                if phase.bubble()
                                    && let Some(view) = view.upgrade()
                                {
                                    view.update(cx, |this, cx| this.mouse_move(event, cx));
                                }
                            }
                        });
                        window.on_mouse_event({
                            let view = paint_view.downgrade();
                            move |event: &gpui::MouseUpEvent, phase, _, cx| {
                                if phase.bubble()
                                    && let Some(view) = view.upgrade()
                                {
                                    view.update(cx, |this, cx| this.mouse_up(event, cx));
                                }
                            }
                        });
                        paint_view.update(cx, |view, cx| {
                            view.glyphs.prepare(
                                base_font.clone(),
                                px(font_size),
                                palette.cursor_accent,
                            );
                            let scale = window.scale_factor();
                            let screen = view.buffer.screen();
                            let cursor_shape = if view.focused {
                                screen.cursor_style.shape
                            } else {
                                CursorShape::HollowBlock
                            };
                            let painted_cursor = screen.cursor.filter(|_| view.blink.visible());
                            let cursor_color = screen.cursor_color;
                            let cursor_columns = screen.cursor_columns;
                            let text_cursor = screen.cursor;
                            let row_height = view.row_height;
                            window.with_content_mask(
                                Some(gpui::ContentMask { bounds }),
                                |window| {
                                    // Backgrounds first, then glyphs and decorations. A row's
                                    // background must not cover a neighbouring row's ink, and
                                    // the same order lets a curly underline hang past its cell
                                    // like the reference renderer.
                                    for cell in screen.cells {
                                        let block_cursor = cursor_shape == CursorShape::Block
                                            && painted_cursor == Some((cell.row, cell.col));
                                        let background = if block_cursor {
                                            cursor_color
                                        } else {
                                            cell.background
                                        };
                                        if background != palette.background {
                                            window.paint_quad(fill(
                                                Bounds::new(
                                                    bounds.origin
                                                        + point(
                                                            width * cell.col as f32,
                                                            px(row_height * cell.row as f32),
                                                        ),
                                                    size(
                                                        width * cell.columns as f32,
                                                        px(row_height),
                                                    ),
                                                ),
                                                rgb(background),
                                            ));
                                        }
                                    }
                                    let screen = view.buffer.screen();
                                    for cell in screen.cells {
                                        let block_cursor = cursor_shape == CursorShape::Block
                                            && painted_cursor == Some((cell.row, cell.col));
                                        // Selection colors are resolved on the authoritative cell,
                                        // including the original explicit-background/inverse modes.
                                        let empty =
                                            cell.character == ' ' && cell.zerowidth.is_empty();
                                        let decorated =
                                            cell.underline.is_some() || cell.strikethrough;
                                        if cell.hidden && !block_cursor {
                                            continue;
                                        }
                                        let origin = bounds.origin
                                            + point(
                                                width * cell.col as f32,
                                                px(row_height * cell.row as f32),
                                            );
                                        if decorated && !block_cursor {
                                            let metrics = paseo_desktop::terminal_decorations::Metrics {
                                                width: f32::from(width)
                                                    * cell.columns as f32
                                                    * scale,
                                                height: row_height * scale,
                                                scale,
                                                font_size,
                                            };
                                            if cell.underline.is_some() {
                                                let color =
                                                    cell.underline_color.unwrap_or(cell.foreground);
                                                match cell.underline {
                                                    paseo_desktop::terminal::Underline::Curly => {
                                                        paint_curly(
                                                            window,
                                                            origin,
                                                            scale,
                                                            paseo_desktop::terminal_decorations::curly_polygons(metrics),
                                                            color,
                                                            cell.opacity,
                                                        );
                                                    }
                                                    style => paint_stripes(
                                                        window,
                                                        origin,
                                                        scale,
                                                        &paseo_desktop::terminal_decorations::underline_stripes(style, metrics),
                                                        color,
                                                        cell.opacity,
                                                    ),
                                                }
                                            }
                                            if cell.strikethrough {
                                                paint_stripes(
                                                    window,
                                                    origin,
                                                    scale,
                                                    &paseo_desktop::terminal_decorations::strikethrough_stripes(metrics),
                                                    cell.foreground,
                                                    cell.opacity,
                                                );
                                            }
                                        }
                                        if empty && (!decorated || block_cursor) {
                                            continue;
                                        }
                                        let line = if block_cursor {
                                            view.glyphs.line_with_cursor(
                                                &cell,
                                                window.text_system(),
                                                true,
                                            )
                                        } else {
                                            view.glyphs.line(&cell, window.text_system())
                                        };
                                        let _ = line.paint(
                                            origin,
                                            px(row_height),
                                            gpui::TextAlign::Left,
                                            None,
                                            window,
                                            cx,
                                        );
                                    }
                                    if cursor_shape != CursorShape::Block
                                        && let Some((row, col)) = painted_cursor
                                    {
                                        let metrics = paseo_desktop::terminal_geometry::Metrics {
                                            width: f32::from(width),
                                            height: row_height,
                                        };
                                        for rect in paseo_desktop::terminal_geometry::cursor_rects(
                                            metrics,
                                            cursor_columns,
                                            cursor_shape,
                                        )
                                        .into_iter()
                                        .flatten()
                                        {
                                            window.paint_quad(fill(
                                                Bounds::new(
                                                    bounds.origin
                                                        + point(
                                                            width * col as f32 + px(rect.x),
                                                            px(row_height * row as f32 + rect.y),
                                                        ),
                                                    size(px(rect.width), px(rect.height)),
                                                ),
                                                rgb(cursor_color),
                                            ));
                                        }
                                    }
                                    let marked = view.marked.clone();
                                    if !marked.is_empty() {
                                        let (row, col) = text_cursor.unwrap_or((0, 0));
                                        let run = TextRun {
                                            len: marked.len(),
                                            font: base_font,
                                            color: rgb(0xffcb6b).into(),
                                            background_color: None,
                                            underline: Some(gpui::UnderlineStyle {
                                                thickness: px(1.),
                                                color: None,
                                                wavy: false,
                                            }),
                                            strikethrough: None,
                                        };
                                        let line = window.text_system().shape_line(
                                            marked.into(),
                                            px(font_size),
                                            &[run],
                                            None,
                                        );
                                        let _ = line.paint(
                                            bounds.origin
                                                + point(
                                                    width * col as f32,
                                                    px(row_height * row as f32),
                                                ),
                                            px(row_height),
                                            gpui::TextAlign::Left,
                                            None,
                                            window,
                                            cx,
                                        );
                                    }
                                },
                            );
                            if let Some(slider) = view.scrollbar_slider() {
                                let reveal = view.scroll_revealed.map(|at| at.elapsed());
                                let dragging = view.scroll_dragging.is_some();
                                let opacity = paseo_desktop::terminal_scrollbar::opacity(
                                    reveal,
                                    view.scroll_hovered,
                                    dragging,
                                );
                                if opacity > 0. {
                                    let base = if dragging {
                                        paseo_desktop::terminal_scrollbar::ACTIVE_ALPHA
                                    } else if view.scroll_hovered {
                                        paseo_desktop::terminal_scrollbar::HOVER_ALPHA
                                    } else {
                                        paseo_desktop::terminal_scrollbar::REST_ALPHA
                                    };
                                    let alpha = (f32::from(base) * opacity).round() as u32;
                                    let track = view.scrollbar_region();
                                    let track_height = f32::from(track.size.height);
                                    let height = slider.height.min(track_height);
                                    let top = slider.top.clamp(0., (track_height - height).max(0.));
                                    window.paint_quad(fill(
                                        Bounds::new(
                                            point(track.origin.x, track.origin.y + px(top)),
                                            size(px(SCROLLBAR_WIDTH), px(height)),
                                        ),
                                        rgba((palette.foreground << 8) | alpha),
                                    ));
                                }
                            }
                            if view.hover_link.is_some() {
                                let hitbox = window
                                    .insert_hitbox(bounds, gpui::HitboxBehavior::Normal);
                                window.set_cursor_style(gpui::CursorStyle::PointingHand, &hitbox);
                            }
                            view.paint_find_matches(window, bounds, width, row_height, palette);
                            view.paint_find_bar(window, cx);
                            view.paint_menu(window, cx);
                        });
                    },
                )
                .flex_1()
                .w_full(),
            )
    }
}

impl EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let units: Vec<u16> = self
            .composing_text()
            .encode_utf16()
            .skip(range.start)
            .take(range.end.saturating_sub(range.start))
            .collect();
        *actual = Some(range);
        Some(String::from_utf16_lossy(&units))
    }
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let end = self.composing_text().encode_utf16().count();
        Some(UTF16Selection {
            range: end..end,
            reversed: false,
        })
    }
    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        if self.find.is_some() {
            return None;
        }
        (!self.marked.is_empty()).then(|| 0..self.marked.encode_utf16().count())
    }
    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.marked.clear();
        if let Some(find) = &mut self.find {
            find.marked.clear();
        }
        cx.notify();
    }
    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked.clear();
        if self.find.is_some() {
            self.find_input(text, cx);
        } else {
            self.send_input(text, cx);
        }
        cx.notify();
    }
    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.find.is_some() {
            self.find_mark(text, cx);
        } else {
            self.marked = text.into();
            cx.notify();
        }
    }
    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        if self.find.is_some() {
            let bar = self.find_bar.as_ref()?.bar;
            return Some(Bounds::new(
                bar.origin + point(px(10.), px(6.)),
                size(bar.size.width - px(100.), px(18.)),
            ));
        }
        let (row, col) = self.buffer.screen().cursor.unwrap_or((0, 0));
        Some(Bounds::new(
            self.bounds.origin
                + point(
                    self.cell_width * col as f32,
                    px(self.row_height * row as f32),
                ),
            size(self.cell_width, px(self.row_height)),
        ))
    }
    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.composing_text().encode_utf16().count())
    }
}

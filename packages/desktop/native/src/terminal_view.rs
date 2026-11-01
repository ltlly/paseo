use std::{ops::Range, sync::Arc};

use alacritty_terminal::term::TermMode;
use anyhow::{Result, ensure};
use gpui::{
    App, Bounds, Context, ElementInputHandler, EntityInputHandler, FocusHandle, Focusable,
    KeyDownEvent, MouseButton, Pixels, Point, Render, ScrollEvent, TextRun, UTF16Selection, Window,
    canvas, div, fill, font, point, prelude::*, px, rgb, rgba, size,
};
use serde_json::{Value, json};

use paseo_desktop::{
    client::Connection,
    terminal::{GridSize, TerminalBuffer},
    wire::{TerminalFrame, TerminalOpcode},
};

const FONT_SIZE: f32 = 14.;
const ROW_HEIGHT: f32 = 20.;

pub struct TerminalView {
    terminal_id: String,
    connection: Arc<Connection>,
    focus: FocusHandle,
    buffer: TerminalBuffer,
    slot: Option<u8>,
    request_id: Option<String>,
    replay_ready: bool,
    pending_input: Vec<u8>,
    desired_size: GridSize,
    bounds: Bounds<Pixels>,
    cell_width: Pixels,
    wheel_remainder: f32,
    marked: String,
    error: Option<String>,
    visible: bool,
    opening: bool,
    needs_claim: bool,
    selecting: bool,
}

impl TerminalView {
    pub fn new(
        terminal_id: String,
        connection: Arc<Connection>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let initial = GridSize { rows: 24, cols: 80 };
        Self {
            terminal_id,
            connection,
            focus,
            buffer: TerminalBuffer::new(initial, 10_000),
            slot: None,
            request_id: None,
            replay_ready: false,
            pending_input: Vec::new(),
            desired_size: initial,
            bounds: Bounds::default(),
            cell_width: px(8.),
            wheel_remainder: 0.,
            marked: String::new(),
            error: None,
            visible: true,
            opening: false,
            needs_claim: true,
            selecting: false,
        }
    }

    // Slot binding can still be in flight when a workspace is left. The daemon's
    // ID-addressed input lane preserves the already-committed bytes before unsubscribe.
    pub fn prepare_detach(&mut self) -> Result<()> {
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
        !self.opening && self.pending_input.is_empty()
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
            }
            if visible {
                cx.notify();
            }
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
            self.slot = payload["slot"]
                .as_u64()
                .and_then(|slot| u8::try_from(slot).ok());
        }
        self.notify_visible(cx);
        true
    }

    pub fn receive(&mut self, frame: TerminalFrame, cx: &mut Context<Self>) {
        if self.slot != Some(frame.slot) {
            return;
        }
        let completes_replay = matches!(
            frame.opcode,
            TerminalOpcode::ReplayEnd | TerminalOpcode::Restore
        );
        let result = self.buffer.apply(frame).and_then(|()| {
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
        self.notify_visible(cx);
    }

    fn measure(&mut self, bounds: Bounds<Pixels>, width: Pixels, cx: &mut Context<Self>) {
        self.bounds = bounds;
        self.cell_width = width;
        let rows = (f32::from(bounds.size.height) / ROW_HEIGHT)
            .floor()
            .clamp(1., 1000.) as u16;
        let cols = (bounds.size.width / width).floor().clamp(2., 1000.) as u16;
        self.desired_size = GridSize { rows, cols };
        if self.request_id.is_none() {
            self.buffer.resize(self.desired_size);
            let result = self.connection.subscribe(&format!("terminal:{}", self.terminal_id), "subscribe_terminal_request", json!({"terminalId":self.terminal_id,"restore":{"mode":"full-snapshot","scrollbackLines":10_000,"size":{"rows":rows,"cols":cols}}}));
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
        let app = self.buffer.modes().contains(TermMode::APP_CURSOR);
        let text = paseo_desktop::keyboard::encode(
            &key.key,
            paseo_desktop::keyboard::Modifiers {
                shift: key.modifiers.shift,
                alt: key.modifiers.alt,
                control: key.modifiers.control,
                platform: key.modifiers.platform,
            },
            app,
        );
        if let Some(text) = text {
            self.focus.focus(window, cx);
            self.send_input(&text, cx);
            cx.stop_propagation();
        }
    }

    fn selection_position(&self, position: Point<Pixels>) -> (usize, usize) {
        let row = ((position.y - self.bounds.top()) / px(ROW_HEIGHT))
            .floor()
            .clamp(0., f32::from(self.buffer.size.rows - 1)) as usize;
        let column = ((position.x - self.bounds.left()) / self.cell_width)
            .round()
            .clamp(0., f32::from(self.buffer.size.cols)) as usize;
        (row, column)
    }

    fn begin_selection(
        &mut self,
        event: &gpui::MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.bounds.contains(&event.position) {
            return;
        }
        self.focus.focus(window, cx);
        self.claim(cx);
        let force_selection = !cfg!(target_os = "macos") && event.modifiers.shift;
        if self.buffer.modes().intersects(TermMode::MOUSE_MODE) && !force_selection {
            return;
        }
        let (row, column) = self.selection_position(event.position);
        if event.modifiers.shift && self.buffer.has_selection() {
            self.buffer.extend_selection(row, column);
        } else {
            self.buffer
                .begin_selection(row, column, event.modifiers.alt);
        }
        self.selecting = true;
        cx.notify();
        cx.stop_propagation();
    }

    fn scroll(&mut self, event: &ScrollEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let delta = f32::from(event.delta.pixel_delta(px(ROW_HEIGHT)).y) / ROW_HEIGHT;
        if delta.signum() != self.wheel_remainder.signum() {
            self.wheel_remainder = 0.;
        }
        self.wheel_remainder += delta;
        let lines = self.wheel_remainder.trunc().clamp(-1000., 1000.) as i32;
        self.wheel_remainder -= lines as f32;
        if lines == 0 {
            return;
        }
        let modes = self.buffer.modes();
        if modes.intersects(TermMode::MOUSE_MODE) && modes.contains(TermMode::SGR_MOUSE) {
            let row = ((event.position.y - self.bounds.top()) / px(ROW_HEIGHT))
                .floor()
                .clamp(0., f32::from(self.desired_size.rows - 1)) as u16
                + 1;
            let col = ((event.position.x - self.bounds.left()) / self.cell_width)
                .floor()
                .clamp(0., f32::from(self.desired_size.cols - 1)) as u16
                + 1;
            let button = if lines > 0 { 64 } else { 65 }
                + u8::from(event.modifiers.shift) * 4
                + u8::from(event.modifiers.alt) * 8
                + u8::from(event.modifiers.control) * 16;
            self.send_input(
                &format!("\x1b[<{button};{col};{row}M").repeat(lines.unsigned_abs() as usize),
                cx,
            );
        } else if modes.contains(TermMode::ALT_SCREEN) && !event.modifiers.shift {
            self.send_input(
                &(if lines > 0 { "\x1b[A" } else { "\x1b[B" })
                    .repeat(lines.unsigned_abs() as usize),
                cx,
            );
        } else {
            self.buffer.scroll(lines);
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

impl Focusable for TerminalView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
            .on_mouse_down(MouseButton::Left, cx.listener(Self::begin_selection))
            .when_some(self.error.clone(), |view, error| {
                view.child(div().px_3().py_1().text_color(rgb(0xf07178)).child(error))
            })
            .child(
                canvas(
                    move |bounds, window, cx| {
                        let font = font(crate::fonts::TERMINAL_FAMILY);
                        let width = window
                            .text_system()
                            .advance(window.text_system().resolve_font(&font), px(FONT_SIZE), 'M')
                            .map(|s| s.width)
                            .unwrap_or(px(8.));
                        measure_view.update(cx, |view, cx| view.measure(bounds, width, cx));
                        (font, width)
                    },
                    move |bounds, (base_font, width), window, cx| {
                        let focus = paint_view.read(cx).focus.clone();
                        window.handle_input(
                            &focus,
                            ElementInputHandler::new(bounds, paint_view.clone()),
                            cx,
                        );
                        window.on_mouse_event({
                            let view = paint_view.clone();
                            move |event: &gpui::MouseMoveEvent, phase, _, cx| {
                                if phase.bubble() && event.dragging() && view.read(cx).selecting {
                                    view.update(cx, |this, cx| {
                                        let (row, column) = this.selection_position(event.position);
                                        this.buffer.extend_selection(row, column);
                                        cx.notify();
                                    });
                                    cx.stop_propagation();
                                }
                            }
                        });
                        window.on_mouse_event({
                            let view = paint_view.clone();
                            move |event: &gpui::MouseUpEvent, phase, _, cx| {
                                if phase.bubble()
                                    && event.button == MouseButton::Left
                                    && view.read(cx).selecting
                                {
                                    view.update(cx, |this, cx| {
                                        let (row, column) = this.selection_position(event.position);
                                        this.buffer.extend_selection(row, column);
                                        this.selecting = false;
                                        cx.notify();
                                    });
                                }
                            }
                        });
                        paint_view.update(cx, |view, cx| {
                            let screen = view.buffer.screen();
                            window.with_content_mask(
                                Some(gpui::ContentMask { bounds }),
                                |window| {
                                    for cell in screen.cells {
                                        let origin = bounds.origin
                                            + point(
                                                width * cell.col as f32,
                                                px(ROW_HEIGHT * cell.row as f32),
                                            );
                                        if cell.background != 0x171919 {
                                            window.paint_quad(fill(
                                                Bounds::new(
                                                    origin,
                                                    size(
                                                        width * cell.columns as f32,
                                                        px(ROW_HEIGHT),
                                                    ),
                                                ),
                                                rgb(cell.background),
                                            ));
                                        }
                                        if cell.selected {
                                            window.paint_quad(fill(
                                                Bounds::new(
                                                    origin,
                                                    size(
                                                        width * cell.columns as f32,
                                                        px(ROW_HEIGHT),
                                                    ),
                                                ),
                                                rgba(crate::theme::terminal_selection::BACKGROUND),
                                            ));
                                        }
                                        if cell.character == ' ' && cell.zerowidth.is_empty() {
                                            continue;
                                        }
                                        let font = match (cell.bold, cell.italic) {
                                            (true, true) => base_font.clone().bold().italic(),
                                            (true, false) => base_font.clone().bold(),
                                            (false, true) => base_font.clone().italic(),
                                            _ => base_font.clone(),
                                        };
                                        let text: gpui::SharedString = if cell.zerowidth.is_empty()
                                        {
                                            cell.character.into()
                                        } else {
                                            cell.chars().collect::<String>().into()
                                        };
                                        let run = TextRun {
                                            len: text.len(),
                                            font,
                                            color: rgb(if cell.selected {
                                                crate::theme::terminal_selection::FOREGROUND
                                            } else {
                                                cell.foreground
                                            })
                                            .into(),
                                            background_color: None,
                                            underline: cell
                                                .underline
                                                .then_some(gpui::UnderlineStyle {
                                                thickness: px(1.),
                                                color: Some(
                                                    rgb(if cell.selected {
                                                        crate::theme::terminal_selection::FOREGROUND
                                                    } else {
                                                        cell.foreground
                                                    })
                                                    .into(),
                                                ),
                                                wavy: false,
                                            }),
                                            strikethrough: None,
                                        };
                                        let line = window.text_system().shape_line(
                                            text,
                                            px(FONT_SIZE),
                                            &[run],
                                            None,
                                        );
                                        let _ = line.paint(
                                            origin,
                                            px(ROW_HEIGHT),
                                            gpui::TextAlign::Left,
                                            None,
                                            window,
                                            cx,
                                        );
                                    }
                                    if let Some((row, col)) = screen.cursor {
                                        window.paint_quad(fill(
                                            Bounds::new(
                                                bounds.origin
                                                    + point(
                                                        width * col as f32,
                                                        px(ROW_HEIGHT * row as f32),
                                                    ),
                                                size(px(2.), px(ROW_HEIGHT)),
                                            ),
                                            rgb(0x82aaff),
                                        ));
                                    }
                                    let marked = view.marked.clone();
                                    if !marked.is_empty() {
                                        let (row, col) = screen.cursor.unwrap_or((0, 0));
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
                                            px(FONT_SIZE),
                                            &[run],
                                            None,
                                        );
                                        let _ = line.paint(
                                            bounds.origin
                                                + point(
                                                    width * col as f32,
                                                    px(ROW_HEIGHT * row as f32),
                                                ),
                                            px(ROW_HEIGHT),
                                            gpui::TextAlign::Left,
                                            None,
                                            window,
                                            cx,
                                        );
                                    }
                                },
                            );
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
            .marked
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
        let end = self.marked.encode_utf16().count();
        Some(UTF16Selection {
            range: end..end,
            reversed: false,
        })
    }
    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        (!self.marked.is_empty()).then(|| 0..self.marked.encode_utf16().count())
    }
    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.marked.clear();
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
        self.send_input(text, cx);
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
        self.marked = text.into();
        cx.notify();
    }
    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let (row, col) = self.buffer.screen().cursor.unwrap_or((0, 0));
        Some(Bounds::new(
            self.bounds.origin + point(self.cell_width * col as f32, px(ROW_HEIGHT * row as f32)),
            size(self.cell_width, px(ROW_HEIGHT)),
        ))
    }
    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.marked.encode_utf16().count())
    }
}

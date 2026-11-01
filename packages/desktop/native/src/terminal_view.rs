use std::{ops::Range, sync::Arc};

use alacritty_terminal::term::TermMode;
use anyhow::{Result, ensure};
use gpui::{
    App, Bounds, Context, ElementInputHandler, EntityInputHandler, FocusHandle, Focusable,
    KeyDownEvent, MouseButton, Pixels, Point, Render, ScrollEvent, TextRun, UTF16Selection, Window,
    canvas, div, fill, font, point, prelude::*, px, rgb, size,
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
        }
    }

    pub fn bind(&mut self, payload: &Value, cx: &mut Context<Self>) -> bool {
        if payload["requestId"].as_str() != self.request_id.as_deref() {
            return false;
        }
        if let Some(error) = payload["error"].as_str() {
            self.error = Some(error.into());
        } else {
            self.slot = payload["slot"]
                .as_u64()
                .and_then(|slot| u8::try_from(slot).ok());
        }
        cx.notify();
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
            self.flush_input()
        });
        self.finish(result, cx);
        cx.notify();
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
            let result = self.connection.request("subscribe_terminal_request", json!({"terminalId":self.terminal_id,"restore":{"mode":"full-snapshot","scrollbackLines":10_000,"size":{"rows":rows,"cols":cols}}}));
            match result {
                Ok(id) => self.request_id = Some(id),
                Err(error) => self.error = Some(error.to_string()),
            }
        } else {
            let result = self.resize_if_ready();
            self.finish(result, cx);
        }
    }

    fn resize_if_ready(&mut self) -> Result<()> {
        let Some(slot) = self.slot else {
            return Ok(());
        };
        if !self.replay_ready || self.buffer.replaying || self.desired_size == self.buffer.size {
            return Ok(());
        }
        self.buffer.resize(self.desired_size);
        self.connection.send_terminal(TerminalFrame { opcode: TerminalOpcode::Resize, slot, data: serde_json::to_vec(&json!({"rows":self.desired_size.rows,"cols":self.desired_size.cols,"intent":"update"}))? })
    }

    fn claim(&mut self, cx: &mut Context<Self>) {
        if !self.replay_ready {
            return;
        }
        let result = self.connection.send_session(json!({"type":"terminal_input","terminalId":self.terminal_id,"message":{"type":"resize","rows":self.desired_size.rows,"cols":self.desired_size.cols,"intent":"claim"}}));
        self.finish(result, cx);
    }

    fn send_input(&mut self, text: &str, cx: &mut Context<Self>) {
        let result = (|| {
            ensure!(
                self.pending_input.len() + text.len() <= 1024 * 1024,
                "terminal pending input exceeds 1 MiB"
            );
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
            cx.notify();
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = &event.keystroke;
        let shortcut = if cfg!(target_os = "macos") {
            key.modifiers.platform
        } else {
            key.modifiers.control && key.modifiers.shift
        };
        if shortcut && key.key == "v" {
            if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                let text = text.replace("\r\n", "\n").replace('\r', "\n");
                let text = if self.buffer.modes().contains(TermMode::BRACKETED_PASTE) {
                    format!("\x1b[200~{}\x1b[201~", text.replace('\x1b', ""))
                } else {
                    text
                };
                self.send_input(&text, cx);
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
        let _ = self.connection.send_session(
            json!({"type":"unsubscribe_terminal_request","terminalId":self.terminal_id}),
        );
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
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.focus.focus(window, cx);
                    this.claim(cx);
                }),
            )
            .when_some(self.error.clone(), |view, error| {
                view.child(div().px_3().py_1().text_color(rgb(0xf07178)).child(error))
            })
            .child(
                canvas(
                    move |bounds, window, cx| {
                        let family = if cfg!(target_os = "macos") {
                            "Menlo"
                        } else if cfg!(windows) {
                            "Consolas"
                        } else {
                            "monospace"
                        };
                        let font = font(family);
                        let width = window
                            .text_system()
                            .advance(window.text_system().resolve_font(&font), px(FONT_SIZE), 'M')
                            .map(|s| s.width)
                            .unwrap_or(px(8.));
                        measure_view.update(cx, |view, cx| view.measure(bounds, width, cx));
                        (measure_view.read(cx).buffer.screen(), font, width)
                    },
                    move |bounds, (screen, base_font, width), window, cx| {
                        let focus = paint_view.read(cx).focus.clone();
                        window.handle_input(
                            &focus,
                            ElementInputHandler::new(bounds, paint_view.clone()),
                            cx,
                        );
                        window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
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
                                            size(width * cell.columns as f32, px(ROW_HEIGHT)),
                                        ),
                                        rgb(cell.background),
                                    ));
                                }
                                if cell.text == " " {
                                    continue;
                                }
                                let font = match (cell.bold, cell.italic) {
                                    (true, true) => base_font.clone().bold().italic(),
                                    (true, false) => base_font.clone().bold(),
                                    (false, true) => base_font.clone().italic(),
                                    _ => base_font.clone(),
                                };
                                let text: gpui::SharedString = cell.text.into();
                                let run = TextRun {
                                    len: text.len(),
                                    font,
                                    color: rgb(cell.foreground).into(),
                                    background_color: None,
                                    underline: cell.underline.then_some(gpui::UnderlineStyle {
                                        thickness: px(1.),
                                        color: Some(rgb(cell.foreground).into()),
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
                            let marked = paint_view.read(cx).marked.clone();
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
                                        + point(width * col as f32, px(ROW_HEIGHT * row as f32)),
                                    px(ROW_HEIGHT),
                                    gpui::TextAlign::Left,
                                    None,
                                    window,
                                    cx,
                                );
                            }
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

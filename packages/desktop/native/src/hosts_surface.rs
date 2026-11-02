//! The native Hosts surface: a single-line text field and the hosts panel.
//! The panel is presentation and intent only — every connection, registry
//! write and daemon RPC is performed by `ui::Paseo` through
//! `paseo_desktop::hosts`, so all states shown here are real.

use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, Context, ElementInputHandler, Entity, EntityInputHandler,
    EventEmitter, FocusHandle, Focusable, KeyDownEvent, MouseButton, Pixels, Point, Render,
    TextAlign, TextRun, UTF16Selection, WrappedLine, canvas, div, fill, font, prelude::*, px, rgb,
    size,
};

const LINE_HEIGHT: f32 = 20.;
const FIELD_HEIGHT: f32 = 28.;

/// A minimal single-line text field with native IME support, following the
/// composer's editing model (`paseo_desktop::editing::Editing`).
pub struct TextField {
    focus: FocusHandle,
    pub placeholder: String,
    pub secret: bool,
    editing: paseo_desktop::editing::Editing,
    shaped: Option<WrappedLine>,
    bounds: Bounds<Pixels>,
    selecting: bool,
}

pub enum TextFieldEvent {
    /// Enter pressed with a non-empty, committed value.
    Submitted,
}

impl EventEmitter<TextFieldEvent> for TextField {}

impl TextField {
    pub fn new(placeholder: &str, secret: bool, cx: &mut Context<Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
            placeholder: placeholder.into(),
            secret,
            editing: Default::default(),
            shaped: None,
            bounds: Bounds::default(),
            selecting: false,
        }
    }

    pub fn value(&self) -> String {
        self.editing.text.trim().to_owned()
    }

    pub fn password(&self) -> Option<String> {
        if self.secret && !self.editing.text.is_empty() {
            Some(self.editing.text.clone())
        } else {
            None
        }
    }

    fn edited(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        window.notify_text_input_changed(&self.focus);
        cx.notify();
    }

    fn key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let key = &event.keystroke;
        if self.editing.marked.is_some() {
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
                    self.editing.select(0, false);
                    self.editing.select(self.editing.text.len(), true);
                }
                "c" | "x" => {
                    let selection = self.editing.selection.clone();
                    if !selection.is_empty() {
                        cx.write_to_clipboard(ClipboardItem::new_string(
                            self.editing.text[selection].to_owned(),
                        ));
                        if key.key == "x" {
                            self.editing.replace(None, "", false, None);
                        }
                    }
                }
                "v" => {
                    if let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) {
                        self.editing
                            .replace(None, &text.replace(['\r', '\n'], ""), false, None);
                    }
                }
                _ => return,
            }
        } else if key.modifiers.control || key.modifiers.platform || key.modifiers.alt {
            return;
        } else {
            match key.key.as_str() {
                "enter" if !extend => {
                    if !self.value().is_empty() {
                        cx.emit(TextFieldEvent::Submitted);
                    }
                }
                "backspace" => self.editing.delete(false),
                "delete" => self.editing.delete(true),
                "left" => self.editing.horizontal(false, extend),
                "right" => self.editing.horizontal(true, extend),
                "home" => self.editing.select(0, extend),
                "end" => self.editing.select(self.editing.text.len(), extend),
                _ => return, // Printable text goes through the native text service exactly once.
            }
        }
        self.edited(window, cx);
        window.prevent_default();
        cx.stop_propagation();
    }

    fn display_text(&self) -> String {
        if self.secret {
            self.editing.text.chars().map(|_| '•').collect()
        } else {
            self.editing.text.clone()
        }
    }

    fn layout(&mut self, bounds: Bounds<Pixels>, window: &mut gpui::Window) {
        self.bounds = bounds;
        let empty = self.editing.text.is_empty();
        let text = if empty {
            self.placeholder.clone()
        } else {
            self.display_text()
        };
        let selected = !empty && self.editing.selection.contains(&0);
        let runs = [TextRun {
            len: text.len(),
            font: font(crate::fonts::ui_family()),
            color: rgb(if empty {
                crate::theme::colors::SURFACE4
            } else {
                crate::theme::colors::FOREGROUND
            })
            .into(),
            background_color: selected.then(|| rgb(crate::theme::colors::SURFACE3).into()),
            underline: None,
            strikethrough: None,
        }];
        self.shaped = window
            .text_system()
            .shape_text(
                text.into(),
                px(crate::theme::font_size::SM),
                &runs,
                Some(bounds.size.width),
                None,
            )
            .ok()
            .and_then(|mut lines| (!lines.is_empty()).then(|| lines.remove(0)));
    }

    fn caret(&self) -> Option<Point<Pixels>> {
        let shaped = self.shaped.as_ref()?;
        Some(
            self.bounds.origin
                + shaped
                    .position_for_index(self.editing.cursor().min(shaped.len()), px(LINE_HEIGHT))?,
        )
    }

    fn index_at(&self, position: Point<Pixels>) -> usize {
        let Some(shaped) = &self.shaped else {
            return 0;
        };
        let local = position - self.bounds.origin;
        shaped
            .closest_index_for_position(local, px(LINE_HEIGHT))
            .unwrap_or_else(|index| index)
            .min(self.editing.text.len())
    }
}

impl Focusable for TextField {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EntityInputHandler for TextField {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual: &mut Option<Range<usize>>,
        _window: &mut gpui::Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.editing.byte_range(range);
        *actual = Some(self.editing.utf16_range(range.clone()));
        Some(self.editing.text[range].to_owned())
    }

    fn selected_text_range(
        &mut self,
        _ignore: bool,
        _window: &mut gpui::Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.editing.utf16_range(self.editing.selection.clone()),
            reversed: self.editing.reversed,
        })
    }

    fn set_selected_text_range(
        &mut self,
        range: Range<usize>,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        self.editing.selection = self.editing.byte_range(range);
        self.editing.reversed = false;
        self.edited(window, cx);
    }

    fn text_length_utf16(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.editing.text.encode_utf16().count())
    }

    fn marked_text_range(
        &self,
        _window: &mut gpui::Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.editing
            .marked
            .clone()
            .map(|range| self.editing.utf16_range(range))
    }

    fn unmark_text(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        self.editing.marked = None;
        self.edited(window, cx);
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        self.editing.replace(range, text, false, None);
        self.edited(window, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        self.editing.replace(range, text, true, selected);
        self.edited(window, cx);
    }

    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        _bounds: Bounds<Pixels>,
        _window: &mut gpui::Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let shaped = self.shaped.as_ref()?;
        let byte = self.editing.byte_range(range).start;
        Some(Bounds::new(
            self.bounds.origin
                + shaped.position_for_index(byte.min(shaped.len()), px(LINE_HEIGHT))?,
            size(px(1.), px(LINE_HEIGHT)),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _window: &mut gpui::Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.editing.utf16_offset(self.index_at(point)))
    }
}

impl Render for TextField {
    fn render(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        let measure = cx.entity();
        let paint = cx.entity();
        div()
            .id("hosts-text-field")
            .track_focus(&self.focus)
            .cursor_text()
            .rounded(px(crate::theme::radius::MD))
            .bg(rgb(crate::theme::colors::SURFACE1))
            .border_1()
            .border_color(rgb(if self.focus.is_focused(window) {
                crate::theme::colors::BORDER_ACCENT
            } else {
                crate::theme::colors::BORDER
            }))
            .h(px(FIELD_HEIGHT))
            .w_full()
            .px_2()
            .flex()
            .items_center()
            .on_key_down(cx.listener(Self::key_down))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.focus.focus(window, cx);
                    this.selecting = true;
                    let index = this.index_at(event.position);
                    this.editing.select(index, event.modifiers.shift);
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
                        this.editing.select(index, true);
                        this.edited(window, cx);
                    }
                }),
            )
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
                            if let Some(shaped) = view.shaped.as_ref() {
                                let origin = bounds.origin;
                                let _ = shaped.paint_background(
                                    origin,
                                    px(LINE_HEIGHT),
                                    TextAlign::Left,
                                    Some(bounds),
                                    window,
                                    cx,
                                );
                                let _ = shaped.paint(
                                    origin,
                                    px(LINE_HEIGHT),
                                    TextAlign::Left,
                                    Some(bounds),
                                    window,
                                    cx,
                                );
                            }
                            if view.focus.is_focused(window)
                                && let Some(origin) = view.caret()
                            {
                                window.paint_quad(fill(
                                    Bounds::new(origin, size(px(1.), px(LINE_HEIGHT))),
                                    rgb(crate::theme::colors::FOREGROUND),
                                ));
                            }
                        });
                    },
                )
                .w_full()
                .h(px(LINE_HEIGHT)),
            )
    }
}

/// Everything the panel displays comes from the runtime owner as a snapshot.
#[derive(Clone, Default, PartialEq)]
pub struct HostsSnapshot {
    pub profiles: Vec<paseo_desktop::hosts::HostProfile>,
    pub local_server_id: Option<String>,
    pub active_server_id: Option<String>,
    pub phase: Option<paseo_desktop::hosts::HostPhase>,
    /// (danger, text) message line under the host list.
    pub message: Option<(bool, String)>,
    pub adding: bool,
    pub plaintext_warning: bool,
    pub registrations: Option<paseo_desktop::hosts::daemon_registrations::HostDirectory>,
    /// In-flight daemon registration description.
    pub registration_busy: Option<String>,
    pub registration_error: Option<String>,
    pub connected: bool,
    /// The daemon declared the `connectedHosts` capability; management entry
    /// stays hidden without it so an old daemon never gets mutation traffic.
    pub connected_hosts_supported: bool,
}

#[derive(Clone)]
pub enum HostsPanelEvent {
    Connect {
        server_id: String,
    },
    Retry,
    AddHost {
        grammar: String,
        password: Option<String>,
    },
    RemoveHost {
        server_id: String,
    },
    RegisterHost {
        grammar: String,
    },
    RemoveRegistration {
        registration_id: String,
    },
}

impl EventEmitter<HostsPanelEvent> for HostsPanel {}

pub struct HostsPanel {
    snapshot: HostsSnapshot,
    grammar: Entity<TextField>,
    password: Entity<TextField>,
    confirm_remove: Option<String>,
    _grammar_subscription: gpui::Subscription,
}

impl HostsPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let grammar =
            cx.new(|cx| TextField::new("host:port, tcp://, ssh://, pairing URL", false, cx));
        let password = cx.new(|cx| TextField::new("Host password (optional)", true, cx));
        let grammar_subscription = cx.subscribe(&grammar, |this, _, _: &TextFieldEvent, cx| {
            this.submit_add(cx);
        });
        Self {
            snapshot: HostsSnapshot::default(),
            grammar,
            password,
            confirm_remove: None,
            _grammar_subscription: grammar_subscription,
        }
    }

    pub fn set_snapshot(&mut self, snapshot: HostsSnapshot, cx: &mut Context<Self>) {
        if snapshot != self.snapshot {
            self.snapshot = snapshot;
            cx.notify();
        }
    }

    fn submit_add(&mut self, cx: &mut Context<Self>) {
        let grammar = self.grammar.read(cx).value();
        let password = self.grammar.read(cx).password();
        let password = password.or_else(|| self.password.read(cx).password());
        if grammar.is_empty() || self.snapshot.adding {
            return;
        }
        cx.emit(HostsPanelEvent::AddHost { grammar, password });
    }

    fn dot(&self, color: u32, busy: bool) -> gpui::AnyElement {
        use crate::theme::radius;
        div()
            .size(px(8.))
            .flex_shrink_0()
            .rounded_full()
            .when(busy, |dot| {
                dot.child(crate::chrome::loading_spinner(8., color).into_any_element())
            })
            .when(!busy, |dot| dot.bg(gpui::rgba((color << 8) | 0xff)))
            .rounded(px(radius::FULL))
            .into_any_element()
    }
}

fn host_description(connection: &paseo_desktop::hosts::HostConnection) -> String {
    match connection {
        paseo_desktop::hosts::HostConnection::DirectTcp { endpoint, .. } => endpoint.clone(),
        paseo_desktop::hosts::HostConnection::DirectSocket { .. }
        | paseo_desktop::hosts::HostConnection::DirectPipe { .. } => "Local".into(),
        paseo_desktop::hosts::HostConnection::RemoteSsh { host, .. } => format!("ssh://{host}"),
        paseo_desktop::hosts::HostConnection::Relay { relay_endpoint, .. } => {
            relay_endpoint.clone()
        }
        paseo_desktop::hosts::HostConnection::Routed {
            origin_server_id, ..
        } => {
            format!("via {origin_server_id}")
        }
    }
}

impl Render for HostsPanel {
    fn render(&mut self, _window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        use crate::theme::{colors as c, font_size, radius};
        let snapshot = &self.snapshot;
        let mut column = div().flex().flex_col().gap(px(6.)).px_2();

        column = column.child(
            div()
                .text_size(px(font_size::SM))
                .text_color(rgb(c::FOREGROUND_MUTED))
                .child("Saved hosts"),
        );

        for profile in &snapshot.profiles {
            let active = snapshot.active_server_id.as_deref() == Some(profile.server_id.as_str());
            let (dot, busy, failed) = if active {
                match &snapshot.phase {
                    Some(paseo_desktop::hosts::HostPhase::Online) => {
                        (c::STATUS_DOT_SUCCESS, false, false)
                    }
                    Some(paseo_desktop::hosts::HostPhase::Connecting) => {
                        (c::STATUS_DOT_WARNING, true, false)
                    }
                    Some(paseo_desktop::hosts::HostPhase::Failed(_)) => {
                        (c::STATUS_DOT_DANGER, false, true)
                    }
                    _ => (c::FOREGROUND_EXTRA_MUTED, false, false),
                }
            } else {
                (c::FOREGROUND_EXTRA_MUTED, false, false)
            };
            let local = snapshot.local_server_id.as_deref() == Some(profile.server_id.as_str());
            let description = profile
                .connections
                .first()
                .map(host_description)
                .unwrap_or_default();
            let confirming = self.confirm_remove.as_deref() == Some(profile.server_id.as_str());
            let server_id = profile.server_id.clone();
            let connect_id = server_id.clone();
            let remove_id = server_id.clone();
            let retry_row_id = format!("host-retry-{server_id}");
            let remove_row_id = format!("host-remove-{server_id}");
            let label = if local {
                format!("{} (local)", profile.label)
            } else {
                profile.label.clone()
            };
            column = column.child(
                div()
                    .id(gpui::SharedString::from(format!("host-row-{server_id}")))
                    .min_h(px(28.))
                    .px_2()
                    .rounded(px(radius::MD))
                    .flex()
                    .items_center()
                    .gap_2()
                    .when(active, |row| row.bg(rgb(c::SURFACE_SIDEBAR_SELECTED)))
                    .hover(|row| row.bg(rgb(c::SURFACE_SIDEBAR_HOVER)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            if this.snapshot.active_server_id.as_deref()
                                == Some(connect_id.as_str())
                            {
                                return;
                            }
                            this.confirm_remove = None;
                            cx.emit(HostsPanelEvent::Connect {
                                server_id: connect_id.clone(),
                            });
                        }),
                    )
                    .child(self.dot(dot, busy))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(font_size::SM))
                                    .child(label),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(font_size::SM))
                                    .text_color(rgb(c::FOREGROUND_EXTRA_MUTED))
                                    .child(description),
                            ),
                    )
                    .when(failed && active, |row| {
                        row.child(small_button(
                            retry_row_id,
                            "Retry",
                            c::STATUS_DANGER,
                            cx.listener(|_this, _, _, cx| {
                                cx.stop_propagation();
                                cx.emit(HostsPanelEvent::Retry);
                            }),
                        ))
                    })
                    .when(!confirming, |row| {
                        row.child(small_button(
                            remove_row_id,
                            "×",
                            c::FOREGROUND_MUTED,
                            cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.confirm_remove = Some(remove_id.clone());
                                cx.notify();
                            }),
                        ))
                    }),
            );
            if confirming {
                let remove_id = profile.server_id.clone();
                let remove_yes_id = format!("host-remove-yes-{remove_id}");
                let remove_no_id = format!("host-remove-no-{remove_id}");
                let remove_host_id = remove_id.clone();
                column = column.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_end()
                        .gap_2()
                        .px_2()
                        .child(
                            div()
                                .text_size(px(font_size::SM))
                                .text_color(rgb(c::FOREGROUND_MUTED))
                                .child("Remove this saved host?"),
                        )
                        .child(small_button(
                            remove_yes_id,
                            "Remove",
                            c::STATUS_DANGER,
                            cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.confirm_remove = None;
                                cx.emit(HostsPanelEvent::RemoveHost {
                                    server_id: remove_host_id.clone(),
                                });
                            }),
                        ))
                        .child(small_button(
                            remove_no_id,
                            "Keep",
                            c::FOREGROUND_MUTED,
                            cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.confirm_remove = None;
                                cx.notify();
                            }),
                        )),
                );
            }
        }

        // Add-host form. One grammar input serves the client list and the
        // daemon's own registrations.
        let grammar_value = self.grammar.read(cx).value();
        let password_present = self.password.read(cx).password().is_some();
        let adding = snapshot.adding;
        column = column
            .child(
                div()
                    .mt_2()
                    .text_size(px(font_size::SM))
                    .text_color(rgb(c::FOREGROUND_MUTED))
                    .child("Add host"),
            )
            .child(self.grammar.clone())
            .child(self.password.clone())
            .when(password_present && snapshot.plaintext_warning, |form| {
                form.child(
                    div()
                        .text_size(px(font_size::SM))
                        .text_color(rgb(c::STATUS_WARNING))
                        .child(paseo_desktop::hosts::PLAINTEXT_CREDENTIAL_LIMITATION),
                )
            })
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(action_button(
                        "hosts-add",
                        if adding {
                            "Adding…"
                        } else {
                            "Connect & save"
                        },
                        !adding && !grammar_value.is_empty(),
                        c::ACCENT,
                        c::ACCENT_FOREGROUND,
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            if !adding {
                                this.submit_add(cx);
                            }
                        }),
                    ))
                    .when(
                        snapshot.connected && snapshot.connected_hosts_supported,
                        |row| {
                            let registering = snapshot.registration_busy.is_some();
                            let grammar = grammar_value.clone();
                            row.child(action_button(
                                "hosts-register",
                                if registering {
                                    "Registering…"
                                } else {
                                    "Register on daemon"
                                },
                                !registering && !grammar_value.is_empty(),
                                c::SECONDARY,
                                c::SECONDARY_FOREGROUND,
                                cx.listener(move |_, _, _, cx| {
                                    cx.stop_propagation();
                                    if !registering {
                                        cx.emit(HostsPanelEvent::RegisterHost {
                                            grammar: grammar.clone(),
                                        });
                                    }
                                }),
                            ))
                        },
                    ),
            );

        if let Some((danger, message)) = &snapshot.message {
            column = column.child(
                div()
                    .text_size(px(font_size::SM))
                    .text_color(rgb(if *danger {
                        c::STATUS_DANGER
                    } else {
                        c::STATUS_SUCCESS
                    }))
                    .child(message.clone()),
            );
        }
        if matches!(
            snapshot.phase.as_ref(),
            Some(paseo_desktop::hosts::HostPhase::Failed(_))
        ) {
            column = column.child(action_button(
                "hosts-retry",
                "Retry connection",
                true,
                c::SECONDARY,
                c::SECONDARY_FOREGROUND,
                cx.listener(|_, _, _, cx| {
                    cx.stop_propagation();
                    cx.emit(HostsPanelEvent::Retry);
                }),
            ));
        }

        if let Some(directory) = &snapshot.registrations {
            let mut rows = Vec::new();
            for host in &directory.hosts {
                let (dot, busy) = match host.status.as_str() {
                    "online" => (c::STATUS_DOT_SUCCESS, false),
                    "connecting" => (c::STATUS_DOT_WARNING, true),
                    "error" => (c::STATUS_DOT_DANGER, false),
                    _ => (c::FOREGROUND_EXTRA_MUTED, false),
                };
                let registration_id = host.registration_id.clone();
                let error = host
                    .error
                    .as_ref()
                    .and_then(|error| error["message"].as_str().map(str::to_owned))
                    .unwrap_or_else(|| host.target_description.clone());
                rows.push(
                    div()
                        .id(gpui::SharedString::from(format!(
                            "reg-row-{}",
                            host.registration_id
                        )))
                        .min_h(px(28.))
                        .px_2()
                        .rounded(px(radius::MD))
                        .flex()
                        .items_center()
                        .gap_2()
                        .hover(|row| row.bg(rgb(c::SURFACE_SIDEBAR_HOVER)))
                        .child(self.dot(dot, busy))
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(px(font_size::SM))
                                        .child(
                                            host.label
                                                .clone()
                                                .unwrap_or_else(|| host.registration_id.clone()),
                                        ),
                                )
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(px(font_size::SM))
                                        .text_color(rgb(c::FOREGROUND_EXTRA_MUTED))
                                        .child(format!("{} — {error}", host.status)),
                                ),
                        )
                        .child(small_button(
                            format!("reg-remove-{}", host.registration_id),
                            "×",
                            c::FOREGROUND_MUTED,
                            cx.listener(move |_, _, _, cx| {
                                cx.stop_propagation();
                                cx.emit(HostsPanelEvent::RemoveRegistration {
                                    registration_id: registration_id.clone(),
                                });
                            }),
                        )),
                );
            }
            column = column
                .child(
                    div()
                        .mt_2()
                        .text_size(px(font_size::SM))
                        .text_color(rgb(c::FOREGROUND_MUTED))
                        .child(format!("Daemon registrations ({})", directory.hosts.len())),
                )
                .children(rows);
        }
        // Rendered outside the directory condition: a daemon that refuses
        // observation never populates `registrations`, and that refusal must
        // still be visible.
        if let Some(error) = &snapshot.registration_error {
            column = column.child(
                div()
                    .text_size(px(font_size::SM))
                    .text_color(rgb(c::STATUS_DANGER))
                    .child(error.clone()),
            );
        }

        div()
            .id("hosts-panel-scroll")
            .w_full()
            .overflow_y_scroll()
            .child(column)
    }
}

fn small_button(
    id: String,
    label: &str,
    color: u32,
    handler: impl Fn(&gpui::MouseDownEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> gpui::AnyElement {
    use crate::theme::{font_size, radius};
    div()
        .id(gpui::SharedString::from(id))
        .px_1()
        .rounded(px(radius::SM))
        .text_size(px(font_size::SM))
        .text_color(rgb(color))
        .cursor_pointer()
        .hover(|button| button.bg(gpui::rgba(0xffffff14)))
        .on_mouse_down(MouseButton::Left, handler)
        .child(label.to_owned())
        .into_any_element()
}

fn action_button(
    id: &str,
    label: &str,
    enabled: bool,
    background: u32,
    foreground: u32,
    handler: impl Fn(&gpui::MouseDownEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> gpui::AnyElement {
    use crate::theme::{font_size, radius};
    div()
        .id(gpui::SharedString::from(format!("{id}-button")))
        .h(px(26.))
        .px_2()
        .rounded(px(radius::MD))
        .flex()
        .items_center()
        .text_size(px(font_size::SM))
        .when(enabled, |button| {
            button
                .bg(rgb(background))
                .text_color(rgb(foreground))
                .cursor_pointer()
                .on_mouse_down(MouseButton::Left, handler)
        })
        .when(!enabled, |button| {
            button
                .bg(rgb(crate::theme::colors::SURFACE2))
                .text_color(rgb(crate::theme::colors::FOREGROUND_EXTRA_MUTED))
        })
        .child(label.to_owned())
        .into_any_element()
}

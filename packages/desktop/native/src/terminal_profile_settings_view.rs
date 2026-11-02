use crate::profile_settings_presentation as p;
use crate::{
    chrome,
    settings_view::{SharedSettings, active_palette},
    text_field::{FieldEvent, TextField},
};
use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, MouseButton, Render, SharedString,
    Task, Window, div, prelude::*, px, rgb, rgba,
};
use paseo_desktop::{
    client::Connection,
    terminal_profile_settings::{Change, Draft, Outcome, State},
};
use serde_json::{Value, json};
use std::sync::Arc;

pub enum Event {
    Close,
    Focus,
}
struct Editor {
    id: u64,
    base: Option<Value>,
    profile_id: String,
    fields: Vec<Entity<TextField>>,
    _subscriptions: Vec<gpui::Subscription>,
    errors: (bool, bool),
}
pub struct HostTerminals {
    connection: Arc<Connection>,
    shared: SharedSettings,
    pub state: State,
    focus: FocusHandle,
    host: String,
    editor: Option<Editor>,
    next_editor: u64,
    prompt: Option<Task<()>>,
    subscribed: bool,
}
impl EventEmitter<Event> for HostTerminals {}
impl Focusable for HostTerminals {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
impl HostTerminals {
    pub fn new(
        connection: Arc<Connection>,
        host: String,
        shared: SharedSettings,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self {
            connection,
            host,
            shared,
            state: State::default(),
            focus: cx.focus_handle(),
            editor: None,
            next_editor: 0,
            prompt: None,
            subscribed: false,
        };
        this.load(cx);
        this
    }
    pub fn settings_changed(&mut self, cx: &mut Context<Self>) {
        if let Some(editor) = &self.editor {
            for field in &editor.fields {
                field.update(cx, |field, cx| field.settings_changed(cx));
            }
        }
        cx.notify();
    }
    fn t(&self, key: &str) -> String {
        paseo_desktop::locales::translate(
            paseo_desktop::locales::active_locale(self.shared.get().language),
            paseo_desktop::locales::Tx::key(key),
        )
    }
    fn timeout(&self, id: String, cx: &mut Context<Self>) {
        let timer = cx
            .background_executor()
            .timer(std::time::Duration::from_secs(150));
        cx.spawn(async move |this, cx| {
            timer.await;
            let _ = this.update(cx, |this, cx| {
                this.state.expire(&id);
                cx.notify();
            });
        })
        .detach();
    }
    pub fn load(&mut self, cx: &mut Context<Self>) {
        if !self.subscribed {
            match self.connection.subscribe(
                "terminal-profile-settings-events",
                "session.events.set_subscription.request",
                json!({"events":["status.daemon_config_changed"]}),
            ) {
                Ok(_) => self.subscribed = true,
                Err(error) => {
                    self.state.error = Some(error.to_string());
                    cx.notify();
                    return;
                }
            }
        }
        match self
            .connection
            .request("get_daemon_config_request", json!({}))
        {
            Ok(id) => {
                self.state.begin_read(id.clone());
                self.timeout(id, cx);
            }
            Err(error) => self.state.error = Some(error.to_string()),
        }
        cx.notify();
    }
    pub fn disconnected(&mut self, cx: &mut Context<Self>) {
        self.subscribed = false;
        self.state.disconnected();
        cx.notify();
    }
    pub fn receive(&mut self, kind: &str, payload: &Value, cx: &mut Context<Self>) -> bool {
        match self.state.receive(kind, payload) {
            Ok(Outcome::Ignored) => false,
            Ok(Outcome::Saved(editor)) => {
                if editor.is_some() && self.editor.as_ref().map(|e| e.id) == editor {
                    self.editor = None;
                    cx.emit(Event::Focus);
                }
                cx.notify();
                true
            }
            Ok(_) => {
                cx.notify();
                kind != "status"
            }
            Err(error) => {
                self.state.error = Some(error.to_string());
                self.state.needs_reload = true;
                cx.notify();
                true
            }
        }
    }
    fn submit(
        &mut self,
        change: Change,
        editor: Option<u64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.state.patch(&change).and_then(|patch| {
            self.connection
                .request("set_daemon_config_request", json!({"config":patch}))
        }) {
            Ok(id) => {
                self.state.begin_write(id.clone(), editor);
                self.focus.focus(window, cx);
                self.timeout(id, cx);
            }
            Err(error) => self.state.error = Some(error.to_string()),
        }
        cx.notify();
    }
    fn open_editor(&mut self, profile: Option<Value>, window: &mut Window, cx: &mut Context<Self>) {
        if !self.state.writable() {
            return;
        }
        let draft = profile
            .as_ref()
            .map(Draft::from_profile)
            .unwrap_or_default();
        let values = [draft.name, draft.command, draft.args];
        let placeholders = [
            "settings.host.terminalProfiles.namePlaceholder",
            "settings.host.terminalProfiles.commandPlaceholder",
            "settings.host.terminalProfiles.argsPlaceholder",
        ];
        let mut fields = vec![];
        let mut subscriptions = vec![];
        for (index, value) in values.into_iter().enumerate() {
            let placeholder = self.t(placeholders[index]);
            let shared = self.shared.clone();
            let field = cx.new(|cx| {
                TextField::new(&value, &placeholder, false, 0., false, shared, cx).form()
            });
            subscriptions.push(cx.subscribe_in(
                &field,
                window,
                move |this, _, event: &FieldEvent, window, cx| {
                    if this.state.busy() {
                        return;
                    }
                    if matches!(event, FieldEvent::Committed) {
                        if index < 2 {
                            if let Some(editor) = &this.editor {
                                editor.fields[index + 1]
                                    .read(cx)
                                    .focus_handle(cx)
                                    .focus(window, cx);
                            }
                        } else {
                            this.save_editor(window, cx);
                        }
                    } else if let Some(editor) = &mut this.editor {
                        if index == 0 {
                            editor.errors.0 = false;
                        }
                        if index == 1 {
                            editor.errors.1 = false;
                        }
                        cx.notify();
                    }
                },
            ));
            fields.push(field);
        }
        self.next_editor += 1;
        let profile_id = profile
            .as_ref()
            .and_then(|p| p["id"].as_str())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("profile_{}", uuid::Uuid::new_v4()));
        self.state.error = None;
        self.editor = Some(Editor {
            id: self.next_editor,
            base: profile,
            profile_id,
            fields,
            _subscriptions: subscriptions,
            errors: (false, false),
        });
        // The fields must join the rendered focus tree before taking focus.
        // Focusing a freshly created handle during the opening click is lost
        // when GPUI replaces the previous frame's dispatch tree.
        let editor_id = self.next_editor;
        cx.on_next_frame(window, move |this, window, cx| {
            if let Some(editor) = &this.editor {
                if editor.id == editor_id {
                    editor.fields[0].read(cx).focus_handle(cx).focus(window, cx);
                }
            }
        });
        cx.notify();
    }
    fn save_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.busy() {
            return;
        }
        if self.state.needs_reload {
            self.load(cx);
            return;
        }
        let Some(editor) = &mut self.editor else {
            return;
        };
        let draft = Draft {
            name: editor.fields[0].read(cx).text().into(),
            command: editor.fields[1].read(cx).text().into(),
            args: editor.fields[2].read(cx).text().into(),
        };
        editor.errors = draft.errors();
        if editor.errors.0 || editor.errors.1 {
            cx.notify();
            return;
        }
        let id = editor.id;
        let change = match &editor.base {
            Some(base) => Change::Edit {
                base: base.clone(),
                draft,
            },
            None => Change::Add {
                id: editor.profile_id.clone(),
                draft,
            },
        };
        self.submit(change, Some(id), window, cx);
    }
    fn cancel_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.state.busy() {
            self.editor = None;
            self.state.error = None;
            self.focus.focus(window, cx);
            cx.notify();
        }
    }
    fn remove(&mut self, profile: Value, window: &mut Window, cx: &mut Context<Self>) {
        if !self.state.writable() || self.prompt.is_some() {
            return;
        }
        let name = profile["name"].as_str().unwrap_or_default();
        let message = self
            .t("settings.host.terminalProfiles.removeConfirmMessage")
            .replace("{{name}}", name);
        match crate::native_confirmation::ask(
            window,
            cx,
            self.t("settings.host.terminalProfiles.removeConfirmTitle"),
            message,
            self.t("common.actions.cancel"),
            self.t("settings.host.terminalProfiles.remove"),
        ) {
            Ok(answer) => {
                self.focus.focus(window, cx);
                self.prompt = Some(cx.spawn(async move |this, cx| {
                    let confirmed = answer.await;
                    let _ = this.update(cx, |this, cx| {
                        this.prompt = None;
                        cx.emit(Event::Focus);
                        if confirmed {
                            let id = profile["id"].as_str().unwrap_or_default().to_owned();
                            match this.state.patch(&Change::Remove(id)).and_then(|patch| {
                                this.connection
                                    .request("set_daemon_config_request", json!({"config":patch}))
                            }) {
                                Ok(id) => {
                                    this.state.begin_write(id.clone(), None);
                                    this.timeout(id, cx);
                                }
                                Err(error) => this.state.error = Some(error.to_string()),
                            }
                        }
                        cx.notify();
                    });
                }));
            }
            Err(error) => self.state.error = Some(error.to_string()),
        }
        cx.notify();
    }
    fn icon_button(
        &self,
        id: String,
        glyph: &str,
        enabled: bool,
        color: u32,
        window: &Window,
    ) -> gpui::Stateful<gpui::Div> {
        let palette = active_palette(&self.shared, window);
        div()
            .id(SharedString::from(id))
            .w(px(p::ICON_BUTTON_WIDTH))
            .h(px(p::ICON_BUTTON_HEIGHT))
            .flex_shrink_0()
            .rounded(px(6.))
            .flex()
            .items_center()
            .justify_center()
            .when(enabled, |v| {
                v.cursor_pointer().hover(|v| v.bg(rgb(palette.surface2)))
            })
            .when(!enabled, |v| v.opacity(0.5))
            .child(chrome::icon(glyph, p::ICON_BUTTON_GLYPH).text_color(rgb(color)))
    }
    fn text_button(
        &self,
        id: &str,
        label: String,
        primary: bool,
        enabled: bool,
        window: &Window,
    ) -> gpui::Stateful<gpui::Div> {
        let palette = active_palette(&self.shared, window);
        div()
            .id(SharedString::from(id.to_owned()))
            .h(px(44.))
            .flex_1()
            .px_4()
            .rounded(px(p::BUTTON_RADIUS))
            .flex()
            .items_center()
            .justify_center()
            .bg(rgb(if primary {
                palette.accent
            } else {
                palette.surface3
            }))
            .text_color(rgb(if primary {
                palette.accent_foreground
            } else {
                palette.foreground
            }))
            .when(enabled, |v| v.cursor_pointer())
            .when(!enabled, |v| v.opacity(0.5))
            .child(label)
    }
    fn key_down(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.keystroke.key == "escape" {
            if self.editor.is_some() {
                self.cancel_editor(window, cx);
            } else if !self.state.busy() && self.prompt.is_none() {
                cx.emit(Event::Close);
            }
            cx.stop_propagation();
        }
        if event.keystroke.key == "tab" {
            if let Some(editor) = &self.editor {
                if let Some(index) = editor
                    .fields
                    .iter()
                    .position(|field| field.read(cx).focus_handle(cx).is_focused(window))
                {
                    let next = if event.keystroke.modifiers.shift {
                        (index + 2) % 3
                    } else {
                        (index + 1) % 3
                    };
                    editor.fields[next]
                        .read(cx)
                        .focus_handle(cx)
                        .focus(window, cx);
                    cx.stop_propagation();
                }
            }
        }
    }
    pub fn overlay(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if self.prompt.is_some() {
            Some(div().absolute().inset_0().occlude().into_any_element())
        } else {
            self.render_editor(window, cx)
        }
    }
    fn render_editor(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let editor = self.editor.as_ref()?;
        let palette = active_palette(&self.shared, window);
        let busy = self.state.busy();
        let mut body = div().flex().flex_col().gap(px(p::BODY_GAP)).p(px(p::INSET));
        for (index, field) in editor.fields.iter().enumerate() {
            let (label, error) = match index {
                0 => (
                    "settings.host.terminalProfiles.nameLabel",
                    editor
                        .errors
                        .0
                        .then_some("settings.host.terminalProfiles.nameRequired"),
                ),
                1 => (
                    "settings.host.terminalProfiles.commandLabel",
                    editor
                        .errors
                        .1
                        .then_some("settings.host.terminalProfiles.commandRequired"),
                ),
                _ => ("settings.host.terminalProfiles.argsLabel", None),
            };
            body = body.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(p::FIELD_GAP))
                    .child(
                        div()
                            // The matched Electron capture gives a 19px normal
                            // label line box at 14px, rather than the pane's 20px.
                            .line_height(px(19.))
                            .text_color(rgb(palette.foreground_muted))
                            .child(self.t(label)),
                    )
                    .child(field.clone())
                    .when(index == 2, |v| {
                        v.child(
                            div()
                                .text_size(px(12.))
                                .line_height(px(p::HINT_LINE))
                                .text_color(rgb(palette.foreground_muted))
                                .child(self.t("settings.host.terminalProfiles.argsHint")),
                        )
                    })
                    .when_some(error, |v, key| {
                        v.child(
                            div()
                                .text_size(px(12.))
                                .text_color(rgb(palette.status_danger))
                                .child(self.t(key)),
                        )
                    }),
            );
        }
        let cancel = self
            .text_button(
                "cancel-profile",
                self.t("common.actions.cancel"),
                false,
                !busy,
                window,
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    cx.stop_propagation();
                    this.cancel_editor(window, cx);
                }),
            );
        let save = self
            .text_button(
                "save-profile",
                self.t(if busy {
                    "settings.host.terminalProfiles.saving"
                } else {
                    "settings.host.terminalProfiles.save"
                }),
                true,
                !busy,
                window,
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    cx.stop_propagation();
                    this.save_editor(window, cx);
                }),
            );
        body = body
            .when_some(self.state.error.clone(), |v, error| {
                v.child(div().text_color(rgb(palette.status_danger)).child(error))
            })
            .child(div().flex().gap_2().mt_2().mb_2().child(cancel).child(save));
        let title = self.t(if editor.base.is_some() {
            "settings.host.terminalProfiles.editProfileTitle"
        } else {
            "settings.host.terminalProfiles.addProfileTitle"
        });
        Some(
            div()
                .absolute()
                .inset_0()
                .occlude()
                .on_key_down(cx.listener(Self::key_down))
                .bg(rgba(0x0000008c))
                .flex()
                .items_center()
                .justify_center()
                .p_6()
                .child(
                    div()
                        .relative()
                        .w_full()
                        .max_w(px(p::MODAL_WIDTH))
                        .on_mouse_down_out(cx.listener(
                            |this, event: &gpui::MouseDownEvent, window, cx| {
                                if event.button == MouseButton::Left {
                                    cx.stop_propagation();
                                    this.cancel_editor(window, cx);
                                }
                            },
                        ))
                        .max_h(window.viewport_size().height * 0.85)
                        .border_1()
                        .border_color(rgb(palette.surface2))
                        .rounded(px(p::MODAL_RADIUS))
                        .bg(rgb(palette.surface1))
                        .overflow_hidden()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .px_6()
                                .py_4()
                                .border_b_1()
                                .border_color(rgb(palette.surface2))
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(div().font_weight(gpui::FontWeight::MEDIUM).child(title))
                                .child(
                                    self.icon_button(
                                        "close-profile-editor".into(),
                                        "x",
                                        !busy,
                                        palette.foreground_muted,
                                        window,
                                    )
                                    .w(px(32.))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _, window, cx| {
                                            cx.stop_propagation();
                                            this.cancel_editor(window, cx);
                                        }),
                                    ),
                                ),
                        )
                        .child(
                            div()
                                .id("profile-form-scroll")
                                .overflow_y_scroll()
                                .child(body),
                        )
                        .when(busy, |v| v.child(div().absolute().inset_0().occlude())),
                )
                .into_any_element(),
        )
    }
}
impl Render for HostTerminals {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = active_palette(&self.shared, window);
        let enabled = self.state.writable() && self.prompt.is_none();
        let profiles = self.state.profiles.clone();
        let header = div()
            .h(px(52.))
            .px_4()
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap_2()
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(
                self.icon_button(
                    "back-host-terminals".into(),
                    "arrow-left",
                    !self.state.busy(),
                    palette.foreground_muted,
                    window,
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        if !this.state.busy() {
                            cx.emit(Event::Close);
                        }
                    }),
                ),
            )
            .child(format!(
                "{} / {}",
                self.host,
                self.t("settings.hostSections.terminals")
            ));
        let hook_card=div().p_4().flex().items_center().border_1().border_color(rgb(palette.border)).rounded(px(8.)).bg(rgb(palette.surface1)).child(div().flex_1().mr_3().child("Enable terminal agent hooks").child(div().mt_1().text_size(px(12.)).line_height(px(18.)).text_color(rgb(palette.foreground_muted)).child("Get notifications and status from terminal agents. This installs hooks in your agent config files."))).child(div().id("terminal-hooks-toggle").w(px(36.)).h(px(20.)).rounded_full().bg(rgb(if self.state.hooks{palette.accent}else{palette.surface3})).flex().items_center().px(px(2.)).when(self.state.hooks,|v|v.justify_end()).child(div().size(px(16.)).rounded_full().bg(rgb(palette.foreground))).when(enabled,|v|v.cursor_pointer()).on_mouse_down(MouseButton::Left,cx.listener(move|this,_,window,cx|{if enabled{this.submit(Change::Hooks(!this.state.hooks),None,window,cx);}})));
        let mut card = div()
            .border_1()
            .border_color(rgb(palette.border))
            .rounded(px(8.))
            .overflow_hidden()
            .bg(rgb(palette.surface1));
        if let Some(profiles) = profiles {
            if profiles.is_empty() {
                card = card.child(
                    div()
                        .p_4()
                        .text_color(rgb(palette.foreground_muted))
                        .child(self.t("settings.host.terminalProfiles.emptyState")),
                );
            }
            for (index, profile) in profiles.iter().enumerate() {
                let id = profile["id"].as_str().unwrap_or_default().to_owned();
                let parsed = serde_json::from_value::<paseo_desktop::terminal_lifecycle::Profile>(
                    profile.clone(),
                )
                .ok();
                let key = parsed.as_ref().and_then(|p| p.icon_key());
                let command = Draft::from_profile(profile);
                let command = if command.args.is_empty() {
                    command.command
                } else {
                    format!("{} {}", command.command, command.args)
                };
                let mut row = div()
                    .min_h(px(p::ROW_MIN_HEIGHT))
                    .p_4()
                    .flex()
                    .items_center()
                    .gap_2()
                    .when(index > 0, |v| {
                        v.border_t_1().border_color(rgb(palette.border))
                    })
                    .child(
                        chrome::terminal_profile_icon(key, p::ROW_ICON)
                            .text_color(rgb(palette.foreground_muted)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .mr_3()
                            .child(
                                div()
                                    .truncate()
                                    .child(profile["name"].as_str().unwrap_or_default().to_owned()),
                            )
                            .child(
                                div()
                                    .mt_1()
                                    .truncate()
                                    .text_size(px(12.))
                                    .line_height(px(18.))
                                    .text_color(rgb(palette.foreground_muted))
                                    .child(command),
                            ),
                    );
                let up = id.clone();
                let down = id.clone();
                let edit = profile.clone();
                let remove = profile.clone();
                row = row.child(
                    div()
                        .flex()
                        .items_center()
                        .child(
                            self.icon_button(
                                format!("profile-up-{id}"),
                                "arrow-up",
                                enabled && index > 0,
                                palette.foreground_muted,
                                window,
                            )
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, window, cx| {
                                    if enabled && index > 0 {
                                        this.submit(
                                            Change::Move {
                                                id: up.clone(),
                                                offset: -1,
                                            },
                                            None,
                                            window,
                                            cx,
                                        );
                                    }
                                }),
                            ),
                        )
                        .child(
                            self.icon_button(
                                format!("profile-down-{id}"),
                                "arrow-down",
                                enabled && index + 1 < profiles.len(),
                                palette.foreground_muted,
                                window,
                            )
                            .on_mouse_down(MouseButton::Left, {
                                let last = index + 1 == profiles.len();
                                cx.listener(move |this, _, window, cx| {
                                    if enabled && !last {
                                        this.submit(
                                            Change::Move {
                                                id: down.clone(),
                                                offset: 1,
                                            },
                                            None,
                                            window,
                                            cx,
                                        );
                                    }
                                })
                            }),
                        )
                        .child(
                            self.icon_button(
                                format!("profile-edit-{id}"),
                                "pencil",
                                enabled,
                                palette.foreground_muted,
                                window,
                            )
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, window, cx| {
                                    if enabled {
                                        this.open_editor(Some(edit.clone()), window, cx);
                                    }
                                }),
                            ),
                        )
                        .child(
                            self.icon_button(
                                format!("profile-remove-{id}"),
                                "trash-2",
                                enabled,
                                palette.destructive,
                                window,
                            )
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, window, cx| {
                                    if enabled {
                                        this.remove(remove.clone(), window, cx);
                                    }
                                }),
                            ),
                        ),
                );
                card = card.child(row);
            }
        } else {
            card = card.child(div().p_4().child(if self.state.loading() {
                "Loading..."
            } else {
                ""
            }));
        }
        let column = div()
            .w_full()
            .max_w(px(720.))
            .p_6()
            .flex()
            .flex_col()
            .child(
                div()
                    .ml_1()
                    .mb_3()
                    .text_size(px(12.))
                    .text_color(rgb(palette.foreground_muted))
                    .child("Terminal agents"),
            )
            .child(hook_card)
            .child(
                div()
                    .mt_6()
                    .mb_3()
                    .ml_1()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(palette.foreground_muted))
                            .child(self.t("settings.host.terminalProfiles.sectionTitle")),
                    )
                    .child(
                        self.icon_button(
                            "add-terminal-profile".into(),
                            "plus",
                            enabled,
                            palette.foreground_muted,
                            window,
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, window, cx| {
                                if enabled {
                                    this.open_editor(None, window, cx);
                                }
                            }),
                        ),
                    ),
            )
            .child(card)
            .when_some(
                self.state.error.clone().filter(|_| self.editor.is_none()),
                |v, error| {
                    v.child(
                        div()
                            .mt_3()
                            .text_color(rgb(palette.status_danger))
                            .child(error),
                    )
                },
            )
            .when(
                self.state.needs_reload || self.state.profiles.is_none() && !self.state.loading(),
                |v| {
                    v.child(
                        div()
                            .mt_3()
                            .id("reload-terminal-profiles")
                            .cursor_pointer()
                            .child(self.t("common.actions.retry"))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| this.load(cx)),
                            ),
                    )
                },
            );
        div()
            .relative()
            .size_full()
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .bg(rgb(palette.surface0))
            .text_color(rgb(palette.foreground))
            .line_height(px(20.))
            .on_key_down(cx.listener(Self::key_down))
            .child(header)
            .child(
                div()
                    .id("host-terminals-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .justify_center()
                    .child(column),
            )
    }
}

impl Drop for HostTerminals {
    fn drop(&mut self) {
        self.connection
            .unsubscribe("terminal-profile-settings-events");
    }
}

use crate::theme::{colors as c, font_size};
use gpui::{
    App, Bounds, ClipboardItem, Context, ElementInputHandler, EntityInputHandler, EventEmitter,
    FocusHandle, Focusable, MouseButton, Pixels, Point, Render, ShapedLine, TextRun,
    UTF16Selection, Window, canvas, div, fill, font, point, prelude::*, px, rgb, size,
};
use paseo_desktop::{
    client::Connection,
    editing::Editing,
    model::Project,
    workspace_lifecycle::{Lifecycle, Operation, Phase, request_kind, workspace_source},
};
use serde_json::{Value, json};
use std::{ops::Range, sync::Arc};

struct TextField {
    editing: Editing,
    focus: FocusHandle,
    bounds: Bounds<Pixels>,
    line: Option<ShapedLine>,
    scroll_x: Pixels,
    placeholder: String,
    enabled: bool,
}
impl Focusable for TextField {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
impl TextField {
    fn new(value: &str, placeholder: &str, cx: &mut Context<Self>) -> Self {
        let mut editing = Editing::default();
        editing.replace(None, value, false, None);
        Self {
            editing,
            focus: cx.focus_handle(),
            bounds: Bounds::default(),
            line: None,
            scroll_x: px(0.),
            placeholder: placeholder.into(),
            enabled: true,
        }
    }
    fn edited(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.notify_text_input_changed(&self.focus);
        cx.notify();
    }
    fn key_down(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.enabled || self.editing.marked.is_some() {
            return;
        }
        let key = &event.keystroke;
        let shortcut = if cfg!(target_os = "macos") {
            key.modifiers.platform
        } else {
            key.modifiers.control
        };
        if shortcut && !key.modifiers.alt {
            match key.key.as_str() {
                "a" => {
                    self.editing.select(0, false);
                    self.editing.select(self.editing.text.len(), true);
                }
                "c" | "x" => {
                    if !self.editing.selection.is_empty() {
                        cx.write_to_clipboard(ClipboardItem::new_string(
                            self.editing.text[self.editing.selection.clone()].into(),
                        ));
                        if key.key == "x" {
                            self.editing.replace(None, "", false, None);
                        }
                    }
                }
                "v" => {
                    if let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) {
                        self.editing
                            .replace(None, &text.replace(['\r', '\n'], " "), false, None);
                    }
                }
                _ => return,
            }
            self.edited(window, cx);
        } else if key.modifiers.control || key.modifiers.platform || key.modifiers.alt {
            return;
        } else {
            match key.key.as_str() {
                "backspace" | "delete" => {
                    self.editing.delete(key.key == "delete");
                    self.edited(window, cx);
                }
                "left" | "right" => {
                    self.editing
                        .horizontal(key.key == "right", key.modifiers.shift);
                    self.edited(window, cx);
                }
                "home" | "end" => {
                    self.editing.select(
                        if key.key == "home" {
                            0
                        } else {
                            self.editing.text.len()
                        },
                        key.modifiers.shift,
                    );
                    self.edited(window, cx);
                }
                _ => return,
            }
        }
        cx.stop_propagation();
        window.prevent_default();
    }
    fn caret(&self, byte: usize) -> Point<Pixels> {
        self.bounds.origin
            + point(
                self.line
                    .as_ref()
                    .map_or(px(0.), |line| line.x_for_index(byte))
                    - self.scroll_x,
                px(0.),
            )
    }
}
impl EntityInputHandler for TextField {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.editing.byte_range(range);
        *actual = Some(self.editing.utf16_range(range.clone()));
        Some(self.editing.text[range].into())
    }
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.editing.utf16_range(self.editing.selection.clone()),
            reversed: self.editing.reversed,
        })
    }
    fn set_selected_text_range(
        &mut self,
        range: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.editing.selection = self.editing.byte_range(range);
        self.editing.reversed = false;
        self.edited(window, cx);
    }
    fn text_length_utf16(&mut self, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        Some(self.editing.text.encode_utf16().count())
    }
    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.editing
            .marked
            .clone()
            .map(|r| self.editing.utf16_range(r))
    }
    fn unmark_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editing.marked = None;
        self.edited(window, cx);
    }
    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.enabled {
            return;
        }
        self.editing
            .replace(range, &text.replace(['\r', '\n'], " "), false, None);
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
        if !self.enabled {
            return;
        }
        self.editing.replace(range, text, true, selected);
        self.edited(window, cx);
    }
    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        Some(Bounds::new(
            self.caret(self.editing.byte_range(range).start),
            size(px(1.), px(20.)),
        ))
    }
    fn character_index_for_point(
        &mut self,
        position: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        Some(
            self.editing.utf16_offset(
                self.line
                    .as_ref()?
                    .closest_index_for_x(position.x - self.bounds.left() + self.scroll_x),
            ),
        )
    }
}

impl Render for TextField {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let measure = cx.entity();
        let paint = cx.entity();
        div()
            .h(px(32.))
            .w_full()
            .px_2()
            .flex()
            .items_center()
            .rounded(px(6.))
            .border_1()
            .border_color(rgb(c::BORDER))
            .bg(rgb(c::SURFACE0))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key_down))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.focus.focus(window, cx);
                    let index = this.line.as_ref().map_or(0, |line| {
                        line.closest_index_for_x(
                            event.position.x - this.bounds.left() + this.scroll_x,
                        )
                    });
                    this.editing.select(index, event.modifiers.shift);
                    this.edited(window, cx);
                }),
            )
            .child(
                canvas(
                    move |bounds, window, cx| {
                        measure.update(cx, |this, _| {
                            this.bounds = bounds;
                            let empty = this.editing.text.is_empty();
                            let text = if empty {
                                this.placeholder.clone()
                            } else {
                                this.editing.text.clone()
                            };
                            let run = TextRun {
                                len: text.len(),
                                font: font(crate::fonts::ui_family()),
                                color: rgb(if empty {
                                    c::FOREGROUND_MUTED
                                } else {
                                    c::FOREGROUND
                                })
                                .into(),
                                background_color: None,
                                underline: None,
                                strikethrough: None,
                            };
                            this.line = Some(window.text_system().shape_line(
                                text.into(),
                                px(crate::theme::font_size::BASE),
                                &[run],
                                None,
                            ));
                            let x = this
                                .line
                                .as_ref()
                                .unwrap()
                                .x_for_index(this.editing.cursor());
                            this.scroll_x = this
                                .scroll_x
                                .max(x - bounds.size.width + px(1.))
                                .min(x)
                                .max(px(0.));
                        })
                    },
                    move |bounds, (), window, cx| {
                        paint.update(cx, |this, cx| {
                            window.handle_input(
                                &this.focus,
                                ElementInputHandler::new(bounds, paint.clone()),
                                cx,
                            );
                            window.with_content_mask(
                                Some(gpui::ContentMask { bounds }),
                                |window| {
                                    if let Some(line) = &this.line {
                                        if !this.editing.selection.is_empty() {
                                            window.paint_quad(fill(
                                                Bounds::new(
                                                    this.caret(this.editing.selection.start),
                                                    size(
                                                        line.x_for_index(
                                                            this.editing.selection.end,
                                                        ) - line.x_for_index(
                                                            this.editing.selection.start,
                                                        ),
                                                        px(20.),
                                                    ),
                                                ),
                                                rgb(c::SURFACE3),
                                            ));
                                        }
                                        let _ = line.paint(
                                            bounds.origin - point(this.scroll_x, px(0.)),
                                            px(20.),
                                            gpui::TextAlign::Left,
                                            None,
                                            window,
                                            cx,
                                        );
                                        if let Some(mark) = &this.editing.marked {
                                            window.paint_quad(fill(
                                                Bounds::new(
                                                    this.caret(mark.start) + point(px(0.), px(19.)),
                                                    size(
                                                        line.x_for_index(mark.end)
                                                            - line.x_for_index(mark.start),
                                                        px(1.),
                                                    ),
                                                ),
                                                rgb(c::FOREGROUND),
                                            ));
                                        }
                                        if this.focus.is_focused(window) {
                                            window.paint_quad(fill(
                                                Bounds::new(
                                                    this.caret(this.editing.cursor()),
                                                    size(px(1.), px(20.)),
                                                ),
                                                rgb(c::FOREGROUND),
                                            ));
                                        }
                                    }
                                },
                            );
                        })
                    },
                )
                .flex_1()
                .h(px(20.)),
            )
    }
}

#[derive(Clone, Debug)]
pub enum FormMode {
    NewWorkspace,
    AddProject,
    CreateDirectory,
    /// Empty workspace id opens manual entry; a known id (e.g. from agent
    /// history) is inspected immediately.
    Recover {
        workspace_id: String,
    },
    Archive {
        id: String,
        name: String,
    },
}
pub struct Completed {
    pub operation: Operation,
    pub payload: Value,
}
pub struct Dismiss;
pub struct OpenForm(pub FormMode);
pub struct WorkspaceForm {
    connection: Arc<Connection>,
    pub state: Lifecycle,
    mode: FormMode,
    projects: Vec<Project>,
    project_id: Option<String>,
    worktree: bool,
    path: gpui::Entity<TextField>,
    name: gpui::Entity<TextField>,
    branch: gpui::Entity<TextField>,
    workspace_id: gpui::Entity<TextField>,
    idempotency: String,
}
impl EventEmitter<Completed> for WorkspaceForm {}
impl EventEmitter<Dismiss> for WorkspaceForm {}
impl EventEmitter<OpenForm> for WorkspaceForm {}
impl WorkspaceForm {
    pub fn new(
        mode: FormMode,
        connection: Arc<Connection>,
        projects: Vec<Project>,
        path: &str,
        cx: &mut Context<Self>,
    ) -> Self {
        let prefilled = match &mode {
            FormMode::Recover { workspace_id } => workspace_id.trim().to_owned(),
            _ => String::new(),
        };
        Self {
            connection,
            state: Lifecycle::default(),
            mode,
            projects,
            project_id: None,
            worktree: false,
            path: cx.new(|cx| TextField::new(path, "Directory path", cx)),
            name: cx.new(|cx| TextField::new("", "Name", cx)),
            branch: cx.new(|cx| TextField::new("", "New branch", cx)),
            workspace_id: cx.new(|cx| TextField::new(&prefilled, "Workspace ID", cx)),
            idempotency: uuid::Uuid::new_v4().to_string(),
        }
    }
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        let focus = if matches!(self.mode, FormMode::Recover { .. }) {
            self.workspace_id.read(cx).focus.clone()
        } else {
            self.path.read(cx).focus.clone()
        };
        focus.focus(window, cx);
    }
    /// A recovery entry carrying a known workspace id inspects it right away;
    /// manual entry waits for the user.
    pub fn begin(&mut self, cx: &mut Context<Self>) {
        if let FormMode::Recover { workspace_id } = &self.mode
            && !workspace_id.trim().is_empty()
        {
            self.submit(cx);
        }
    }
    pub fn disconnected(&mut self, cx: &mut Context<Self>) {
        if matches!(self.state.phase, Phase::Pending { .. }) {
            self.state.fail(
                "Host disconnected. This operation may have completed; check before retrying."
                    .into(),
            );
        }
        cx.notify();
    }
    pub fn owns(&self, payload: &Value) -> bool {
        self.state.owns(payload)
    }
    pub fn receive(&mut self, kind: &str, payload: &Value, cx: &mut Context<Self>) {
        let operation = match &self.state.phase {
            Phase::Pending { operation, .. } => operation.clone(),
            _ => return,
        };
        match self.state.receive(kind, payload) {
            Ok(_) => {
                if self.state.phase == Phase::Complete {
                    cx.emit(Completed {
                        operation,
                        payload: payload.clone(),
                    });
                }
            }
            Err(error) => self.state.fail(error.to_string()),
        };
        cx.notify();
    }
    fn request(&mut self, operation: Operation, payload: Value, cx: &mut Context<Self>) {
        if let Err(error) = self.state.admit() {
            self.state.fail(error.to_string());
            cx.notify();
            return;
        }
        match self
            .connection
            .request(request_kind(&operation), payload.clone())
        {
            Ok(id) => {
                self.state.begin(id.clone(), operation, payload);
                let timer = cx
                    .background_executor()
                    .timer(std::time::Duration::from_secs(150));
                cx.spawn(async move |this, cx| {
                    timer.await;
                    let _ = this.update(cx, |this, cx| {
                        if matches!(&this.state.phase,Phase::Pending{id:pending,..} if pending==&id)
                        {
                            this.state.expire(
                                &id,
                                "Operation timed out. Check the host state before retrying.".into(),
                            );
                            cx.notify();
                        }
                    });
                })
                .detach();
            }
            Err(error) => self.state.fail(error.to_string()),
        };
        cx.notify();
    }
    fn submit(&mut self, cx: &mut Context<Self>) {
        let path = self.path.read(cx).editing.text.clone();
        let name = self.name.read(cx).editing.text.clone();
        let branch = self.branch.read(cx).editing.text.clone();
        let workspace_id = self.workspace_id.read(cx).editing.text.trim().to_owned();
        let request: Result<(Operation, Value), anyhow::Error> = match &self.mode {
            FormMode::NewWorkspace => {
                // The app names a worktree after its branch and a local
                // directory after its folder when no explicit name is given.
                let title = if name.trim().is_empty() {
                    if self.worktree && !branch.trim().is_empty() {
                        branch.trim().to_owned()
                    } else {
                        std::path::Path::new(path.trim())
                            .file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or_default()
                            .to_owned()
                    }
                } else {
                    name.trim().to_owned()
                };
                workspace_source(
                    &path,
                    self.project_id.as_deref(),
                    self.worktree,
                    &branch,
                )
                .map(|source| {
                    (
                        Operation::CreateWorkspace,
                        json!({"source":source,"title":title,"idempotencyKey":self.idempotency}),
                    )
                })
            }
            FormMode::AddProject => Ok((Operation::AddProject, json!({"cwd":path.trim()}))),
            FormMode::CreateDirectory => Ok((
                Operation::CreateDirectory,
                json!({"parentPath":path.trim(),"name":name.trim()}),
            )),
            FormMode::Recover { .. } => {
                if self.state.recovery.as_ref().is_some_and(|recovery| {
                    recovery["workspaceId"].as_str() == Some(workspace_id.as_str())
                        && recovery["kind"] == "recoverable"
                }) {
                    self.state.restore_request().map(|payload| {
                        (
                            Operation::Restore(
                                payload["workspaceId"].as_str().unwrap_or_default().into(),
                            ),
                            payload,
                        )
                    })
                } else if workspace_id.is_empty() {
                    Err(anyhow::anyhow!("Enter a workspace ID"))
                } else {
                    self.state.recovery = None;
                    Ok((
                        Operation::Inspect(workspace_id.clone()),
                        json!({"workspaceId":workspace_id}),
                    ))
                }
            }
            FormMode::Archive { id, .. } => {
                Ok((Operation::Archive(id.clone()), json!({"workspaceId":id})))
            }
        };
        match request {
            Ok((operation, payload)) => self.request(operation, payload, cx),
            Err(error) => {
                self.state.fail(error.to_string());
                cx.notify();
            }
        }
    }
    fn button(
        &self,
        id: &'static str,
        label: &str,
        active: bool,
        cx: &mut Context<Self>,
        handler: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> gpui::AnyElement {
        let label = label.to_owned();
        div()
            .id(id)
            .h(px(32.))
            .px_3()
            .rounded(px(6.))
            .flex()
            .items_center()
            .justify_center()
            .bg(rgb(if active { c::SURFACE3 } else { c::SURFACE2 }))
            .cursor_pointer()
            .child(label)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    handler(this, cx);
                }),
            )
            .into_any_element()
    }
}
impl Render for WorkspaceForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pending = matches!(self.state.phase, Phase::Pending { .. });
        // A selected project owns the directory: the app derives it from the
        // project root instead of letting the user edit both.
        let path_editable = self.project_id.is_none();
        self.path
            .update(cx, |field, _| field.enabled = !pending && path_editable);
        for field in [&self.name, &self.branch, &self.workspace_id] {
            field.update(cx, |field, _| field.enabled = !pending);
        }
        let title = match &self.mode {
            FormMode::NewWorkspace => "New workspace",
            FormMode::AddProject => "Add project",
            FormMode::CreateDirectory => "New directory",
            FormMode::Recover { .. } => "Recover workspace",
            FormMode::Archive { .. } => "Archive workspace",
        };
        let mut body = div().flex().flex_col().gap_3().w_full();
        if matches!(self.mode, FormMode::NewWorkspace) {
            body = body
                .child(div().child("Project"))
                // The app keeps a project-free path: the user types a fresh
                // directory instead of extending an existing project.
                .child(
                    div()
                        .id("project-none")
                        .h(px(32.))
                        .px_2()
                        .rounded(px(6.))
                        .cursor_pointer()
                        .child("New directory (no project)")
                        .when(self.project_id.is_none(), |row| row.bg(rgb(c::SURFACE2)))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                if !matches!(this.state.phase, Phase::Pending { .. }) {
                                    this.project_id = None;
                                    this.worktree = false;
                                    cx.notify();
                                }
                            }),
                        ),
                )
                .children(self.projects.iter().enumerate().map(|(i, project)| {
                    let id = project.project_id.clone();
                    let path = project.project_root_path.clone();
                    div()
                        .id(("project-choice", i))
                        .h(px(32.))
                        .px_2()
                        .rounded(px(6.))
                        .cursor_pointer()
                        .child(project.project_display_name.clone())
                        .when(
                            self.project_id.as_ref() == Some(&project.project_id),
                            |row| row.bg(rgb(c::SURFACE2)),
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                if !matches!(this.state.phase, Phase::Pending { .. }) {
                                    this.project_id = Some(id.clone());
                                    this.worktree = false;
                                    this.path.update(cx, |field, _| {
                                        field.editing.replace(
                                            Some(0..field.editing.text.encode_utf16().count()),
                                            &path,
                                            false,
                                            None,
                                        )
                                    });
                                    cx.notify();
                                }
                            }),
                        )
                }));
            if self.projects.iter().any(|project| {
                self.project_id.as_ref() == Some(&project.project_id)
                    && project.project_kind.as_deref() == Some("git")
            }) {
                body = body.child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            self.button("local", "Local", !self.worktree, cx, |this, cx| {
                                if !matches!(this.state.phase, Phase::Pending { .. }) {
                                    this.worktree = false;
                                    cx.notify();
                                }
                            }),
                        )
                        .child(self.button(
                            "worktree",
                            "New worktree",
                            self.worktree,
                            cx,
                            |this, cx| {
                                if !matches!(this.state.phase, Phase::Pending { .. }) {
                                    this.worktree = true;
                                    cx.notify();
                                }
                            },
                        )),
                );
            }
        }
        if !matches!(
            self.mode,
            FormMode::Recover { .. } | FormMode::Archive { .. }
        ) {
            body = body
                .child(
                    div().child(if matches!(self.mode, FormMode::CreateDirectory) {
                        "Parent directory"
                    } else {
                        "Directory"
                    }),
                )
                .child(self.path.clone());
        }
        if matches!(
            self.mode,
            FormMode::NewWorkspace | FormMode::CreateDirectory
        ) {
            body = body.child(div().child("Name")).child(self.name.clone());
        }
        if self.worktree && matches!(self.mode, FormMode::NewWorkspace) {
            body = body.child(div().child("Branch")).child(self.branch.clone());
        }
        if matches!(self.mode, FormMode::Recover { .. }) {
            body = body
                .child(div().child("Workspace ID"))
                .child(self.workspace_id.clone());
            if let Some(recovery) = &self.state.recovery {
                body = body.child(
                    div().child(
                        recovery["workspaceName"]
                            .as_str()
                            .or_else(|| recovery["message"].as_str())
                            .unwrap_or("Recovery state unavailable")
                            .to_owned(),
                    ),
                );
            }
        }
        if let FormMode::Archive { name, .. } = &self.mode {
            body =
                body.child(div().child(format!("Archive {name}?")))
                    .child(div().text_color(rgb(c::FOREGROUND_MUTED)).child(
                    "The host closes this workspace's sessions. Other workspaces are unchanged.",
                ));
        }
        if let Phase::Failed(error) = &self.state.phase {
            body = body.child(div().text_color(rgb(c::STATUS_DANGER)).child(error.clone()));
        }
        if self.state.phase == Phase::Complete {
            body = body.child(div().text_color(rgb(c::STATUS_SUCCESS)).child("Completed"));
        }
        let label = if pending {
            "Working…"
        } else if matches!(self.mode, FormMode::Recover { .. })
            && self
                .state
                .recovery
                .as_ref()
                .is_some_and(|recovery| recovery["kind"] == "recoverable")
        {
            "Restore"
        } else if matches!(self.state.phase, Phase::Failed(_)) {
            "Retry"
        } else {
            match self.mode {
                FormMode::NewWorkspace => "Create workspace",
                FormMode::AddProject => "Add project",
                FormMode::CreateDirectory => "Create directory",
                FormMode::Recover { .. } => "Inspect",
                FormMode::Archive { .. } => "Archive workspace",
            }
        };
        if matches!(self.mode, FormMode::NewWorkspace | FormMode::AddProject) {
            body = body.child(self.button(
                "new-directory",
                "New directory…",
                false,
                cx,
                |this, cx| {
                    if !matches!(this.state.phase, Phase::Pending { .. }) {
                        cx.emit(OpenForm(FormMode::CreateDirectory));
                    }
                },
            ));
        }
        body = body.child(
            div()
                .flex()
                .gap_2()
                .child(self.button("submit", label, false, cx, |this, cx| {
                    if !matches!(this.state.phase, Phase::Pending { .. })
                        && (!matches!(this.state.phase, Phase::Complete)
                            || matches!(this.state.last_request, Some((Operation::Inspect(_), _))))
                    {
                        this.submit(cx);
                    }
                }))
                .child(self.button("close", "Close", false, cx, |this, cx| {
                    if !matches!(this.state.phase, Phase::Pending { .. }) {
                        cx.emit(Dismiss);
                    }
                })),
        );
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgb(c::SURFACE_WORKSPACE))
            .child(
                div()
                    .w(px(520.))
                    .max_w_full()
                    .p_4()
                    .text_size(px(font_size::BASE))
                    .text_color(rgb(c::FOREGROUND))
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(div().text_size(px(font_size::LG)).child(title))
                    .child(body),
            )
    }
}

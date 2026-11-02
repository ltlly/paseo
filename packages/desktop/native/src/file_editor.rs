use std::{ops::Range, sync::Arc, time::Duration};

use anyhow::Context as _;
use gpui::{
    App, Bounds, ClipboardItem, Context, ElementInputHandler, EntityInputHandler, EventEmitter,
    FocusHandle, Focusable, KeyDownEvent, MouseButton, Pixels, Point, Render, TextAlign, TextRun,
    UTF16Selection, UnderlineStyle, Window, canvas, div, fill, font, point, prelude::*, px, rgb,
    size,
};
use paseo_desktop::client::Connection;
use paseo_desktop::editing::Editing;
use serde_json::Value as Json;
use serde_json::{Value, json};

use crate::{
    chrome::icon,
    theme::{colors as c, font_size, radius},
};

const LINE_HEIGHT: f32 = 21.;
const AUTOSAVE_DELAY_MS: u64 = 800;
const GUTTER_WIDTH: f32 = 56.;
/// The host refuses writes past this size, so reads ask for the same bound.
const MAX_EDITABLE_FILE_BYTES: i64 = 1024 * 1024;

/// The user resolved an unsaved-changes confirmation; the host tab should go away.
pub struct CloseRequested;

impl EventEmitter<CloseRequested> for FileEditor {}

/// Which side of the disk round-trip the editor is on. Mirrors the original
/// file editor states: a clean buffer follows the disk, a modified one
/// conflicts when the disk moves underneath it.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Status {
    Loading,
    Failed,
    Clean,
    Dirty,
    Saving,
    Conflict { deleted: bool },
}

// A host update can arrive before the queued autosave fires. Only the
// explicit Overwrite control may use the new host version with local text.
fn permits_save(status: Status, overwrite: bool) -> bool {
    status == Status::Dirty || (overwrite && status == Status::Conflict { deleted: false })
}

struct Line {
    start: usize,
    top: Pixels,
    height: Pixels,
    gutter: Option<gpui::ShapedLine>,
    shaped: gpui::WrappedLine,
}

pub struct FileEditor {
    connection: Arc<Connection>,
    cwd: String,
    path: String,
    focus: FocusHandle,
    editing: Editing,
    /// Separator the file uses on disk; edits use `\n` internally.
    separator: &'static str,
    status: Status,
    modified_at: Option<String>,
    revision: Option<String>,
    error: Option<String>,
    /// The in-memory buffer differs from the last read or saved version.
    dirty: bool,
    save_request: Option<String>,
    /// The buffer text the in-flight write carries, so an edit typed during the
    /// round trip keeps the tab dirty when that write acknowledges.
    save_snapshot: Option<String>,
    load_request: Option<String>,
    /// The host-bound subscription request awaiting its response.
    subscribe_request: Option<String>,
    /// Set when the user asked to close after the in-flight save lands.
    close_after_save: bool,
    /// The next layout should bring the caret back into view (typed/navigated).
    pending_scroll: bool,
    /// Subscriptions bookkeeping key, present while the host subscription is open.
    subscription: Option<String>,
    connected: bool,
    autosave: Option<gpui::Task<()>>,
    lines: Vec<Line>,
    content_height: Pixels,
    bounds: Bounds<Pixels>,
    scroll: Pixels,
    scroll_x: Pixels,
    selecting: bool,
    find: Option<Find>,
    find_line: Option<gpui::ShapedLine>,
    find_scroll_x: Pixels,
    find_focused: bool,
    closing: bool,
}

struct Find {
    query: Editing,
    matches: Vec<Range<usize>>,
    current: usize,
    focus: FocusHandle,
}

impl Find {
    fn run(&mut self, text: &str) {
        self.matches = find_matches(text, &self.query.text);
        if self.current >= self.matches.len() {
            self.current = self.matches.len().checked_sub(1).unwrap_or(0);
        }
    }
}

/// Case-insensitive literal matches as byte ranges in the original text.
///
/// Folding is ASCII-only over the original bytes: Unicode case folding changes
/// byte lengths (İ, ẞ), which would shift every offset. Multi-byte characters
/// therefore match byte-exactly, and matches never start inside a character.
fn find_matches(text: &str, query: &str) -> Vec<Range<usize>> {
    fn fold(byte: u8) -> u8 {
        byte.to_ascii_lowercase()
    }
    let needle: Vec<u8> = query.as_bytes().iter().map(|byte| fold(*byte)).collect();
    let mut matches = Vec::new();
    let haystack = text.as_bytes();
    if !needle.is_empty() && needle.len() <= haystack.len() {
        let mut start = 0;
        while start + needle.len() <= haystack.len() {
            if !text.is_char_boundary(start) {
                start += 1;
                continue;
            }
            if haystack[start..start + needle.len()]
                .iter()
                .map(|byte| fold(*byte))
                .eq(needle.iter().copied())
            {
                matches.push(start..start + needle.len());
                start += needle.len();
            } else {
                start += 1;
            }
        }
    }
    matches
}

/// The host-assigned subscription id from a subscribe response.
fn subscription_from_response(payload: &Json) -> Option<String> {
    payload["subscriptionId"].as_str().map(str::to_owned)
}

/// Whether a pushed update belongs to this editor's bound subscription.
fn update_targets(subscription: Option<&str>, payload: &Json) -> bool {
    payload["subscriptionId"].as_str() == subscription
}

/// The local bookkeeping key for one file subscription. It carries the
/// workspace, so the same relative path in two workspaces is two host watchers.
fn file_subscription_key(cwd: &str, path: &str) -> String {
    format!("file:{cwd}:{path}")
}

/// A disconnect ends every round trip. A save that was in flight can never
/// acknowledge, so its dirty buffer has to become editable and savable again
/// instead of staying in `Saving`, which no guard lets the user leave.
fn status_after_disconnect(status: Status) -> Status {
    match status {
        Status::Loading => Status::Failed,
        Status::Saving => Status::Dirty,
        status => status,
    }
}

/// Whether the buffer still holds text the acknowledged write did not carry.
fn unsaved_after_write(buffer: &str, sent: Option<&str>) -> bool {
    sent.is_none_or(|sent| sent != buffer)
}

/// Write fields with a real compare-and-swap marker. An absent marker refuses
/// the write instead of sending `""`, which the host can never match.
fn write_fields(
    cwd: &str,
    path: &str,
    content: &str,
    modified_at: Option<&str>,
    revision: Option<&str>,
) -> anyhow::Result<Json> {
    let mut fields = json!({
        "cwd":cwd,
        "path":path,
        "content":content,
    });
    if let Some(revision) = revision {
        fields["expectedRevision"] = json!(revision);
        fields["expectedModifiedAt"] =
            json!(modified_at.context("write without a known version marker")?);
    } else {
        fields["expectedModifiedAt"] =
            json!(modified_at.context("write without a known version marker")?);
    }
    Ok(fields)
}

/// The outcome shape of `fs.file.write.response.result`.
enum WriteOutcome {
    Written,
    Conflict { deleted: bool },
    Error(String),
}

fn write_outcome(result: &Json) -> WriteOutcome {
    match result["status"].as_str() {
        Some("written") => WriteOutcome::Written,
        Some("conflict") => WriteOutcome::Conflict {
            deleted: version_kind(&result["version"]) == VersionKind::Missing,
        },
        _ => WriteOutcome::Error(
            result["error"]
                .as_str()
                .unwrap_or("The host refused the write.")
                .to_owned(),
        ),
    }
}

#[derive(Clone, Copy, PartialEq)]
enum VersionKind {
    Ready,
    Missing,
}

fn version_kind(version: &Json) -> VersionKind {
    match version["status"].as_str() {
        Some("missing") => VersionKind::Missing,
        _ => VersionKind::Ready,
    }
}

impl FileEditor {
    pub fn new(
        cwd: String,
        path: String,
        connection: Arc<Connection>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self {
            connection,
            cwd,
            path,
            focus: cx.focus_handle(),
            editing: Editing::default(),
            separator: "\n",
            status: Status::Loading,
            modified_at: None,
            revision: None,
            error: None,
            dirty: false,
            save_request: None,
            save_snapshot: None,
            load_request: None,
            subscribe_request: None,
            close_after_save: false,
            pending_scroll: false,
            subscription: None,
            connected: true,
            autosave: None,
            lines: Vec::new(),
            content_height: px(0.),
            bounds: Bounds::default(),
            scroll: px(0.),
            scroll_x: px(0.),
            selecting: false,
            find: None,
            find_line: None,
            find_scroll_x: px(0.),
            find_focused: false,
            closing: false,
        };
        view.load(cx);
        view
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty || self.status == Status::Saving
    }

    pub fn title(&self) -> String {
        self.path
            .rsplit('/')
            .next()
            .unwrap_or(&self.path)
            .to_owned()
    }

    pub fn disconnected(&mut self, cx: &mut Context<Self>) {
        self.connected = false;
        self.subscription = None;
        self.subscribe_request = None;
        self.save_request = None;
        self.save_snapshot = None;
        self.load_request = None;
        self.close_after_save = false;
        let saving = matches!(self.status, Status::Saving);
        self.status = status_after_disconnect(self.status);
        if self.status == Status::Failed {
            self.error = Some("Host is disconnected.".into());
        } else if saving {
            // The write can never acknowledge, so the buffer stays unsaved and
            // must become editable again instead of sitting in Saving forever.
            self.error = Some("The host disconnected before the save was acknowledged.".into());
        }
        cx.notify();
    }

    /// The connection is live again; its subscriptions are not. Every editor
    /// resubscribes so disk movement still reaches it; only a clean buffer also
    /// re-reads content, a dirty one keeps its local text.
    pub fn reconnected(&mut self, cx: &mut Context<Self>) {
        self.connected = true;
        self.error = None;
        self.subscription = None;
        self.subscribe_request = None;
        self.resubscribe();
        if !self.dirty {
            self.fetch_latest();
        }
        cx.notify();
    }

    /// Open the host file subscription; the response binds the host id.
    fn resubscribe(&mut self) {
        if self.subscribe_request.is_none() {
            match self.connection.subscribe(
                &self.subscription_key(),
                "fs.file.subscribe.request",
                json!({"cwd":self.cwd,"path":self.path}),
            ) {
                Ok(id) => self.subscribe_request = Some(id),
                Err(error) => self.error = Some(error.to_string()),
            }
        }
    }

    /// Called when the tab is closed. Releases the host file subscription.
    pub fn closed(&mut self) {
        self.subscribe_request = None;
        self.subscription = None;
        // `Connection::unsubscribe` releases by the local bookkeeping key; the
        // host-assigned id is not a key, so passing it releases nothing and
        // leaks the host watcher for the life of the connection.
        self.connection.unsubscribe(&self.subscription_key());
    }

    fn subscription_key(&self) -> String {
        file_subscription_key(&self.cwd, &self.path)
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        match self.connection.request(
            "file_explorer_request",
            json!({
                "cwd":self.cwd,
                "path":self.path,
                "mode":"file",
                "maxBytes":MAX_EDITABLE_FILE_BYTES,
            }),
        ) {
            Ok(id) => self.load_request = Some(id),
            Err(error) => {
                self.status = Status::Failed;
                self.error = Some(error.to_string());
            }
        }
        self.resubscribe();
        cx.notify();
    }

    /// Whether this editor answers a pending reply or failure.
    pub fn owns_request(&self, request_id: &str) -> bool {
        self.load_request.as_deref() == Some(request_id)
            || self.save_request.as_deref() == Some(request_id)
    }

    pub fn receive(&mut self, kind: &str, payload: &Value, cx: &mut Context<Self>) -> bool {
        if let Some(request_id) = payload["requestId"].as_str() {
            if kind == "fs.file.subscribe.response"
                && self.subscribe_request.as_deref() == Some(request_id)
            {
                self.subscribe_request = None;
                match subscription_from_response(payload) {
                    Some(id) => self.subscription = Some(id),
                    None => {
                        self.error = Some("The host did not bind the file subscription.".into())
                    }
                }
                cx.notify();
                return true;
            }
            if kind == "fs.file.write.response" && self.save_request.as_deref() == Some(request_id)
            {
                self.save_request = None;
                match write_outcome(&payload["result"]) {
                    WriteOutcome::Written => {
                        let sent = self.save_snapshot.take();
                        // The write only settles the text it carried. Keystrokes
                        // typed during the round trip are still unsaved, and
                        // clearing the flag would drop them on close.
                        let unsaved = unsaved_after_write(&self.editing.text, sent.as_deref());
                        self.dirty = unsaved;
                        self.status = if unsaved {
                            Status::Dirty
                        } else {
                            Status::Clean
                        };
                        self.modified_at =
                            payload["result"]["modifiedAt"].as_str().map(str::to_owned);
                        self.revision = payload["result"]["revision"].as_str().map(str::to_owned);
                        self.error = None;
                        if self.close_after_save {
                            if unsaved {
                                // Save the newer text before the tab goes away.
                                self.save(cx);
                            } else {
                                self.close_after_save = false;
                                cx.emit(CloseRequested);
                            }
                        }
                    }
                    WriteOutcome::Conflict { deleted } => {
                        self.save_snapshot = None;
                        // The tab stays open to resolve the conflict; a later
                        // save must not close it behind the user's back.
                        self.close_after_save = false;
                        // Only explicit Overwrite uses this host version.
                        // Ordinary saves remain blocked by the conflict.
                        self.adopt_version(&payload["result"]["version"]);
                        self.status = if self.dirty {
                            Status::Conflict { deleted }
                        } else {
                            Status::Clean
                        };
                    }
                    WriteOutcome::Error(error) => {
                        self.save_snapshot = None;
                        self.close_after_save = false;
                        self.status = if self.dirty {
                            Status::Dirty
                        } else {
                            Status::Clean
                        };
                        self.error = Some(error);
                    }
                }
                cx.notify();
                return true;
            }
            if kind == "file_explorer_response" && self.load_request.as_deref() == Some(request_id)
            {
                self.load_request = None;
                if let Some(error) = payload["error"].as_str() {
                    self.status = Status::Failed;
                    self.error = Some(error.to_owned());
                    cx.notify();
                    return true;
                }
                match &payload["file"] {
                    Value::Null => {
                        self.status = Status::Failed;
                        self.error = Some("File unavailable.".into());
                    }
                    file => match self.apply_file(file) {
                        Ok(()) => {
                            self.status = Status::Clean;
                            self.dirty = false;
                            self.error = None;
                        }
                        Err(error) => {
                            self.status = Status::Failed;
                            self.error = Some(format!("{error:#}"));
                        }
                    },
                }
                cx.notify();
                return true;
            }
            if kind == "rpc_error" {
                if self.subscribe_request.as_deref() == Some(request_id) {
                    // The tab stays readable but not live, and clearing the
                    // marker lets a reconnect resubscribe instead of hanging.
                    self.subscribe_request = None;
                    self.error = Some(
                        payload["error"]
                            .as_str()
                            .unwrap_or("The host refused the file subscription.")
                            .to_owned(),
                    );
                    cx.notify();
                    return true;
                }
                if self.owns_request(request_id) {
                    let saving = self.save_request.take().is_some();
                    let loading = self.load_request.take().is_some();
                    if saving {
                        self.save_snapshot = None;
                        // A refused save leaves the tab open.
                        self.close_after_save = false;
                    }
                    self.error = payload["error"].as_str().map(str::to_owned);
                    if loading {
                        self.status = Status::Failed;
                    } else if saving {
                        self.status = if self.dirty {
                            Status::Dirty
                        } else {
                            Status::Clean
                        };
                    }
                    cx.notify();
                    return true;
                }
            }
        }
        if kind == "fs.file.update" {
            return self.receive_update(payload, cx);
        }
        false
    }

    fn receive_update(&mut self, payload: &Value, cx: &mut Context<Self>) -> bool {
        if !update_targets(self.subscription.as_deref(), payload) {
            return false;
        }
        let version = &payload["version"];
        match version["status"].as_str() {
            Some("missing") => {
                self.apply_deleted(cx);
                true
            }
            Some("error") => {
                self.error = version["error"].as_str().map(str::to_owned);
                cx.notify();
                true
            }
            Some("ready") => {
                self.modified_at = version["modifiedAt"].as_str().map(str::to_owned);
                self.revision = version["revision"].as_str().map(str::to_owned);
                if self.dirty {
                    // A dirty buffer conflicts; it only adopts the disk version
                    // through an explicit reload or a conflicting save.
                    self.status = Status::Conflict { deleted: false };
                    cx.notify();
                } else {
                    self.fetch_latest();
                }
                true
            }
            _ => false,
        }
    }

    fn fetch_latest(&mut self) {
        match self.connection.request(
            "file_explorer_request",
            json!({
                "cwd":self.cwd,
                "path":self.path,
                "mode":"file",
                "maxBytes":MAX_EDITABLE_FILE_BYTES,
            }),
        ) {
            Ok(id) => self.load_request = Some(id),
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    fn apply_deleted(&mut self, cx: &mut Context<Self>) {
        if self.dirty {
            self.status = Status::Conflict { deleted: true };
        } else {
            self.status = Status::Failed;
            self.error = Some("This file was deleted on the host.".into());
            self.editing.text = String::new();
        }
        cx.notify();
    }

    fn apply_file(&mut self, file: &Value) -> anyhow::Result<()> {
        if file["kind"].as_str() == Some("binary") || file["encoding"].as_str() != Some("utf-8") {
            anyhow::bail!("This file is not UTF-8 text. Paseo edits text files.");
        }
        let content = file["content"]
            .as_str()
            .context("the host returned no file content")?;
        self.separator = if content.contains("\r\n") {
            "\r\n"
        } else if content.contains('\r') {
            "\r"
        } else {
            "\n"
        };
        self.editing.text = content.replace("\r\n", "\n").replace('\r', "\n");
        self.editing.selection = 0..0;
        self.editing.marked = None;
        self.modified_at = file["modifiedAt"].as_str().map(str::to_owned);
        self.revision = file["revision"].as_str().map(str::to_owned);
        if let Some(find) = &mut self.find {
            find.run(&self.editing.text);
        }
        Ok(())
    }

    /// Adopt a version payload from a write conflict.
    fn adopt_version(&mut self, version: &Value) {
        if version["status"].as_str() == Some("ready") {
            self.modified_at = version["modifiedAt"].as_str().map(str::to_owned);
            self.revision = version["revision"].as_str().map(str::to_owned);
        }
    }

    fn edited(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pending_scroll = true;
        if !self.dirty && !matches!(self.status, Status::Conflict { .. }) {
            self.dirty = true;
            self.status = Status::Dirty;
        } else if !self.dirty {
            self.dirty = true;
        }
        if let Some(find) = &mut self.find {
            find.run(&self.editing.text);
        }
        self.schedule_autosave(cx);
        window.notify_text_input_changed(&self.focus);
        cx.notify();
    }

    fn schedule_autosave(&mut self, cx: &mut Context<Self>) {
        let timer = cx
            .background_executor()
            .timer(Duration::from_millis(AUTOSAVE_DELAY_MS));
        self.autosave = Some(cx.spawn(async move |this, cx| {
            timer.await;
            let _ = this.update(cx, |this, cx| {
                this.autosave = None;
                if this.dirty && this.save_request.is_none() && this.connected {
                    this.save(cx);
                }
            });
        }));
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        self.save_with_intent(false, cx);
    }

    fn save_with_intent(&mut self, overwrite: bool, cx: &mut Context<Self>) {
        if !self.connected || self.save_request.is_some() || !permits_save(self.status, overwrite) {
            return;
        }
        // The snapshot records what this write carries; the acknowledgement
        // compares the buffer against it.
        let text = self.editing.text.clone();
        let content = text.replace('\n', self.separator);
        // The compare-and-swap markers are the versions the editor last read,
        // adopted from a conflict, or received from a write. Never a blank.
        let fields = match write_fields(
            &self.cwd,
            &self.path,
            &content,
            self.modified_at.as_deref(),
            self.revision.as_deref(),
        ) {
            Ok(fields) => fields,
            Err(_) => {
                self.error =
                    Some("Read the file before saving: the host version is unknown.".into());
                cx.notify();
                return;
            }
        };
        match self.connection.request("fs.file.write.request", fields) {
            Ok(id) => {
                self.save_request = Some(id);
                self.save_snapshot = Some(text);
                self.status = Status::Saving;
                let timer = cx.background_executor().timer(Duration::from_secs(30));
                cx.spawn(async move |this, cx| {
                    timer.await;
                    let _ = this.update(cx, |this, cx| {
                        if this.save_request.take().is_some() {
                            this.save_snapshot = None;
                            // No acknowledgement: the tab stays open.
                            this.close_after_save = false;
                            this.status = if this.dirty {
                                Status::Dirty
                            } else {
                                Status::Clean
                            };
                            this.error = Some(
                                "No save acknowledgement. Check the connection and retry.".into(),
                            );
                            cx.notify();
                        }
                    });
                })
                .detach();
            }
            Err(error) => {
                // Nothing was sent, so no acknowledgement will close the tab.
                self.close_after_save = false;
                self.error = Some(error.to_string());
            }
        }
        cx.notify();
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        self.fetch_latest();
        cx.notify();
    }

    /// Overwrite compares against the version the last conflict or update
    /// adopted, so the write lands unless the host moved again meanwhile.
    /// A deleted host file cannot be overwritten; only Discard resolves it.
    fn overwrite(&mut self, cx: &mut Context<Self>) {
        self.save_with_intent(true, cx);
    }

    fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.find.is_none() {
            let mut find = Find {
                query: Editing::default(),
                matches: Vec::new(),
                current: 0,
                focus: cx.focus_handle(),
            };
            find.run(&self.editing.text);
            self.find = Some(find);
        }
        self.find_focused = true;
        self.find.as_ref().unwrap().focus.clone().focus(window, cx);
        cx.notify();
    }

    fn find_step(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(find) = &mut self.find else {
            return;
        };
        if find.matches.is_empty() {
            return;
        }
        find.current = if forward {
            (find.current + 1) % find.matches.len()
        } else {
            (find.current + find.matches.len() - 1) % find.matches.len()
        };
        if let Some(found) = find.matches.get(find.current).cloned() {
            self.editing.select(found.start, false);
            self.editing.select(found.end, true);
            self.pending_scroll = true;
        }
        window.notify_text_input_changed(&self.focus);
        cx.notify();
    }

    /// A dirty tab must confirm before its buffer is dropped.
    pub fn begin_close(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.is_dirty() {
            self.closed();
            return true;
        }
        self.closing = true;
        cx.notify();
        false
    }

    /// Resolve the unsaved-changes confirmation: save first, close on success;
    /// or drop the buffer and let the host remove the tab.
    fn confirm_close_save(&mut self, cx: &mut Context<Self>) {
        if matches!(self.status, Status::Conflict { .. }) {
            self.error = Some("Resolve the file conflict before saving and closing.".into());
            cx.notify();
            return;
        }
        self.close_after_save = true;
        self.save(cx);
        if self.save_request.is_some() {
            // The write is in flight; the acknowledgement closes the tab.
            self.closing = false;
        } else {
            // `save` refused (disconnected, or no known version). Keep the
            // confirmation up instead of eating the click.
            self.close_after_save = false;
            if self.error.is_none() {
                self.error = Some("Cannot save: the host is disconnected.".into());
            }
        }
        cx.notify();
    }

    fn confirm_close_discard(&mut self, cx: &mut Context<Self>) {
        self.dirty = false;
        self.closed();
        cx.emit(CloseRequested);
        cx.notify();
    }

    fn scroll_toward(&mut self, y: Pixels) {
        if y < px(0.) {
            self.scroll += y;
        }
        if y + px(LINE_HEIGHT) > self.bounds.size.height {
            self.scroll += y + px(LINE_HEIGHT) - self.bounds.size.height;
        }
        self.clamp_scroll();
    }

    fn clamp_scroll(&mut self) {
        self.scroll = self
            .scroll
            .max(px(0.))
            .min((self.content_height - self.bounds.size.height).max(px(0.)));
        let widest = self
            .lines
            .iter()
            .map(|l| l.shaped.size(px(LINE_HEIGHT)).width)
            .fold(px(0.), Pixels::max);
        self.scroll_x = self
            .scroll_x
            .max(px(0.))
            .min((widest - self.bounds.size.width + px(GUTTER_WIDTH)).max(px(0.)));
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
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
                            .replace(None, &text.replace("\r\n", "\n"), false, None);
                    }
                }
                "s" => {
                    self.save(cx);
                }
                "f" => {
                    self.open_find(window, cx);
                    window.prevent_default();
                    cx.stop_propagation();
                    return;
                }
                _ => return,
            }
        } else if key.modifiers.control || key.modifiers.platform || key.modifiers.alt {
            return;
        } else {
            match key.key.as_str() {
                "enter" => self.editing.replace(None, "\n", false, None),
                "backspace" => self.editing.delete(false),
                "delete" => self.editing.delete(true),
                "tab" => self.editing.replace(None, "  ", false, None),
                "left" | "right" => self.editing.horizontal(key.key == "right", extend),
                "home" => {
                    let cursor = self.editing.cursor();
                    let start = self.editing.text[..cursor].rfind('\n').map_or(0, |i| i + 1);
                    self.editing.select(start, extend);
                }
                "end" => {
                    let cursor = self.editing.cursor();
                    let end = self.editing.text[cursor..]
                        .find('\n')
                        .map_or(self.editing.text.len(), |i| cursor + i);
                    self.editing.select(end, extend);
                }
                "up" | "down" | "pageup" | "pagedown" => {
                    let page = self.bounds.size.height - px(LINE_HEIGHT);
                    if let Some(position) = self.cursor_point(self.editing.cursor()) {
                        let target = position
                            + point(
                                px(0.),
                                match key.key.as_str() {
                                    "up" => px(-LINE_HEIGHT),
                                    "down" => px(LINE_HEIGHT),
                                    "pageup" => -page,
                                    _ => page,
                                },
                            );
                        let index = self.index_at(target);
                        self.editing.select(index, extend);
                    }
                    self.edited(window, cx);
                    window.prevent_default();
                    cx.stop_propagation();
                    return;
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
        let local = line
            .shaped
            .position_for_index((byte - line.start).min(line.shaped.len()), px(LINE_HEIGHT))?;
        Some(
            self.bounds.origin
                + point(
                    px(GUTTER_WIDTH) + local.x - self.scroll_x,
                    line.top - self.scroll + local.y,
                ),
        )
    }

    fn index_at(&self, position: Point<Pixels>) -> usize {
        let local = position - self.bounds.origin + point(self.scroll_x, self.scroll);
        if local.y < px(0.) {
            return 0;
        }
        for line in &self.lines {
            if local.y < line.top + line.height {
                let index = line
                    .shaped
                    .closest_index_for_position(
                        local - point(px(GUTTER_WIDTH), line.top),
                        px(LINE_HEIGHT),
                    )
                    .unwrap_or_else(|i| i);
                return (line.start + index).min(self.editing.text.len());
            }
        }
        self.editing.text.len()
    }

    fn layout(&mut self, bounds: Bounds<Pixels>, window: &mut Window) {
        self.bounds = bounds;
        let editing = &self.editing;
        let width = bounds.size.width - px(GUTTER_WIDTH);
        self.lines.clear();
        let mut top = px(0.);
        let mut start = 0;
        let mut number = 1;
        for hard_line in editing.text.split('\n') {
            let end = start + hard_line.len();
            let mut cuts = vec![start, end];
            cuts.extend([
                editing.selection.start.clamp(start, end),
                editing.selection.end.clamp(start, end),
            ]);
            if let Some(range) = &editing.marked {
                cuts.extend([range.start.clamp(start, end), range.end.clamp(start, end)]);
            }
            if let Some(found) = self.find.as_ref().and_then(|f| f.matches.get(f.current)) {
                cuts.extend([found.start.clamp(start, end), found.end.clamp(start, end)]);
            }
            cuts.sort_unstable();
            cuts.dedup();
            let number_label = number.to_string();
            let gutter = Some(window.text_system().shape_line(
                number_label.clone().into(),
                px(font_size::CODE),
                &[TextRun {
                    len: number_label.len(),
                    font: font(crate::fonts::code_family()),
                    color: rgb(c::FOREGROUND_EXTRA_MUTED).into(),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                }],
                None,
            ));
            let active_match = self
                .find
                .as_ref()
                .and_then(|f| f.matches.get(f.current))
                .cloned();
            let runs =
                cuts.windows(2)
                    .map(|span| TextRun {
                        len: span[1] - span[0],
                        font: font(crate::fonts::code_family()),
                        color: rgb(c::FOREGROUND).into(),
                        background_color: if editing.selection.contains(&span[0])
                            && editing.selection.start != editing.selection.end
                        {
                            Some(rgb(c::SURFACE3).into())
                        } else if active_match.as_ref().is_some_and(|found| {
                            found.contains(&span[0]) && found.start != found.end
                        }) {
                            Some(rgb(c::SURFACE4).into())
                        } else {
                            None
                        },
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
            if let Ok(lines) = window.text_system().shape_text(
                hard_line.to_owned().into(),
                px(font_size::CODE),
                &runs,
                Some(width),
                None,
            ) {
                for shaped in lines {
                    let height = shaped.size(px(LINE_HEIGHT)).height;
                    self.lines.push(Line {
                        start,
                        top,
                        height,
                        gutter: gutter.clone(),
                        shaped,
                    });
                    top += height;
                }
            }
            start = end + 1;
            number += 1;
        }
        self.content_height = top;
        // Wheel scrolling owns the viewport; the caret only claims it after a
        // navigation or edit, never on every frame.
        if self.pending_scroll
            && let Some(caret) = self.cursor_point(self.editing.cursor())
        {
            self.pending_scroll = false;
            self.scroll_toward(caret.y - self.bounds.origin.y);
        }
    }

    fn layout_find(&mut self, bounds: Bounds<Pixels>, window: &mut Window) {
        let Some(find) = &mut self.find else {
            return;
        };
        let empty = find.query.text.is_empty();
        let text = if empty {
            "Find".to_owned()
        } else {
            find.query.text.clone()
        };
        let text_len = text.len();
        let line = window.text_system().shape_line(
            text.into(),
            px(font_size::SM),
            &[TextRun {
                len: text_len,
                font: font(crate::fonts::ui_family()),
                color: rgb(if empty {
                    c::FOREGROUND_EXTRA_MUTED
                } else {
                    c::FOREGROUND
                })
                .into(),
                background_color: None,
                underline: None,
                strikethrough: None,
            }],
            None,
        );
        let x = line.x_for_index(find.query.cursor());
        self.find_scroll_x = self
            .find_scroll_x
            .max(x - bounds.size.width + px(1.))
            .min(x)
            .max(px(0.));
        self.find_line = Some(line);
    }
}

impl Focusable for FileEditor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// The find field owns text input while it is focused; otherwise the buffer does.
impl FileEditor {
    fn input_target_is_find(&self) -> bool {
        self.find_focused && self.find.is_some()
    }

    fn replace_in_target(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.input_target_is_find() {
            if let Some(find) = &mut self.find {
                find.query
                    .replace(range, &text.replace(['\r', '\n'], " "), false, None);
                find.run(&self.editing.text);
            }
            cx.notify();
            let _ = window;
        } else {
            self.editing.replace(range, text, false, None);
            self.edited(window, cx);
        }
    }

    fn selection_in_target(&self) -> Range<usize> {
        if self.input_target_is_find() {
            self.find
                .as_ref()
                .map(|f| f.query.utf16_range(f.query.selection.clone()))
                .unwrap_or_default()
        } else {
            self.editing.utf16_range(self.editing.selection.clone())
        }
    }
}

impl EntityInputHandler for FileEditor {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let (text, byte_range) = if self.input_target_is_find() {
            let find = self.find.as_ref()?;
            let range = find.query.byte_range(range);
            (
                find.query.text.clone(),
                find.query.utf16_range(range.clone()),
            )
        } else {
            let range = self.editing.byte_range(range);
            (
                self.editing.text.clone(),
                self.editing.utf16_range(range.clone()),
            )
        };
        *actual = Some(byte_range.clone());
        text.get(byte_range).map(str::to_owned)
    }
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.selection_in_target(),
            reversed: if self.input_target_is_find() {
                self.find.as_ref()?.query.reversed
            } else {
                self.editing.reversed
            },
        })
    }
    fn set_selected_text_range(
        &mut self,
        range: Range<usize>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
        if self.input_target_is_find() {
            if let Some(find) = &mut self.find {
                find.query.selection = find.query.byte_range(range);
                find.query.reversed = false;
            }
        } else {
            self.editing.selection = self.editing.byte_range(range);
            self.editing.reversed = false;
        }
    }
    fn text_length_utf16(&mut self, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        Some(if self.input_target_is_find() {
            let find = self.find.as_ref()?;
            find.query.text.encode_utf16().count()
        } else {
            self.editing.text.encode_utf16().count()
        })
    }
    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        if self.input_target_is_find() {
            let find = self.find.as_ref()?;
            Some(find.query.utf16_range(find.query.marked.clone()?))
        } else {
            Some(self.editing.utf16_range(self.editing.marked.clone()?))
        }
    }
    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        if self.input_target_is_find() {
            if let Some(find) = &mut self.find {
                find.query.marked = None;
            }
        } else {
            self.editing.marked = None;
        }
    }
    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.replace_in_target(range, text, window, cx);
    }
    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.input_target_is_find() {
            if let Some(find) = &mut self.find {
                find.query
                    .replace(range, &text.replace(['\r', '\n'], " "), true, selected);
                find.run(&self.editing.text);
            }
            cx.notify();
        } else {
            self.editing.replace(range, text, true, selected);
            self.edited(window, cx);
        }
    }
    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        if self.input_target_is_find() {
            let find = self.find.as_ref()?;
            let byte = find.query.byte_range(range).start;
            let x = self
                .find_line
                .as_ref()
                .map_or(px(0.), |line| line.x_for_index(byte))
                - self.find_scroll_x;
            Some(Bounds::new(
                self.bounds.origin + point(x, px(0.)),
                size(px(1.), px(22.)),
            ))
        } else {
            let byte = self.editing.byte_range(range).start;
            Some(Bounds::new(
                self.cursor_point(byte)?,
                size(px(1.), px(LINE_HEIGHT)),
            ))
        }
    }
    fn character_index_for_point(
        &mut self,
        position: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        if self.input_target_is_find() {
            let find = self.find.as_ref()?;
            Some(
                find.query.utf16_offset(
                    self.find_line
                        .as_ref()?
                        .closest_index_for_x(position.x - self.bounds.left() + self.find_scroll_x),
                ),
            )
        } else {
            Some(self.editing.utf16_offset(self.index_at(position)))
        }
    }
}

impl Render for FileEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let measure = cx.entity();
        let paint = cx.entity();
        let find_measure = cx.entity();
        let find_paint = cx.entity();
        let status_label = match self.status {
            Status::Loading => "Loading…".to_owned(),
            Status::Saving => "Saving…".to_owned(),
            Status::Failed => "Failed to load".to_owned(),
            Status::Clean => String::new(),
            Status::Dirty => "Unsaved".to_owned(),
            Status::Conflict { deleted } => {
                if deleted {
                    "Deleted on host".to_owned()
                } else {
                    "Changed on host".to_owned()
                }
            }
        };
        let conflict = matches!(self.status, Status::Conflict { .. });
        let alarming = conflict || self.status == Status::Failed;
        let can_save =
            self.connected && permits_save(self.status, false) && self.save_request.is_none();

        let header = div()
            .h(px(36.))
            .flex_shrink_0()
            .px_2()
            .flex()
            .items_center()
            .gap_2()
            .border_b_1()
            .border_color(rgb(c::BORDER))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(icon("file-text", 14.).text_color(rgb(c::FOREGROUND_MUTED)))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(font_size::SM))
                            .text_color(rgb(c::FOREGROUND_MUTED))
                            .child(self.path.clone()),
                    )
                    .when(!status_label.is_empty(), |row| {
                        row.child(
                            div()
                                .flex_shrink_0()
                                .text_size(px(font_size::SM))
                                .text_color(rgb(if alarming {
                                    c::STATUS_DANGER
                                } else {
                                    c::FOREGROUND_MUTED
                                }))
                                .child(status_label),
                        )
                    }),
            )
            .child(
                div()
                    .id("find-trigger")
                    .size(px(26.))
                    .rounded(px(radius::MD))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(|button| button.bg(rgb(c::SURFACE2)))
                    .child(icon("search", 14.))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| {
                            cx.stop_propagation();
                            this.open_find(window, cx);
                        }),
                    ),
            )
            .child(
                div()
                    .id("save-button")
                    .h(px(26.))
                    .px_2()
                    .rounded(px(radius::MD))
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_size(px(font_size::SM))
                    .when(can_save, |button| {
                        button
                            .cursor_pointer()
                            .bg(rgb(c::ACCENT))
                            .text_color(rgb(c::ACCENT_FOREGROUND))
                    })
                    .when(!can_save, |button| button.opacity(0.5))
                    .child(icon("save", 14.))
                    .child("Save")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            if can_save {
                                this.save(cx);
                            }
                        }),
                    ),
            );

        let banner = conflict.then(|| {
            let deleted = matches!(self.status, Status::Conflict { deleted: true });
            div()
                .flex_shrink_0()
                .px_2()
                .py_1()
                .flex()
                .items_center()
                .gap_2()
                .bg(rgb(c::SURFACE2))
                .border_b_1()
                .border_color(rgb(c::BORDER))
                .text_size(px(font_size::SM))
                .text_color(rgb(c::STATUS_WARNING))
                .child(if deleted {
                    "This file was deleted on the host while you were editing."
                } else {
                    "This file changed on the host while you were editing."
                })
                .child(div().flex_1())
                .when(!deleted, |row| {
                    row.child(
                        div()
                            .id("reload-conflict")
                            .h(px(22.))
                            .px_2()
                            .rounded(px(radius::SM))
                            .flex()
                            .items_center()
                            .cursor_pointer()
                            .hover(|v| v.bg(rgb(c::SURFACE3)))
                            .text_color(rgb(c::FOREGROUND))
                            .child("Reload from host")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.dirty = false;
                                    this.status = Status::Clean;
                                    this.reload(cx);
                                }),
                            ),
                    )
                })
                .when(!deleted, |row| {
                    row.child(
                        div()
                            .id("overwrite-conflict")
                            .h(px(22.))
                            .px_2()
                            .rounded(px(radius::SM))
                            .flex()
                            .items_center()
                            .cursor_pointer()
                            .hover(|v| v.bg(rgb(c::SURFACE3)))
                            .text_color(rgb(c::FOREGROUND))
                            .child("Overwrite host copy")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.overwrite(cx);
                                }),
                            ),
                    )
                })
                .when(deleted, |row| {
                    row.child(
                        div()
                            .id("discard-deleted")
                            .h(px(22.))
                            .px_2()
                            .rounded(px(radius::SM))
                            .flex()
                            .items_center()
                            .cursor_pointer()
                            .hover(|v| v.bg(rgb(c::SURFACE3)))
                            .text_color(rgb(c::STATUS_DANGER))
                            .child("Discard changes")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.confirm_close_discard(cx);
                                }),
                            ),
                    )
                })
        });

        let find_bar = self.find.is_some().then(|| {
            let (matches, current) = self
                .find
                .as_ref()
                .map(|f| (f.matches.len(), f.current))
                .unwrap_or((0, 0));
            let count = if matches == 0 {
                "No matches".to_owned()
            } else {
                format!("{} of {}", current + 1, matches)
            };
            div()
                .flex_shrink_0()
                .h(px(32.))
                .px_2()
                .flex()
                .items_center()
                .gap_2()
                .bg(rgb(c::SURFACE1))
                .border_b_1()
                .border_color(rgb(c::BORDER))
                .child(
                    div()
                        .id("find-input")
                        .track_focus(self.find.as_ref().map(|f| &f.focus).unwrap_or(&self.focus))
                        .min_w(px(180.))
                        .h(px(22.))
                        .px_2()
                        .rounded(px(radius::SM))
                        .bg(rgb(c::INPUT))
                        .border_1()
                        .border_color(rgb(c::BORDER_ACCENT))
                        .cursor_text()
                        .flex_1()
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                            let key = &event.keystroke.key;
                            if key == "escape" {
                                this.find = None;
                                this.find_focused = false;
                                this.focus.focus(window, cx);
                                cx.notify();
                                window.prevent_default();
                                cx.stop_propagation();
                            } else if key == "enter" {
                                let forward = !event.keystroke.modifiers.shift;
                                this.find_step(forward, window, cx);
                                window.prevent_default();
                                cx.stop_propagation();
                            }
                        }))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, window, cx| {
                                cx.stop_propagation();
                                this.find_focused = true;
                                if let Some(find) = &this.find {
                                    find.focus.clone().focus(window, cx);
                                }
                                cx.notify();
                            }),
                        )
                        .child(
                            canvas(
                                move |bounds, window, cx| {
                                    find_measure.update(cx, |this, _| {
                                        this.layout_find(bounds, window);
                                    });
                                },
                                move |bounds, (), window, cx| {
                                    find_paint.update(cx, |this, cx| {
                                        let Some(find) = &mut this.find else {
                                            return;
                                        };
                                        window.handle_input(
                                            &find.focus,
                                            ElementInputHandler::new(bounds, find_paint.clone()),
                                            cx,
                                        );
                                        if let Some(line) = &this.find_line {
                                            if !find.query.selection.is_empty() {
                                                window.paint_quad(fill(
                                                    Bounds::new(
                                                        bounds.origin
                                                            + point(
                                                                line.x_for_index(
                                                                    find.query.selection.start,
                                                                ) - this.find_scroll_x,
                                                                px(3.),
                                                            ),
                                                        size(
                                                            line.x_for_index(
                                                                find.query.selection.end,
                                                            ) - line.x_for_index(
                                                                find.query.selection.start,
                                                            ),
                                                            px(16.),
                                                        ),
                                                    ),
                                                    rgb(c::SURFACE3),
                                                ));
                                            }
                                            let _ = line.paint(
                                                bounds.origin - point(this.find_scroll_x, px(0.)),
                                                px(22.),
                                                TextAlign::Left,
                                                None,
                                                window,
                                                cx,
                                            );
                                            if find.focus.is_focused(window) {
                                                let x = line.x_for_index(find.query.cursor())
                                                    - this.find_scroll_x;
                                                window.paint_quad(fill(
                                                    Bounds::new(
                                                        bounds.origin + point(x, px(3.)),
                                                        size(px(1.), px(16.)),
                                                    ),
                                                    rgb(c::FOREGROUND),
                                                ));
                                            }
                                        }
                                    });
                                },
                            )
                            .flex_1()
                            .h_full(),
                        ),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(font_size::SM))
                        .text_color(rgb(c::FOREGROUND_MUTED))
                        .child(count),
                )
                .child(
                    div()
                        .id("find-previous")
                        .size(px(22.))
                        .rounded(px(radius::SM))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .hover(|v| v.bg(rgb(c::SURFACE2)))
                        .child(icon("chevron-down", 14.))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, window, cx| {
                                cx.stop_propagation();
                                this.find_step(false, window, cx);
                            }),
                        ),
                )
                .child(
                    div()
                        .id("find-next")
                        .size(px(22.))
                        .rounded(px(radius::SM))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .hover(|v| v.bg(rgb(c::SURFACE2)))
                        .child(icon("chevron-down", 14.))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, window, cx| {
                                cx.stop_propagation();
                                this.find_step(true, window, cx);
                            }),
                        ),
                )
                .child(
                    div()
                        .id("find-close")
                        .size(px(22.))
                        .rounded(px(radius::SM))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .hover(|v| v.bg(rgb(c::SURFACE2)))
                        .child(icon("x", 14.))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, window, cx| {
                                cx.stop_propagation();
                                this.find = None;
                                this.find_focused = false;
                                this.focus.focus(window, cx);
                                cx.notify();
                            }),
                        ),
                )
        });

        let closing = self.closing && self.is_dirty();
        let confirm = closing.then(|| {
            div()
                .flex_shrink_0()
                .px_2()
                .py_1()
                .flex()
                .items_center()
                .gap_2()
                .bg(rgb(c::SURFACE2))
                .border_b_1()
                .border_color(rgb(c::BORDER))
                .text_size(px(font_size::SM))
                .text_color(rgb(c::STATUS_WARNING))
                .child("This file has unsaved changes.")
                .child(div().flex_1())
                .child(
                    div()
                        .id("close-save")
                        .h(px(22.))
                        .px_2()
                        .rounded(px(radius::SM))
                        .flex()
                        .items_center()
                        .cursor_pointer()
                        .hover(|v| v.bg(rgb(c::SURFACE3)))
                        .text_color(rgb(c::FOREGROUND))
                        .child("Save and close")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.confirm_close_save(cx);
                            }),
                        ),
                )
                .child(
                    div()
                        .id("close-discard")
                        .h(px(22.))
                        .px_2()
                        .rounded(px(radius::SM))
                        .flex()
                        .items_center()
                        .cursor_pointer()
                        .hover(|v| v.bg(rgb(c::SURFACE3)))
                        .text_color(rgb(c::STATUS_DANGER))
                        .child("Discard changes")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.confirm_close_discard(cx);
                            }),
                        ),
                )
        });

        let error = self.error.clone().map(|error| {
            div()
                .flex_shrink_0()
                .px_2()
                .py_1()
                .flex()
                .items_center()
                .gap_2()
                .text_size(px(font_size::SM))
                .text_color(rgb(c::STATUS_DANGER))
                .child(error)
                .child(div().flex_1())
                .when(self.connected, |row| {
                    row.child(
                        div()
                            .id("editor-retry")
                            .h(px(22.))
                            .px_2()
                            .rounded(px(radius::SM))
                            .flex()
                            .items_center()
                            .cursor_pointer()
                            .hover(|v| v.bg(rgb(c::SURFACE2)))
                            .text_color(rgb(c::FOREGROUND_MUTED))
                            .child("Retry")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.error = None;
                                    this.load(cx);
                                }),
                            ),
                    )
                })
        });

        div()
            .id("file-editor")
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(c::SURFACE_WORKSPACE))
            .text_color(rgb(c::FOREGROUND))
            .child(header)
            .children(confirm)
            .children(banner)
            .children(find_bar)
            .children(error)
            .child(
                div()
                    .id("editor-surface")
                    .track_focus(&self.focus)
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .on_scroll(cx.listener(|this, event: &gpui::ScrollEvent, _, cx| {
                        let delta = event.delta.pixel_delta(px(LINE_HEIGHT));
                        this.scroll -= delta.y;
                        this.scroll_x = this.scroll_x - delta.x;
                        this.clamp_scroll();
                        cx.notify();
                    }))
                    .on_key_down(cx.listener(Self::key_down))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            this.find_focused = false;
                            this.focus.focus(window, cx);
                            this.selecting = true;
                            let index = this.index_at(event.position);
                            this.editing.select(index, event.modifiers.shift);
                            this.pending_scroll = false;
                            cx.notify();
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
                                let _ = window;
                                cx.notify();
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
                                    window.with_content_mask(
                                        Some(gpui::ContentMask { bounds }),
                                        |window| {
                                            if view.status == Status::Loading
                                                && view.editing.text.is_empty()
                                                || view.lines.is_empty()
                                            {
                                                return;
                                            }
                                            for line in &view.lines {
                                                let origin = bounds.origin
                                                    + point(px(0.), line.top - view.scroll);
                                                if origin.y + line.height < bounds.top()
                                                    || origin.y > bounds.bottom()
                                                {
                                                    continue;
                                                }
                                                if let Some(gutter) = &line.gutter {
                                                    let _ = gutter.paint(
                                                        origin + point(px(12.), px(4.)),
                                                        px(LINE_HEIGHT),
                                                        TextAlign::Left,
                                                        None,
                                                        window,
                                                        cx,
                                                    );
                                                }
                                                let text_origin = origin
                                                    + point(
                                                        px(GUTTER_WIDTH) - view.scroll_x,
                                                        px(0.),
                                                    );
                                                let _ = line.shaped.paint_background(
                                                    text_origin,
                                                    px(LINE_HEIGHT),
                                                    TextAlign::Left,
                                                    Some(bounds),
                                                    window,
                                                    cx,
                                                );
                                                let _ = line.shaped.paint(
                                                    text_origin,
                                                    px(LINE_HEIGHT),
                                                    TextAlign::Left,
                                                    Some(bounds),
                                                    window,
                                                    cx,
                                                );
                                            }
                                            if view.focus.is_focused(window)
                                                && !view.input_target_is_find()
                                                && let Some(origin) =
                                                    view.cursor_point(view.editing.cursor())
                                            {
                                                window.paint_quad(fill(
                                                    Bounds::new(
                                                        origin,
                                                        size(px(1.), px(LINE_HEIGHT)),
                                                    ),
                                                    rgb(c::FOREGROUND),
                                                ));
                                            }
                                        },
                                    );
                                });
                            },
                        )
                        .size_full(),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::permits_save;
    use super::{
        Status, WriteOutcome, file_subscription_key, find_matches, status_after_disconnect,
        subscription_from_response, unsaved_after_write, update_targets, write_fields,
        write_outcome,
    };
    use serde_json::json;
    use std::ops::Range;

    #[test]
    fn find_matches_reports_disjoint_byte_ranges_case_insensitively() {
        let text = "aBc abc ABC";
        assert_eq!(find_matches(text, "abc"), vec![0..3, 4..7, 8..11]);
    }

    #[test]
    fn overlapping_matches_never_recurse_into_a_previous_hit() {
        assert_eq!(find_matches("aaaa", "aa"), vec![0..2, 2..4]);
    }

    #[test]
    fn an_empty_or_absent_query_yields_no_matches() {
        assert!(find_matches("anything", "").is_empty());
        assert!(find_matches("anything", "zzz").is_empty());
    }

    #[test]
    fn byte_ranges_slice_the_original_text() {
        let text = "Hello ÜNICODE";
        for range in find_matches(text, "ünicode") {
            assert_eq!(&text[range], "ÜNICODE");
        }
    }

    #[test]
    fn length_changing_case_folding_never_shifts_byte_offsets() {
        // Folding is ASCII-only over original bytes, so offsets never drift:
        // İ (2 bytes) cannot match a query "i" the way a full Unicode fold
        // would, and matches still slice the original text exactly.
        let text = "İNG ẞ";
        let empty: Vec<Range<usize>> = Vec::new();
        assert_eq!(find_matches(text, "ing"), empty);
        assert_eq!(find_matches(text, "NG"), vec![2..4]);
        assert_eq!(&text[2..4], "NG");
        assert_eq!(find_matches(text, "ẞ"), vec![5..8]);
        assert_eq!(&text[5..8], "ẞ");
    }

    #[test]
    fn multi_byte_neighbours_are_not_split_by_a_match() {
        let text = "aÄb";
        let empty: Vec<Range<usize>> = Vec::new();
        assert_eq!(find_matches(text, "ä"), empty);
        assert_eq!(find_matches(text, "Äb"), vec![1..4]);
    }

    #[test]
    fn queued_autosave_and_normal_save_cannot_overwrite_a_conflict() {
        assert!(permits_save(Status::Dirty, false));
        for status in [
            Status::Loading,
            Status::Failed,
            Status::Clean,
            Status::Saving,
            Status::Conflict { deleted: false },
            Status::Conflict { deleted: true },
        ] {
            assert!(!permits_save(status, false), "{status:?}");
        }
        assert!(permits_save(Status::Conflict { deleted: false }, true));
        assert!(!permits_save(Status::Conflict { deleted: true }, true));
    }

    #[test]
    fn write_fields_carry_the_last_known_version_marker_not_a_blank() {
        let fields = write_fields("/w", "a.rs", "text", Some("2026-01-01T00:00:00Z"), None)
            .expect("a read file always has a marker");
        assert_eq!(fields["expectedModifiedAt"], "2026-01-01T00:00:00Z");
        assert!(fields["expectedRevision"].is_null());

        let fields = write_fields("/w", "a.rs", "text", Some("m"), Some("dev:1:2:3"))
            .expect("a read file always has a marker");
        assert_eq!(fields["expectedRevision"], "dev:1:2:3");
        assert_eq!(fields["expectedModifiedAt"], "m");

        // Overwriting a conflict without a known version must refuse instead
        // of sending "" (which the host can never match).
        assert!(write_fields("/w", "a.rs", "text", None, None).is_err());
    }

    #[test]
    fn write_outcomes_map_conflicts_and_deleted_hosts() {
        let written = write_outcome(&json!({
            "status":"written","modifiedAt":"m2","size":4,"revision":"r2"
        }));
        assert!(matches!(written, WriteOutcome::Written));

        let conflict = write_outcome(&json!({
            "status":"conflict",
            "version":{"status":"ready","cwd":"/w","path":"a.rs","size":1,
                       "modifiedAt":"m3","revision":"r3"}
        }));
        assert!(matches!(
            conflict,
            WriteOutcome::Conflict { deleted: false }
        ));

        let deleted = write_outcome(&json!({
            "status":"conflict","version":{"status":"missing","cwd":"/w","path":"a.rs"}
        }));
        assert!(matches!(deleted, WriteOutcome::Conflict { deleted: true }));

        let error = write_outcome(&json!({"status":"error","error":"File is too large to edit"}));
        assert!(matches!(error, WriteOutcome::Error(message) if message.contains("too large")));
    }

    #[test]
    fn a_disconnect_during_a_save_returns_the_buffer_to_an_editable_state() {
        // A write in flight can never acknowledge after the host goes away.
        // Leaving Saving sticks the header there: save() and autosave both
        // refuse a status that is neither Dirty nor Conflict.
        assert_eq!(status_after_disconnect(Status::Saving), Status::Dirty);
        assert_eq!(status_after_disconnect(Status::Loading), Status::Failed);
        assert_eq!(status_after_disconnect(Status::Dirty), Status::Dirty);
        assert_eq!(status_after_disconnect(Status::Clean), Status::Clean);
        assert_eq!(
            status_after_disconnect(Status::Conflict { deleted: true }),
            Status::Conflict { deleted: true }
        );
    }

    #[test]
    fn a_write_acknowledgement_only_settles_the_text_it_carried() {
        // Ctrl+S, then one keystroke while the request is in flight: the
        // acknowledgement must not mark the newer text saved, or closing the
        // tab drops it without a prompt.
        assert!(!unsaved_after_write("v2", Some("v2")));
        assert!(unsaved_after_write("v2x", Some("v2")));
        assert!(unsaved_after_write("v2", None));
    }

    #[test]
    fn the_file_subscription_key_is_local_and_workspace_scoped() {
        // The key is what `Connection::unsubscribe` looks up; a host-assigned
        // id closes nothing and leaks the host watcher.
        assert_eq!(
            file_subscription_key("/repos/a", "README.md"),
            "file:/repos/a:README.md"
        );
        assert_ne!(
            file_subscription_key("/repos/a", "README.md"),
            file_subscription_key("/repos/b", "README.md")
        );
        assert_ne!(
            file_subscription_key("/repos/a", "README.md"),
            "0f4c2a5e-6d7b-4c1a-9f2e-2b6f5a8c9d01"
        );
    }

    #[test]
    fn subscribe_responses_bind_the_host_uuid_not_a_local_key() {
        // ui.rs hands the editor the unwrapped `message.payload` object.
        let payload = json!({
            "subscriptionId":"0f4c2a5e-6d7b-4c1a-9f2e-2b6f5a8c9d01",
            "requestId":"req-1"
        });
        let bound = subscription_from_response(&payload);
        assert_eq!(
            bound.as_deref(),
            Some("0f4c2a5e-6d7b-4c1a-9f2e-2b6f5a8c9d01")
        );
        // Updates carrying that host id target the editor; a local "file:a.rs"
        // key would never have matched and every update was dropped before.
        assert!(update_targets(bound.as_deref(), &payload));
        assert!(!update_targets(Some("file:a.rs"), &payload));
        assert!(!update_targets(None, &payload));
        assert!(!update_targets(
            Some("other"),
            &json!({"subscriptionId":"0f4c2a5e-6d7b-4c1a-9f2e-2b6f5a8c9d01"})
        ));
    }
}

/// A fake host over a real websocket: bind the subscribe response to a
/// host UUID, require a real compare-and-swap marker for writes, and push
/// an update on the bound subscription — the exact seam the editor rides.
#[tokio::test]
async fn subscribe_save_and_conflict_round_trip_against_a_host_socket() {
    use futures_util::{SinkExt, StreamExt};
    use paseo_desktop::client::{Connection, Target};
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::Message;

    const MODIFIED: &str = "2026-01-01T00:00:00.000Z";
    const REVISION: &str = "dev:1:2:3";
    const SUBSCRIPTION: &str = "0f4c2a5e-6d7b-4c1a-9f2e-2b6f5a8c9d01";

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        socket
            .send(Message::Text(
                json!({"type":"session","message":{"type":"status","payload":{
                        "status":"server_info","serverId":"fixture","features":{}}}})
                .to_string(),
            ))
            .await
            .unwrap();
        while let Some(message) = socket.next().await {
            let Message::Text(text) = message.unwrap() else {
                continue;
            };
            let envelope: Json = serde_json::from_str(&text).unwrap();
            if envelope["type"] != "session" {
                continue;
            }
            let message = &envelope["message"];
            let request_id = message["requestId"].as_str().unwrap_or_default();
            match message["type"].as_str().unwrap_or_default() {
                "fs.file.subscribe.request" => {
                    socket
                            .send(Message::Text(
                                json!({"type":"session","message":{"type":"fs.file.subscribe.response","payload":{
                                    "subscriptionId":SUBSCRIPTION,"requestId":request_id}}})
                                .to_string(),
                            ))
                            .await
                            .unwrap();
                    // Push an update on the just-bound subscription so the
                    // test proves the host id path reaches delivery.
                    socket
                        .send(Message::Text(
                            json!({"type":"session","message":{"type":"fs.file.update","payload":{
                                    "subscriptionId":SUBSCRIPTION,
                                    "version":{"status":"ready","cwd":"/w","path":"a.rs","size":2,
                                               "modifiedAt":MODIFIED,"revision":REVISION}}}})
                            .to_string(),
                        ))
                        .await
                        .unwrap();
                }
                "file_explorer_request" => {
                    socket
                            .send(Message::Text(
                                json!({"type":"session","message":{"type":"file_explorer_response","payload":{
                                    "cwd":"/w","path":"a.rs","mode":"file","directory":null,
                                    "file":{"path":"a.rs","kind":"text","encoding":"utf-8",
                                            "content":"v1","size":2,"modifiedAt":MODIFIED,
                                            "revision":REVISION},
                                    "error":null,"requestId":request_id}}})
                                .to_string(),
                            ))
                            .await
                            .unwrap();
                }
                "fs.file.write.request" => {
                    let expected = message["expectedModifiedAt"].as_str().unwrap_or_default();
                    let reply = if expected == MODIFIED {
                        json!({"type":"session","message":{"type":"fs.file.write.response","payload":{
                                "result":{"status":"written","modifiedAt":"m2","size":2,
                                          "revision":"dev:1:2:4"},
                                "requestId":request_id}}})
                    } else {
                        // A blank or stale marker never matches: conflict,
                        // and a missing file reports the missing version.
                        json!({"type":"session","message":{"type":"fs.file.write.response","payload":{
                                "result":{"status":"conflict","version":{"status":"missing",
                                          "cwd":"/w","path":"a.rs"}},
                                "requestId":request_id}}})
                    };
                    socket.send(Message::Text(reply.to_string())).await.unwrap();
                }
                _ => continue,
            }
        }
    });

    let connection =
        Connection::start(Target::remote(format!("ws://{address}/ws"), None).unwrap()).unwrap();
    let mut events = connection.events.clone();
    while !matches!(
        tokio::time::timeout(std::time::Duration::from_secs(5), events.recv()).await,
        Ok(Ok(paseo_desktop::client::Event::Ready(_)))
    ) {}

    // Read the file: the editor stores modifiedAt/revision from this reply.
    let read_id = connection
        .request(
            "file_explorer_request",
            json!({"cwd":"/w","path":"a.rs","mode":"file","maxBytes":1_048_576}),
        )
        .unwrap();
    let mut read_modified = None;
    let mut bound_update = false;
    // Subscribe: the response's host UUID must reach the bookkeeping.
    let subscribe_id = connection
        .subscribe(
            "file:a.rs",
            "fs.file.subscribe.request",
            json!({"cwd":"/w","path":"a.rs"}),
        )
        .unwrap();
    let _ = subscribe_id;
    let mut save_written = None;
    let mut overwrite_conflict = false;
    let deadline = std::time::Duration::from_secs(5);
    loop {
        let event = tokio::time::timeout(deadline, events.recv()).await;
        let Ok(Ok(paseo_desktop::client::Event::Session { kind, payload })) = event else {
            break;
        };
        match kind.as_str() {
            "file_explorer_response" if payload["requestId"] == read_id.as_str() => {
                read_modified = Some(payload["file"]["modifiedAt"].as_str().unwrap().to_owned());
                // Save with the real marker: the host accepts it.
                let write_id = connection
                    .request(
                        "fs.file.write.request",
                        json!({"cwd":"/w","path":"a.rs","content":"v2",
                                   "expectedModifiedAt":read_modified.as_deref().unwrap()}),
                    )
                    .unwrap();
                let _ = write_id;
            }
            "fs.file.subscribe.response" => {
                assert_eq!(
                    payload["subscriptionId"].as_str(),
                    Some(SUBSCRIPTION),
                    "the host-bound id must reach the client"
                );
                // The editor binds exactly this id (subscription_from_response).
                assert_eq!(
                    subscription_from_response(&payload).as_deref(),
                    Some(SUBSCRIPTION)
                );
            }
            "fs.file.write.response" => {
                if payload["result"]["status"] == "written" && save_written.is_none() {
                    save_written = Some(payload["result"]["modifiedAt"].clone());
                    // Overwrite attempt with a stale marker conflicts.
                    let _ = connection
                        .request(
                            "fs.file.write.request",
                            json!({"cwd":"/w","path":"a.rs","content":"v3",
                                       "expectedModifiedAt":"2020-01-01T00:00:00.000Z"}),
                        )
                        .unwrap();
                } else if payload["result"]["status"] == "conflict" {
                    assert_eq!(payload["result"]["version"]["status"], "missing");
                    overwrite_conflict = true;
                }
            }
            "fs.file.update" => {
                assert_eq!(payload["subscriptionId"].as_str(), Some(SUBSCRIPTION));
                bound_update = true;
            }
            _ => {}
        }
        if read_modified.is_some() && save_written.is_some() && overwrite_conflict && bound_update {
            break;
        }
    }
    assert_eq!(read_modified.as_deref(), Some(MODIFIED));
    assert_eq!(save_written, Some(json!("m2")));
    assert!(overwrite_conflict, "a stale marker must conflict");
    assert!(
        bound_update,
        "the bound subscription must pass updates through"
    );
    connection.close();
    server.abort();
}

/// A fake host over a real websocket: bind a subscribe response to a host UUID,
/// then record every release the client sends. Closing a tab must release the
/// host id through the local bookkeeping key; the id itself is not a key.
#[tokio::test]
async fn closing_a_tab_releases_the_host_subscription_by_its_local_key() {
    use futures_util::{SinkExt, StreamExt};
    use paseo_desktop::client::{Connection, Target};
    use std::sync::{Arc, Mutex};
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::Message;

    const SUBSCRIPTION: &str = "0f4c2a5e-6d7b-4c1a-9f2e-2b6f5a8c9d01";
    const HOST_ID: &str = "host-uuid-that-is-not-a-local-key";
    let key = file_subscription_key("/repos/a", "README.md");

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let released = Arc::new(Mutex::new(Vec::<String>::new()));
    let host_released = released.clone();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        socket
            .send(Message::Text(
                json!({"type":"session","message":{"type":"status","payload":{
                        "status":"server_info","serverId":"fixture","features":{}}}})
                .to_string(),
            ))
            .await
            .unwrap();
        while let Some(message) = socket.next().await {
            let Message::Text(text) = message.unwrap() else {
                continue;
            };
            let envelope: Json = serde_json::from_str(&text).unwrap();
            if envelope["type"] != "session" {
                continue;
            }
            let message = &envelope["message"];
            match message["type"].as_str().unwrap_or_default() {
                "fs.file.subscribe.request" => {
                    socket
                        .send(Message::Text(
                            json!({"type":"session","message":{"type":"fs.file.subscribe.response","payload":{
                                    "subscriptionId":SUBSCRIPTION,
                                    "requestId":message["requestId"]}}})
                            .to_string(),
                        ))
                        .await
                        .unwrap();
                }
                "subscription.release.request" => {
                    host_released.lock().unwrap().push(
                        message["subscriptionId"]
                            .as_str()
                            .unwrap_or_default()
                            .to_owned(),
                    );
                }
                _ => continue,
            }
        }
    });

    let connection =
        Connection::start(Target::remote(format!("ws://{address}/ws"), None).unwrap()).unwrap();
    let events = connection.events.clone();
    while !matches!(
        tokio::time::timeout(std::time::Duration::from_secs(5), events.recv()).await,
        Ok(Ok(paseo_desktop::client::Event::Ready(_)))
    ) {}

    let request = connection
        .subscribe(
            &key,
            "fs.file.subscribe.request",
            json!({"cwd":"/repos/a","path":"README.md"}),
        )
        .unwrap();
    let deadline = std::time::Duration::from_secs(5);
    while let Ok(Ok(paseo_desktop::client::Event::Session { kind, payload })) =
        tokio::time::timeout(deadline, events.recv()).await
    {
        // The binding only exists once the response arrives.
        if kind == "fs.file.subscribe.response" && payload["requestId"] == request.as_str() {
            assert_eq!(payload["subscriptionId"].as_str(), Some(SUBSCRIPTION));
            break;
        }
    }

    // The host id is not a bookkeeping key: this releases nothing.
    connection.unsubscribe(HOST_ID);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(
        released.lock().unwrap().is_empty(),
        "a host id is not a subscription key, so it must release nothing"
    );
    // The tab close path sends the local key, which releases the bound host id.
    connection.unsubscribe(&key);

    let waited = std::time::Instant::now();
    while released.lock().unwrap().is_empty() {
        assert!(
            waited.elapsed() < std::time::Duration::from_secs(5),
            "no release reached the host; the local key was not the one used"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(
        released.lock().unwrap().as_slice(),
        [SUBSCRIPTION.to_owned()],
        "exactly the bound host id is released, once"
    );
    connection.close();
    server.abort();
}

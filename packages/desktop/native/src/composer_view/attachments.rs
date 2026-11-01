use super::*;
use gpui::{RenderImage, img};
use paseo_desktop::{
    attachment_geometry::{
        CONTENT_HEIGHT, PREVIEW_CONTROL_SIZE, PREVIEW_MAX_HEIGHT, PREVIEW_MAX_WIDTH,
        PREVIEW_PADDING,
    },
    attachments::{DraftAttachment, Prepared, ReadyAttachment, UploadedFile, prepare},
};
use std::{collections::BTreeMap, path::PathBuf, sync::atomic::Ordering};

#[derive(Default)]
pub(super) struct Attachments {
    requests: BTreeMap<String, String>,
    preparing: Option<String>,
    thumbnails: BTreeMap<String, Arc<RenderImage>>,
    pub(super) menu: bool,
    picking: bool,
    pub(super) error: Option<String>,
    menu_focus: Option<FocusHandle>,
    menu_active: usize,
    button_bounds: Bounds<Pixels>,
    preview_id: Option<String>,
    preview: Option<Result<Arc<RenderImage>, String>>,
    preview_focus: Option<FocusHandle>,
}
impl Composer {
    fn open_attachment_image(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ReadyAttachment::Image(data)) = self
            .state
            .attachments
            .iter()
            .find(|a| a.id == id)
            .and_then(|a| a.ready.as_ref())
        else {
            return;
        };
        let data = data.clone();
        let id = id.to_owned();
        self.attachments.preview_id = Some(id.clone());
        self.attachments.preview = None;
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        self.attachments.preview_focus = Some(focus);
        let task = cx.background_spawn(async move { paseo_desktop::attachments::preview(&data) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if this.attachments.preview_id.as_ref() != Some(&id) {
                    return;
                }
                this.attachments.preview =
                    Some(result.map(render_image).map_err(|e| e.to_string()));
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn close_attachment_image(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.attachments.preview_id = None;
        self.attachments.preview = None;
        self.attachments.preview_focus = None;
        self.focus.focus(window, cx);
        cx.notify();
    }
    pub(super) fn attachment_preview(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let focus = self.attachments.preview_focus.as_ref()?;
        let viewport = window.viewport_size();
        let content = div()
            .id("attachment-lightbox")
            .track_focus(focus)
            .relative()
            .w(viewport.width)
            .h(viewport.height)
            .bg(gpui::rgba(0x000000e6))
            .flex()
            .items_center()
            .justify_center()
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    this.close_attachment_image(window, cx);
                    window.prevent_default();
                    cx.stop_propagation();
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    cx.stop_propagation();
                    this.close_attachment_image(window, cx);
                }),
            );
        let content = match self.attachments.preview.as_ref() {
            Some(Ok(image)) => {
                let native = image.size(0);
                let width = native.width.0 as f32;
                let height = native.height.0 as f32;
                let fit = ((f32::from(viewport.width) - PREVIEW_PADDING * 2.) / width)
                    .min((f32::from(viewport.height) - PREVIEW_PADDING * 2.) / height)
                    .min(PREVIEW_MAX_WIDTH / width)
                    .min(PREVIEW_MAX_HEIGHT / height)
                    .max(0.);
                content.child(
                    img(image.clone())
                        .w(px(width * fit))
                        .h(px(height * fit))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()),
                )
            }
            Some(Err(error)) => content.child(
                div()
                    .text_color(rgb(crate::theme::colors::FOREGROUND_MUTED))
                    .child(error.clone()),
            ),
            None => content.child("Loading…"),
        };
        Some(
            gpui::deferred(
                gpui::anchored()
                    .position(point(px(0.), px(0.)))
                    .anchor(gpui::Anchor::TopLeft)
                    .child(
                        content.child(
                            div()
                                .id("close-image")
                                .absolute()
                                .top_2()
                                .right_2()
                                .size(px(PREVIEW_CONTROL_SIZE))
                                .rounded(px(6.))
                                .bg(rgb(crate::theme::colors::SURFACE2))
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .child(crate::chrome::icon("x", 14.))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.close_attachment_image(window, cx);
                                    }),
                                ),
                        ),
                    ),
            )
            .priority(10)
            .into_any_element(),
        )
    }
    pub(super) fn pick_attachments(
        &mut self,
        as_image: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.attachments.menu = false;
        self.attachments.menu_focus = None;
        self.focus.focus(window, cx);
        if self.attachments.picking || !self.connected {
            return;
        }
        self.attachments.error = None;
        self.attachments.picking = true;
        let picker = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some(if as_image { "Add image" } else { "Add file" }.into()),
        });
        cx.spawn(async move |this, cx| {
            let result = picker.await;
            let _ = this.update(cx, |this, cx| {
                this.attachments.picking = false;
                match result {
                    Ok(Ok(Some(paths))) => this.add_attachment_paths(paths, Some(as_image), cx),
                    Ok(Ok(None)) => {}
                    Ok(Err(error)) => {
                        this.attachments.error = Some(format!("Cannot select attachments: {error}"))
                    }
                    Err(error) => {
                        this.attachments.error =
                            Some(format!("File picker closed unexpectedly: {error}"))
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    pub(super) fn add_attachment_paths(
        &mut self,
        paths: Vec<PathBuf>,
        as_image: Option<bool>,
        cx: &mut Context<Self>,
    ) {
        if !self.connected {
            return;
        }
        self.attachments.error = None;
        for path in paths {
            let mut attachment = DraftAttachment::new(path);
            attachment.as_image = as_image.unwrap_or_else(|| {
                paseo_desktop::attachment_geometry::IMAGE_TYPES
                    .iter()
                    .any(|(extension, _)| {
                        attachment.path.extension().is_some_and(|ext| {
                            ext.to_string_lossy().eq_ignore_ascii_case(extension)
                        })
                    })
            });
            self.state.attachments.push(attachment);
        }
        self.start_next_attachment(cx);
        cx.notify();
    }
    fn start_next_attachment(&mut self, cx: &mut Context<Self>) {
        if self.attachments.preparing.is_some() || !self.connected {
            return;
        }
        let Some(attachment) = self
            .state
            .attachments
            .iter()
            .find(|a| a.ready.is_none() && a.error.is_none())
        else {
            return;
        };
        let id = attachment.id.clone();
        let path = attachment.path.clone();
        let as_image = attachment.as_image;
        self.attachments.preparing = Some(id.clone());
        let task = cx.background_spawn(async move { prepare(&path, as_image) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if this.attachments.preparing.as_ref() != Some(&id) {
                    return;
                }
                if !this.state.attachments.iter().any(|a| a.id == id) {
                    this.attachments.preparing = None;
                    this.start_next_attachment(cx);
                    return;
                }
                match result {
                    Ok(Prepared::Image(data, thumbnail)) => {
                        if let Some(thumbnail) = thumbnail {
                            let mut rgba = thumbnail.rgba;
                            for pixel in rgba.chunks_exact_mut(4) {
                                pixel.swap(0, 2);
                            }
                            let frame = image::Frame::new(
                                image::RgbaImage::from_raw(thumbnail.width, thumbnail.height, rgba)
                                    .expect("bounded thumbnail dimensions"),
                            );
                            this.attachments
                                .thumbnails
                                .insert(id.clone(), Arc::new(RenderImage::new(vec![frame])));
                        }
                        this.state
                            .attachments
                            .iter_mut()
                            .find(|a| a.id == id)
                            .unwrap()
                            .ready = Some(ReadyAttachment::Image(data));
                        this.attachments.preparing = None;
                        this.start_next_attachment(cx);
                    }
                    Ok(Prepared::File(file)) => {
                        let request = Uuid::new_v4().to_string();
                        this.attachments
                            .requests
                            .insert(request.clone(), id.clone());
                        let cancel = this
                            .state
                            .attachments
                            .iter()
                            .find(|a| a.id == id)
                            .unwrap()
                            .cancelled
                            .clone();
                        let connection = this.connection.clone();
                        let request_for_task = request.clone();
                        let task = cx.background_spawn(async move {
                            connection
                                .upload_file(file, &request_for_task, cancel)
                                .await
                        });
                        let failed_request = request.clone();
                        cx.spawn(async move |this, cx| {
                            if let Err(error) = task.await {
                                let _ = this.update(cx, |this, cx| {
                                    this.finish_attachment(
                                        &failed_request,
                                        Err(error.to_string()),
                                        cx,
                                    );
                                });
                            }
                        })
                        .detach();
                        let timer = cx
                            .background_executor()
                            .timer(std::time::Duration::from_secs(10 * 60));
                        cx.spawn(async move |this, cx| {
                            timer.await;
                            let _ = this.update(cx, |this, cx| {
                                this.finish_attachment(
                                    &request,
                                    Err("Attachment upload timed out. Retry or remove the file."
                                        .into()),
                                    cx,
                                );
                            });
                        })
                        .detach();
                    }
                    Err(error) => {
                        this.state
                            .attachments
                            .iter_mut()
                            .find(|a| a.id == id)
                            .unwrap()
                            .error = Some(error.to_string());
                        this.attachments.preparing = None;
                        this.start_next_attachment(cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn finish_attachment(
        &mut self,
        request: &str,
        result: Result<UploadedFile, String>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(id) = self.attachments.requests.remove(request) else {
            return false;
        };
        if let Some(attachment) = self.state.attachments.iter_mut().find(|a| a.id == id) {
            match result {
                Ok(file) => attachment.ready = Some(ReadyAttachment::File(Arc::new(file))),
                Err(error) => {
                    attachment.cancelled.store(true, Ordering::Relaxed);
                    attachment.error = Some(error);
                }
            }
        }
        if self.attachments.preparing.as_ref() == Some(&id) {
            self.attachments.preparing = None;
        }
        self.start_next_attachment(cx);
        cx.notify();
        true
    }
    pub(super) fn receive_attachment(
        &mut self,
        kind: &str,
        payload: &Value,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(request) = payload["requestId"].as_str() else {
            return false;
        };
        if !matches!(kind, "file.upload.response" | "rpc_error") {
            return false;
        }
        if !self.attachments.requests.contains_key(request) {
            return false;
        }
        let result = if let Some(error) = payload["error"].as_str() {
            Err(error.into())
        } else {
            serde_json::from_value::<UploadedFile>(payload["file"].clone())
                .map_err(|e| format!("Invalid upload response: {e}"))
        };
        self.finish_attachment(request, result, cx)
    }
    pub(super) fn attachment_disconnected(&mut self) {
        self.attachments.menu = false;
        self.attachments.preparing = None;
        self.attachments.requests.clear();
        for a in &mut self.state.attachments {
            if a.ready.is_none() {
                a.cancelled.store(true, Ordering::Relaxed);
                a.error =
                    Some("Host disconnected. Reconnect before retrying this attachment.".into());
            }
        }
    }
    pub(super) fn release_attachment_layout(&mut self) {
        self.attachments.menu = false;
        self.attachments.menu_focus = None;
        self.attachments.preview_id = None;
        self.attachments.preview = None;
        self.attachments.preview_focus = None;
    }
    pub(super) fn prune_attachment_previews(&mut self) {
        self.attachments
            .thumbnails
            .retain(|id, _| self.state.attachments.iter().any(|a| a.id == *id));
    }
    fn remove_attachment(&mut self, id: &str, cx: &mut Context<Self>) {
        self.state.attachments.retain(|a| a.id != id);
        self.attachments.thumbnails.remove(id);
        self.attachments
            .requests
            .retain(|_, attachment| attachment != id);
        if self.attachments.preparing.as_deref() == Some(id) {
            self.attachments.preparing = None;
            self.start_next_attachment(cx);
        }
        cx.notify();
    }
    fn retry_attachment(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(index) = self.state.attachments.iter().position(|a| a.id == id) else {
            return;
        };
        let old = &self.state.attachments[index];
        let mut next = DraftAttachment::new(old.path.clone());
        next.as_image = old.as_image;
        self.state.attachments[index] = next;
        self.start_next_attachment(cx);
        cx.notify();
    }
    pub(super) fn attachment_button(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let entity = cx.entity();
        div()
            .id("attach-files")
            .relative()
            .size(px(28.))
            .flex_shrink_0()
            .rounded_full()
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .when(!self.connected || self.attachments.picking, |v| {
                v.opacity(0.5)
            })
            .hover(|v| v.bg(gpui::rgba(0xffffff14)))
            .child(
                canvas(
                    move |bounds, _, cx| {
                        entity.update(cx, |this, _| this.attachments.button_bounds = bounds)
                    },
                    |_, (), _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .child(crate::chrome::icon("plus", 16.))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    cx.stop_propagation();
                    if this.connected && !this.attachments.picking {
                        this.picker = None;
                        this.attachments.menu = !this.attachments.menu;
                        if this.attachments.menu {
                            let focus = cx.focus_handle();
                            focus.focus(window, cx);
                            this.attachments.menu_focus = Some(focus);
                            this.attachments.menu_active = 0;
                        } else {
                            this.attachments.menu_focus = None;
                            this.focus.focus(window, cx);
                        }
                        cx.notify();
                    }
                }),
            )
            .into_any_element()
    }
    pub(super) fn attachment_menu(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if !self.attachments.menu {
            return None;
        }
        let bounds = self.attachments.button_bounds;
        let focus = self.attachments.menu_focus.as_ref()?;
        Some(
            gpui::deferred(
                gpui::anchored()
                    .position(point(bounds.left(), bounds.top() - px(8.)))
                    .anchor(gpui::Anchor::BottomLeft)
                    .snap_to_window_with_margin(px(8.))
                    .child(
                        div()
                            .id("attachment-menu")
                            .track_focus(focus)
                            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                                match event.keystroke.key.as_str() {
                                    "escape" => {
                                        this.attachments.menu = false;
                                        this.attachments.menu_focus = None;
                                        this.focus.focus(window, cx);
                                    }
                                    "up" | "down" => {
                                        this.attachments.menu_active =
                                            (this.attachments.menu_active + 1) % 2;
                                    }
                                    "enter" => this.pick_attachments(
                                        this.attachments.menu_active == 0,
                                        window,
                                        cx,
                                    ),
                                    _ => return,
                                }
                                window.prevent_default();
                                cx.stop_propagation();
                                cx.notify();
                            }))
                            .w(px(220.))
                            .rounded(px(8.))
                            .bg(rgb(crate::theme::colors::SURFACE0))
                            .border_1()
                            .border_color(rgb(crate::theme::colors::BORDER_ACCENT))
                            .overflow_hidden()
                            .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                                this.attachments.menu = false;
                                this.attachments.menu_focus = None;
                                this.focus.focus(window, cx);
                                cx.notify();
                            }))
                            .children(
                                [
                                    (true, "Add image", "image"),
                                    (false, "Upload file", "paperclip"),
                                ]
                                .into_iter()
                                .map(|(as_image, label, glyph)| {
                                    div()
                                        .id(label)
                                        .when(
                                            (as_image && self.attachments.menu_active == 0)
                                                || (!as_image && self.attachments.menu_active == 1),
                                            |row| row.bg(rgb(crate::theme::colors::SURFACE1)),
                                        )
                                        .min_h(px(36.))
                                        .px_3()
                                        .py_2()
                                        .flex()
                                        .gap_2()
                                        .items_center()
                                        .cursor_pointer()
                                        .hover(|v| v.bg(rgb(crate::theme::colors::SURFACE1)))
                                        .child(crate::chrome::icon(glyph, 16.))
                                        .child(label)
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(move |this, _, window, cx| {
                                                cx.stop_propagation();
                                                this.pick_attachments(as_image, window, cx);
                                            }),
                                        )
                                }),
                            ),
                    ),
            )
            .into_any_element(),
        )
    }
    pub(super) fn attachment_tray(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        div()
            .id("attachment-tray")
            .flex()
            .flex_wrap()
            .gap_2()
            .children(self.state.attachments.iter().map(|attachment| {
                let id = attachment.id.clone();
                let remove_id = id.clone();
                let retry_id = id.clone();
                let group: gpui::SharedString = format!("attachment-pill-{id}").into();
                let content = div()
                    .id(gpui::SharedString::from(format!("attachment-{id}")))
                    .group(group.clone())
                    .relative()
                    .flex_shrink_0()
                    .child(
                        div()
                            .h(px(CONTENT_HEIGHT + 2.))
                            .max_w(px(262.))
                            .rounded(px(6.))
                            .overflow_hidden()
                            .border_1()
                            .border_color(rgb(crate::theme::colors::BORDER_ACCENT))
                            .bg(rgb(crate::theme::colors::SURFACE1))
                            .flex()
                            .items_center()
                            .map(|pill| {
                                if let Some(image) = self.attachments.thumbnails.get(&id) {
                                    pill.child(
                                        img(image.clone())
                                            .size(px(CONTENT_HEIGHT))
                                            .object_fit(gpui::ObjectFit::Cover),
                                    )
                                } else {
                                    pill.px_3()
                                        .gap_2()
                                        .child(
                                            div()
                                                .w(px(18.))
                                                .flex_shrink_0()
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .child(crate::chrome::icon("file-text", 16.)),
                                        )
                                        .child(
                                            div()
                                                .min_w_0()
                                                .flex()
                                                .flex_col()
                                                .child(
                                                    div()
                                                        .truncate()
                                                        .child(attachment.file_name.clone()),
                                                )
                                                .child(
                                                    div()
                                                        .text_size(px(crate::theme::font_size::SM))
                                                        .text_color(rgb(
                                                            crate::theme::colors::FOREGROUND_MUTED,
                                                        ))
                                                        .child(if attachment.error.is_some() {
                                                            "Retry".into()
                                                        } else if attachment.ready.is_none() {
                                                            "Uploading…".into()
                                                        } else {
                                                            attachment
                                                                .path
                                                                .extension()
                                                                .map(|ext| {
                                                                    ext.to_string_lossy()
                                                                        .to_uppercase()
                                                                })
                                                                .unwrap_or_else(|| "File".into())
                                                        }),
                                                ),
                                        )
                                }
                            })
                            .when(
                                matches!(attachment.ready, Some(ReadyAttachment::Image(_))),
                                |pill| {
                                    pill.cursor_pointer().on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, _, window, cx| {
                                            cx.stop_propagation();
                                            this.open_attachment_image(&id, window, cx);
                                        }),
                                    )
                                },
                            )
                            .when(attachment.error.is_some() && self.connected, |pill| {
                                pill.cursor_pointer().on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.retry_attachment(&retry_id, cx);
                                    }),
                                )
                            }),
                    )
                    .child(
                        div()
                            .id(gpui::SharedString::from(format!(
                                "remove-attachment-{remove_id}"
                            )))
                            .absolute()
                            .top(px(-8.))
                            .left(px(-8.))
                            .size(px(24.))
                            .rounded_full()
                            .bg(rgb(crate::theme::colors::SURFACE2))
                            .border_1()
                            .border_color(rgb(crate::theme::colors::BORDER))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .opacity(0.)
                            .group_hover(group, |v| v.opacity(1.))
                            .child(crate::chrome::icon("x", 12.))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.remove_attachment(&remove_id, cx);
                                }),
                            ),
                    );
                div()
                    .flex()
                    .flex_col()
                    .max_w(px(262.))
                    .child(content)
                    .when_some(attachment.error.clone(), |column, error| {
                        column.child(
                            div()
                                .text_size(px(crate::theme::font_size::SM))
                                .text_color(rgb(crate::theme::colors::STATUS_DANGER))
                                .child(error),
                        )
                    })
            }))
            .into_any_element()
    }
}

fn render_image(thumbnail: paseo_desktop::attachments::Thumbnail) -> Arc<RenderImage> {
    let mut rgba = thumbnail.rgba;
    for pixel in rgba.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    let frame = image::Frame::new(
        image::RgbaImage::from_raw(thumbnail.width, thumbnail.height, rgba)
            .expect("bounded preview dimensions"),
    );
    Arc::new(RenderImage::new(vec![frame]))
}

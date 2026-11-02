//! Image loading for rendered markdown.
//!
//! Local images use the existing correlated file read: `file_explorer_request` with
//! `mode:"file"` and `acceptBinary:true`. The daemon answers with a JSON
//! `file_explorer_response` for errors or legacy payloads and with 0x10/0x11/0x12 file-transfer
//! frames for the bytes. The connection loop hands binary frames to [`ImageCache::receive_binary`]
//! unchanged, so this module owns the frame parser and never touches the transport.
//!
//! Expected UI dispatch:
//! - `Event::Binary(bytes)` -> `Conversation::receive_binary(bytes)` -> `ImageCache::receive_binary`
//! - every session message -> `images.update(cx, |images, cx| images.receive_session(kind, payload, cx))`
//! - disconnect -> `images.clear(cx)`
use crate::{
    message_style::MessageStyle,
    theme::{font_size, radius},
};
use anyhow::{Context as _, Result, ensure};
use base64::Engine as _;
use gpui::{Context, RenderImage, div, prelude::*, px, rgb};
use image::Frame;
use paseo_desktop::client::Connection;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
    time::Duration,
};

const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;
const MAX_READY_IMAGES: usize = 32;
const MAX_READY_BYTES: usize = 96 * 1024 * 1024;
const MAX_BASE64_CHARS: usize = ((MAX_IMAGE_BYTES as usize + 2) / 3) * 4;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone)]
pub enum ImageState {
    Missing,
    Loading,
    Ready(Arc<RenderImage>),
    Failed(String),
}

#[allow(dead_code)] // Read by the binary dispatch merged with the parent-owned Event::Binary.
struct Transfer {
    path: String,
    expected: Option<u64>,
    chunks: Vec<u8>,
    generation: u64,
}

pub struct ImageCache {
    connection: Arc<Connection>,
    states: BTreeMap<String, ImageState>,
    requests: BTreeMap<String, Transfer>,
    by_request: BTreeMap<String, String>,
    ready_order: VecDeque<String>,
    ready_bytes: usize,
    generation: u64,
}

impl ImageCache {
    pub fn new(connection: Arc<Connection>, _cx: &mut Context<Self>) -> Self {
        Self {
            connection,
            states: BTreeMap::new(),
            requests: BTreeMap::new(),
            by_request: BTreeMap::new(),
            ready_order: VecDeque::new(),
            ready_bytes: 0,
            generation: 0,
        }
    }
    pub fn state(&self, key: &str) -> ImageState {
        self.states.get(key).cloned().unwrap_or(ImageState::Missing)
    }
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        for transfer in self.requests.values() {
            self.states
                .insert(transfer.path.clone(), ImageState::Missing);
        }
        self.requests.clear();
        self.by_request.clear();
        cx.notify();
    }
    /// Forget unresolved requests when the workspace root changes so they are retried.
    pub fn retry_unresolved(&mut self, cx: &mut Context<Self>) {
        self.requests.clear();
        self.by_request.clear();
        self.states
            .retain(|_, state| matches!(state, ImageState::Ready(_)));
        self.ready_order
            .retain(|key| matches!(self.states.get(key), Some(ImageState::Ready(_))));
        self.ready_bytes = self.states.values().filter_map(state_bytes).sum();
        cx.notify();
    }
    /// Resolve and request an image once. HTTPS/data sources are handled without the daemon.
    pub fn ensure(&mut self, root: Option<&str>, source: &str, cx: &mut Context<Self>) {
        let source = normalize_source(source);
        let key = image_key(root, source);
        if self.states.contains_key(&key) {
            return;
        }
        if let Some(data) = source.strip_prefix("data:image/") {
            let data = data.to_owned();
            self.states.insert(key.clone(), ImageState::Loading);
            cx.notify();
            self.decode_later(key, move || decode_data_uri(&data), cx);
            return;
        }
        if source.starts_with("http://") || source.starts_with("https://") {
            self.states.insert(
                key,
                ImageState::Failed(
                    "Remote images are not supported in the native desktop yet".into(),
                ),
            );
            cx.notify();
            return;
        }
        let Some(root) = root.map(str::trim).filter(|root| !root.is_empty()) else {
            self.states.insert(
                key,
                ImageState::Failed("No workspace root is available for this image".into()),
            );
            cx.notify();
            return;
        };
        let Some(relative) = relative_path(root, source) else {
            self.states.insert(
                key,
                ImageState::Failed("The image is outside the workspace".into()),
            );
            cx.notify();
            return;
        };
        let request = json!({
            "cwd": root,
            "path": relative,
            "mode": "file",
            "acceptBinary": true,
            "maxBytes": MAX_IMAGE_BYTES,
        });
        let request_id = match self.connection.request("file_explorer_request", request) {
            Ok(id) => id,
            Err(error) => {
                self.states.insert(
                    key,
                    ImageState::Failed(format!("Could not request the image: {error}")),
                );
                cx.notify();
                return;
            }
        };
        self.generation = self.generation.wrapping_add(1);
        self.requests.insert(
            request_id.clone(),
            Transfer {
                path: key.clone(),
                expected: None,
                chunks: Vec::new(),
                generation: self.generation,
            },
        );
        self.by_request.insert(request_id.clone(), key.clone());
        self.states.insert(key, ImageState::Loading);
        cx.notify();
        let generation = self.generation;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(REQUEST_TIMEOUT).await;
            let _ = this.update(cx, |this, cx| {
                let Some(transfer) = this.requests.get(&request_id) else {
                    return;
                };
                if transfer.generation != generation {
                    return;
                }
                let path = transfer.path.clone();
                this.requests.remove(&request_id);
                this.by_request.remove(&request_id);
                this.set_state(
                    path,
                    ImageState::Failed("Timed out while loading the image".into()),
                );
                cx.notify();
            });
        })
        .detach();
    }
    /// Handle session messages that correlate with an image request. Returns true when consumed.
    pub fn receive_session(&mut self, kind: &str, payload: &Value, cx: &mut Context<Self>) -> bool {
        if !matches!(kind, "file_explorer_response" | "rpc_error") {
            return false;
        }
        let Some(request_id) = payload["requestId"].as_str() else {
            return false;
        };
        let Some(path) = self.by_request.get(request_id).cloned() else {
            return false;
        };
        if kind == "rpc_error" {
            let message = payload["error"]
                .as_str()
                .filter(|message| !message.is_empty())
                .unwrap_or("The host rejected the image request.")
                .to_owned();
            self.fail(&request_id, path, message, cx);
            return true;
        }
        if let Some(error) = payload["error"].as_str().filter(|error| !error.is_empty()) {
            self.fail(&request_id, path, error.to_owned(), cx);
            return true;
        }
        if let Some(file) = payload.get("file").filter(|file| !file.is_null()) {
            if file["kind"] == "image"
                && let Some(content) = file["content"].as_str()
                && file["encoding"] == "base64"
            {
                let content = content.to_owned();
                self.requests.remove(request_id);
                self.by_request.remove(request_id);
                self.decode_later(path, move || decode_base64_image(&content), cx);
                return true;
            }
            self.fail(
                request_id,
                path,
                "The host returned an unsupported image payload".into(),
                cx,
            );
            return true;
        }
        // No file and no error: the binary frames carry the bytes; keep the transfer alive.
        true
    }
    /// Parse a file-transfer frame: [opcode, requestIdLen, requestId, body].
    pub fn receive_binary(&mut self, bytes: &[u8], cx: &mut Context<Self>) -> bool {
        let Some(opcode) = bytes.first().copied() else {
            return false;
        };
        if !(0x10..=0x12).contains(&opcode) {
            return false;
        }
        let Some(length) = bytes.get(1).copied().map(usize::from) else {
            return false;
        };
        if length == 0 || bytes.len() < 2 + length {
            return false;
        }
        let Ok(request_id) = std::str::from_utf8(&bytes[2..2 + length]) else {
            return false;
        };
        let request_id = request_id.to_owned();
        let body = &bytes[2 + length..];
        let Some(path) = self.by_request.get(&request_id).cloned() else {
            // Multiple conversation caches share this stream. Only the cache
            // that issued the request may consume its frames.
            return false;
        };
        match opcode {
            0x10 => {
                if body.len() < 2 {
                    return true;
                }
                let metadata_length = usize::from(u16::from_be_bytes([body[0], body[1]]));
                if body.len() < 2 + metadata_length {
                    return true;
                }
                let Ok(metadata) = serde_json::from_slice::<Value>(&body[2..2 + metadata_length])
                else {
                    self.fail(&request_id, path, "Invalid image metadata".into(), cx);
                    return true;
                };
                let size = metadata["size"].as_u64();
                if size.is_some_and(|size| size > MAX_IMAGE_BYTES) {
                    self.fail(&request_id, path, "The image is too large".into(), cx);
                    return true;
                }
                if metadata["mime"]
                    .as_str()
                    .is_some_and(|mime| !mime.starts_with("image/"))
                {
                    self.fail(&request_id, path, "The file is not an image".into(), cx);
                    return true;
                }
                if let Some(transfer) = self.requests.get_mut(&request_id) {
                    transfer.expected = size;
                }
                true
            }
            0x11 => {
                if let Some(transfer) = self.requests.get_mut(&request_id) {
                    if transfer.chunks.len() + body.len() > MAX_IMAGE_BYTES as usize {
                        self.fail(&request_id, path, "The image is too large".into(), cx);
                        return true;
                    }
                    transfer.chunks.extend_from_slice(body);
                }
                true
            }
            _ => {
                let Some(transfer) = self.requests.remove(&request_id) else {
                    return true;
                };
                self.by_request.remove(&request_id);
                if let Some(expected) = transfer.expected
                    && transfer.chunks.len() as u64 != expected
                {
                    self.set_state(
                        path,
                        ImageState::Failed("The image transfer was incomplete".into()),
                    );
                    cx.notify();
                    return true;
                }
                self.decode_later(path, move || decode_bytes(&transfer.chunks), cx);
                true
            }
        }
    }
    fn fail(&mut self, request_id: &str, path: String, error: String, cx: &mut Context<Self>) {
        self.requests.remove(request_id);
        self.by_request.remove(request_id);
        self.set_state(path, ImageState::Failed(error));
        cx.notify();
    }
    fn decode_later(
        &mut self,
        path: String,
        decode: impl FnOnce() -> Result<Arc<RenderImage>> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            let decoded = cx.background_spawn(async move { decode() }).await;
            let _ = this.update(cx, |this, cx| {
                let state = match decoded {
                    Ok(image) => ImageState::Ready(image),
                    Err(error) => ImageState::Failed(error.to_string()),
                };
                this.set_state(path, state);
                cx.notify();
            });
        })
        .detach();
    }
    fn set_state(&mut self, key: String, state: ImageState) {
        if let Some(bytes) = self.states.get(&key).and_then(state_bytes) {
            self.ready_bytes = self.ready_bytes.saturating_sub(bytes);
            self.ready_order.retain(|entry| entry != &key);
        }
        if let Some(bytes) = state_bytes(&state) {
            self.ready_bytes = self.ready_bytes.saturating_add(bytes);
            self.ready_order.push_back(key.clone());
        }
        self.states.insert(key, state);
        trim_ready(
            &mut self.states,
            &mut self.ready_order,
            &mut self.ready_bytes,
            MAX_READY_IMAGES,
            MAX_READY_BYTES,
        );
    }
}

fn state_bytes(state: &ImageState) -> Option<usize> {
    match state {
        ImageState::Ready(image) => Some(render_image_bytes(image)),
        _ => None,
    }
}

fn render_image_bytes(image: &RenderImage) -> usize {
    let size = image.size(0);
    size.width.0.max(0) as usize * size.height.0.max(0) as usize * 4
}

/// Drop the oldest decoded images past both limits. Evicted keys leave the map so a later
/// `ensure` can load them again instead of pinning a permanent placeholder.
fn trim_ready(
    states: &mut BTreeMap<String, ImageState>,
    order: &mut VecDeque<String>,
    bytes: &mut usize,
    limit_images: usize,
    limit_bytes: usize,
) {
    while order.len() > limit_images || *bytes > limit_bytes {
        let Some(key) = order.pop_front() else {
            break;
        };
        if let Some(ImageState::Ready(image)) = states.remove(&key) {
            *bytes = bytes.saturating_sub(render_image_bytes(&image));
        }
    }
}

/// Strip markdown's `file://` scheme so prefetch, error surfacing, and rendering share one key.
fn normalize_source(source: &str) -> &str {
    let source = source.trim();
    source.strip_prefix("file://").unwrap_or(source)
}

pub fn image_key(root: Option<&str>, source: &str) -> String {
    let source = normalize_source(source);
    if source.starts_with("data:") {
        // Data URIs can be megabytes; hash the payload instead of retaining it as a key.
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        source.hash(&mut hasher);
        return format!("data:{:x}", hasher.finish());
    }
    match root {
        Some(root) if !source.starts_with('/') && !is_windows_absolute(source) => {
            join_path(root, source)
        }
        _ => source.to_owned(),
    }
}

fn is_windows_absolute(source: &str) -> bool {
    source.as_bytes().get(1) == Some(&b':') && source.as_bytes().get(2) == Some(&b'\\')
}

fn join_path(root: &str, relative: &str) -> String {
    let mut segments = root
        .trim_end_matches(['/', '\\'])
        .split(['/', '\\'])
        .filter(|segment| !segment.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    for segment in relative.split(['/', '\\']) {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            segment => segments.push(segment.to_owned()),
        }
    }
    let separator = if root.contains('\\') { "\\" } else { "/" };
    let prefix = if root.starts_with('/') { "/" } else { "" };
    format!("{prefix}{}", segments.join(separator))
}

fn relative_path(root: &str, source: &str) -> Option<String> {
    let root = root.trim_end_matches(['/', '\\']);
    let source = normalize_source(source);
    if source.starts_with('/') || is_windows_absolute(source) {
        let prefix = format!("{root}/");
        let windows = format!("{root}\\");
        return source
            .strip_prefix(&prefix)
            .or_else(|| source.strip_prefix(&windows))
            .map(str::to_owned);
    }
    Some(source.to_owned())
}

fn decode_data_uri(data: &str) -> Result<Arc<RenderImage>> {
    let (metadata, payload) = data.split_once(',').context("Malformed image data URI")?;
    ensure!(
        metadata.contains("base64"),
        "Only base64 data images are supported"
    );
    decode_base64_image(payload)
}

fn decode_base64_image(content: &str) -> Result<Arc<RenderImage>> {
    decode_base64_image_with_limit(content, MAX_BASE64_CHARS)
}

/// Reject an oversized payload before allocating the decoded buffer.
fn decode_base64_image_with_limit(content: &str, max_chars: usize) -> Result<Arc<RenderImage>> {
    let content = content.trim();
    ensure!(content.len() <= max_chars, "The image is too large");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(content)
        .context("Invalid base64 image data")?;
    decode_bytes(&bytes)
}

fn decode_bytes(bytes: &[u8]) -> Result<Arc<RenderImage>> {
    ensure!(!bytes.is_empty(), "The image is empty");
    ensure!(
        bytes.len() as u64 <= MAX_IMAGE_BYTES,
        "The image is too large"
    );
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode().context("Cannot decode this image format")?;
    let image = image
        .thumbnail(2048.min(image.width()), 2048.min(image.height()))
        .to_rgba8();
    let (width, height) = image.dimensions();
    let mut rgba = image.into_raw();
    for pixel in rgba.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    let frame = Frame::new(
        image::RgbaImage::from_raw(width, height, rgba).context("Invalid image dimensions")?,
    );
    Ok(Arc::new(RenderImage::new(vec![frame])))
}

/// The inline markdown image, rendered from the shared cache.
pub fn render_image(
    cache: &ImageCache,
    source: &str,
    root: Option<&str>,
    style: &MessageStyle,
) -> gpui::AnyElement {
    match cache.state(&image_key(root, source)) {
        ImageState::Ready(image) => {
            let size = image.size(0);
            let width = size.width.0 as f32;
            let height = size.height.0 as f32;
            let scale = (480. / width).min(400. / height).min(1.).max(0.01);
            div()
                .rounded(px(radius::MD))
                .border_1()
                .border_color(rgb(style.palette.border))
                .overflow_hidden()
                .child(gpui::img(image).w(px(width * scale)).h(px(height * scale)))
                .into_any_element()
        }
        ImageState::Loading => image_placeholder("Loading image…", style),
        ImageState::Missing => image_placeholder("Image", style),
        ImageState::Failed(error) => div()
            .w_full()
            .max_w(px(420.))
            .p_2()
            .rounded(px(radius::MD))
            .border_1()
            .border_color(rgb(style.palette.border))
            .bg(rgb(style.palette.surface1))
            .text_size(px(font_size::SM))
            .text_color(rgb(style.palette.foreground_muted))
            .child(error)
            .into_any_element(),
    }
}

fn image_placeholder(label: &'static str, style: &MessageStyle) -> gpui::AnyElement {
    div()
        .w_full()
        .max_w(px(360.))
        .h(px(96.))
        .rounded(px(radius::MD))
        .border_1()
        .border_color(rgb(style.palette.border))
        .bg(rgb(style.palette.surface1))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(font_size::SM))
        .text_color(rgb(style.palette.foreground_muted))
        .child(label)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_keys_resolve_relative_sources_and_keep_absolute_paths() {
        assert_eq!(
            image_key(Some("/work/repo"), "images/a.png"),
            "/work/repo/images/a.png"
        );
        assert_eq!(
            image_key(Some("/work/repo"), "./images/../b.png"),
            "/work/repo/b.png"
        );
        assert_eq!(
            image_key(Some("/work/repo"), "/other/c.png"),
            "/other/c.png"
        );
        assert_eq!(
            relative_path("/work/repo", "/work/repo/images/a.png").as_deref(),
            Some("images/a.png")
        );
        assert_eq!(relative_path("/work/repo", "/tmp/a.png"), None);
        assert_eq!(
            relative_path("/work/repo", "images/a.png").as_deref(),
            Some("images/a.png")
        );
        assert_eq!(
            image_key(Some("/work/repo"), "file:///work/repo/images/a.png"),
            "/work/repo/images/a.png"
        );
        assert_eq!(
            image_key(Some("/work/repo"), "file://images/a.png"),
            "/work/repo/images/a.png"
        );
        assert_eq!(
            relative_path("/work/repo", "file:///work/repo/images/a.png").as_deref(),
            Some("images/a.png")
        );
        assert_eq!(relative_path("/work/repo", "file:///other/c.png"), None);
        assert!(image_key(Some("/work/repo"), "data:image/png;base64,AAAA").starts_with("data:"));
    }

    #[test]
    fn decoded_images_leave_the_cache_at_both_limits() {
        let mut states = BTreeMap::new();
        let mut order = VecDeque::new();
        let mut bytes = 0;
        for index in 0..3 {
            let key = format!("image-{index}");
            let image = tiny_image();
            bytes += render_image_bytes(&image);
            states.insert(key.clone(), ImageState::Ready(image));
            order.push_back(key);
        }
        trim_ready(&mut states, &mut order, &mut bytes, 2, usize::MAX);
        assert_eq!(states.len(), 2);
        assert!(!states.contains_key("image-0"));
        assert_eq!(bytes, 8);

        let mut states = BTreeMap::new();
        let mut order = VecDeque::new();
        let mut bytes = 0;
        for index in 0..3 {
            let key = format!("image-{index}");
            let image = tiny_image();
            bytes += render_image_bytes(&image);
            states.insert(key.clone(), ImageState::Ready(image));
            order.push_back(key);
        }
        trim_ready(&mut states, &mut order, &mut bytes, usize::MAX, 8);
        assert_eq!(states.len(), 2);
        assert!(!states.contains_key("image-0"));
        assert!(states.contains_key("image-2"));
        assert_eq!(bytes, 8);
    }

    #[test]
    fn oversized_base64_is_rejected_before_decoding() {
        let error = decode_base64_image_with_limit("!!!!", 3)
            .unwrap_err()
            .to_string();
        assert_eq!(error, "The image is too large");
    }

    #[test]
    fn tiny_png_data_uri_decodes_to_a_render_image() {
        let image = tiny_image();
        assert_eq!(image.size(0).width.0, 1);
        assert_eq!(image.size(0).height.0, 1);
        assert!(decode_data_uri("image/png;base64,not-base64!").is_err());
    }

    fn tiny_image() -> Arc<RenderImage> {
        // 1x1 transparent PNG.
        let uri = "image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
        decode_data_uri(uri).unwrap()
    }
}

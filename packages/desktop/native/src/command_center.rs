use crate::{
    chrome::{icon, loading_spinner},
    file_icon_view::FileIconCache,
    inline_input::{InlineInput, InlineInputEvent},
    settings_view::{SharedSettings, active_palette},
};
use gpui::{
    App, BoxShadow, Context, Entity, EventEmitter, Focusable, ListAlignment, ListState,
    MouseButton, Render, Subscription, Window, div, list, prelude::*, px, rgb, rgba,
};
use paseo_desktop::{
    client::Connection,
    command_center_model::{Row, Scope, Search, Target},
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
#[path = "command_center_presentation.rs"]
mod p;

pub enum Command {
    Cancel,
    Pick(Target),
}
#[derive(Clone, Debug, PartialEq)]
enum Item {
    Section(String),
    Result(Row),
}

fn styled_parts(
    parts: &[(&str, u32)],
    query: &str,
    palette: &paseo_desktop::appearance::Palette,
) -> gpui::StyledText {
    let mut text = String::new();
    let mut styles = vec![];
    for (part, color) in parts {
        let offset = text.len();
        text.push_str(part);
        let mut cursor = 0;
        for range in paseo_desktop::command_center_match::byte_highlights(query, part) {
            if cursor < range.start {
                styles.push((
                    offset + cursor..offset + range.start,
                    gpui::HighlightStyle {
                        color: Some(rgb(*color).into()),
                        ..Default::default()
                    },
                ));
            }
            styles.push((
                offset + range.start..offset + range.end,
                gpui::HighlightStyle {
                    color: Some(rgb(palette.foreground).into()),
                    background_color: Some(rgb(palette.surface3).into()),
                    ..Default::default()
                },
            ));
            cursor = range.end;
        }
        if cursor < part.len() {
            styles.push((
                offset + cursor..offset + part.len(),
                gpui::HighlightStyle {
                    color: Some(rgb(*color).into()),
                    ..Default::default()
                },
            ));
        }
    }
    gpui::StyledText::new(text).with_highlights(styles)
}

pub struct CommandCenter {
    connection: Arc<Connection>,
    shared: SharedSettings,
    input: Entity<InlineInput>,
    _input_events: Subscription,
    state: Search,
    open: bool,
    cwd: Option<String>,
    builtins: Arc<Vec<Row>>,
    results: Vec<Row>,
    items: Vec<Item>,
    list: ListState,
    icons: FileIconCache,
    // Retained across closes/source changes, so late RPC errors stay owned.
    // A stalled host cannot accumulate unbounded request metadata.
    requests: BTreeMap<String, u64>,
    providers: BTreeMap<String, Arc<gpui::RenderImage>>,
    reveal_pending: bool,
}
impl EventEmitter<Command> for CommandCenter {}
impl Focusable for CommandCenter {
    fn focus_handle(&self, cx: &App) -> gpui::FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }
}
impl CommandCenter {
    pub fn new(
        connection: Arc<Connection>,
        shared: SharedSettings,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| InlineInput::command_center(shared.clone(), cx));
        let events = cx.subscribe(
            &input,
            |this, _, event: &InlineInputEvent, cx| match event {
                InlineInputEvent::Changed => {
                    if this.input.read(cx).is_composing() {
                        return;
                    }
                    let query = this.input.read(cx).text().to_owned();
                    if let Some(generation) = this.state.set_query(query) {
                        this.reveal_pending = true;
                        this.schedule(generation, cx);
                        this.project(cx);
                    }
                }
                InlineInputEvent::Submit => this.pick_active(cx),
            },
        );
        Self {
            connection,
            shared,
            input,
            _input_events: events,
            state: Search::new(Scope::All),
            open: false,
            cwd: None,
            builtins: Arc::new(vec![]),
            results: vec![],
            items: vec![],
            list: ListState::new(0, ListAlignment::Top, px(100.)),
            icons: Default::default(),
            requests: BTreeMap::new(),
            providers: BTreeMap::new(),
            reveal_pending: true,
        }
    }
    pub fn is_open(&self) -> bool {
        self.open
    }
    pub fn set_providers(
        &mut self,
        providers: &BTreeMap<String, Arc<gpui::RenderImage>>,
        cx: &mut Context<Self>,
    ) {
        if self.providers.len() == providers.len()
            && providers.iter().all(|(key, image)| {
                self.providers
                    .get(key)
                    .is_some_and(|old| Arc::ptr_eq(old, image))
            })
        {
            return;
        }
        self.providers = providers.clone();
        cx.notify();
    }
    pub fn open(
        &mut self,
        scope: Scope,
        cwd: Option<String>,
        rows: Arc<Vec<Row>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open = true;
        self.reveal_pending = true;
        self.cwd = cwd;
        self.builtins = rows;
        let generation = self.state.reopen(scope);
        self.input.update(cx, |input, cx| {
            input.set_text("", cx);
            input.set_placeholder(
                if scope == Scope::Files {
                    "Search files..."
                } else {
                    "Search commands, files, workspaces, and agents..."
                }
                .into(),
                cx,
            );
        });
        self.schedule(generation, cx);
        self.project(cx);
        self.list.scroll_to(gpui::ListOffset::default());
        self.focus_handle(cx).focus(window, cx);
        cx.notify();
    }
    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.open = false;
        self.state.close();
        self.results.clear();
        self.items.clear();
        self.list.reset(0);
        cx.notify();
    }
    pub fn update_context(
        &mut self,
        cwd: Option<String>,
        rows: Arc<Vec<Row>>,
        cx: &mut Context<Self>,
    ) {
        if !self.open {
            return;
        }
        let source_changed = self.cwd != cwd;
        let rows_changed = !Arc::ptr_eq(&self.builtins, &rows);
        if !source_changed && !rows_changed {
            return;
        }
        self.cwd = cwd;
        self.builtins = rows;
        if source_changed {
            self.state.files.clear();
            self.state.active = None;
            let generation = self.state.begin();
            self.schedule(generation, cx);
        }
        self.project(cx);
    }
    fn item_height(item: &Item, index: usize) -> f32 {
        match item {
            Item::Section(_) => p::SECTION_HEIGHT + if index > 0 { p::DIVIDER_HEIGHT } else { 0. },
            Item::Result(row) => {
                if matches!(row.target, Target::Workspace(_) | Target::Agent(_))
                    || (!matches!(row.target, Target::File(_)) && !row.subtitle.is_empty())
                {
                    p::TWO_ROW_HEIGHT
                } else {
                    p::SINGLE_ROW_HEIGHT
                }
            }
        }
    }
    fn logical_offset(items: &[Item], mut pixel: f32) -> gpui::ListOffset {
        for (index, item) in items.iter().enumerate() {
            let height = Self::item_height(item, index);
            if pixel < height {
                return gpui::ListOffset {
                    item_ix: index,
                    offset_in_item: px(pixel.max(0.)),
                };
            }
            pixel -= height;
        }
        gpui::ListOffset {
            item_ix: items.len().saturating_sub(1),
            offset_in_item: px(0.),
        }
    }
    fn reveal_index(&mut self, index: usize) -> bool {
        let viewport = self.list.viewport_bounds().size.height.to_f64() as f32;
        if viewport <= 0. {
            return false;
        }
        let current = self.list.logical_scroll_top();
        let mut top = 0.;
        let mut start = 0.;
        for (i, item) in self.items.iter().enumerate() {
            let height = Self::item_height(item, i);
            if i < current.item_ix {
                top += height;
            }
            if i < index {
                start += height;
            }
        }
        top += current.offset_in_item.to_f64() as f32;
        let bottom = start + Self::item_height(&self.items[index], index);
        let desired = if start < top {
            start
        } else if bottom > top + viewport {
            bottom - viewport
        } else {
            top
        };
        self.list
            .scroll_to(Self::logical_offset(&self.items, desired));
        true
    }
    fn project(&mut self, cx: &mut Context<Self>) {
        self.results = self.state.rows(&self.builtins);
        self.state.select_first(&self.results);
        let mut items = vec![];
        let mut section = "";
        for row in &self.results {
            if row.section() != section {
                section = row.section();
                items.push(Item::Section(section.into()));
            }
            items.push(Item::Result(row.clone()));
        }
        if self.items != items {
            let top = self.list.logical_scroll_top();
            let pixel = self
                .items
                .iter()
                .enumerate()
                .take(top.item_ix)
                .map(|(index, item)| Self::item_height(item, index))
                .sum::<f32>()
                + top.offset_in_item.to_f64() as f32;
            let old = self.items.len();
            self.items = items;
            self.list.splice(0..old, self.items.len());
            self.list
                .clone()
                .with_uniform_item_height(px(p::SINGLE_ROW_HEIGHT));
            if !self.items.is_empty() {
                self.list
                    .scroll_to(Self::logical_offset(&self.items, pixel));
            }
        }
        if self.reveal_pending && !(self.state.scope == Scope::Files && self.state.loading) {
            if let Some(index) = self.items.iter().position(
                |item| matches!(item,Item::Result(row)if Some(&row.id)==self.state.active.as_ref()),
            ) {
                if self.reveal_index(index) {
                    self.reveal_pending = false;
                }
            } else {
                self.reveal_pending = false;
            }
        }
        cx.notify();
    }
    fn schedule(&mut self, generation: u64, cx: &mut Context<Self>) {
        if !self.open || !self.state.enabled() || self.cwd.is_none() {
            self.state.loading = false;
            return;
        }
        let delay = if paseo_desktop::command_center_match::query_is_empty(&self.state.query) {
            0
        } else {
            100
        };
        let timer = cx.background_executor().timer(Duration::from_millis(delay));
        cx.spawn(async move |this, cx| {
            timer.await;
            let _ = this.update(cx, |this, cx| this.issue(generation, cx));
        })
        .detach();
    }
    fn issue(&mut self, generation: u64, cx: &mut Context<Self>) {
        if !self.open || generation != self.state.generation {
            return;
        }
        if self.requests.len() >= 64 {
            self.state
                .fail(generation, "Search is busy. Try again.".into());
            self.project(cx);
            return;
        }
        let request = json!({"cwd":self.cwd,"query":self.state.query,"includeFiles":true,"includeDirectories":false,"limit":100});
        match self
            .connection
            .request("directory_suggestions_request", request)
        {
            Ok(id) => {
                self.requests.insert(id.clone(), generation);
                let timer = cx.background_executor().timer(Duration::from_secs(30));
                cx.spawn(async move |this, cx| {
                    timer.await;
                    let _ = this.update(cx, |this, cx| {
                        if this.requests.contains_key(&id)
                            && this.state.fail(generation, "File search timed out.".into())
                        {
                            this.project(cx);
                        }
                    });
                })
                .detach();
            }
            Err(error) => {
                self.state.fail(generation, error.to_string());
                self.project(cx);
            }
        }
    }
    pub fn receive(&mut self, kind: &str, payload: &Value, cx: &mut Context<Self>) -> bool {
        if !matches!(kind, "directory_suggestions_response" | "rpc_error") {
            return false;
        }
        let Some(id) = payload["requestId"].as_str() else {
            return false;
        };
        let Some(generation) = self.requests.remove(id) else {
            return false;
        };
        if self.state.accept(generation, payload) {
            self.project(cx);
        }
        true
    }
    pub fn disconnected(&mut self, cx: &mut Context<Self>) {
        self.requests.clear();
        let generation = self.state.generation;
        self.state.fail(generation, "The host disconnected.".into());
        self.project(cx);
    }
    pub fn reconnected(&mut self, cx: &mut Context<Self>) {
        if self.open {
            let generation = self.state.begin();
            self.schedule(generation, cx);
            self.project(cx);
        }
    }
    fn pick_active(&mut self, cx: &mut Context<Self>) {
        if self.state.scope == Scope::Files && self.state.loading {
            return;
        }
        if let Some(row) = self
            .results
            .iter()
            .find(|row| Some(&row.id) == self.state.active.as_ref())
        {
            cx.emit(Command::Pick(row.target.clone()));
        }
    }
    fn clear_scope(&mut self, cx: &mut Context<Self>) {
        let was_enabled = self.state.enabled();
        self.state.scope = Scope::All;
        self.reveal_pending = true;
        self.state.active = None;
        self.input.update(cx, |input, cx| {
            input.set_placeholder(
                "Search commands, files, workspaces, and agents...".into(),
                cx,
            )
        });
        if was_enabled != self.state.enabled() {
            let generation = self.state.begin();
            self.schedule(generation, cx);
        }
        self.project(cx);
    }
    fn key(&mut self, event: &gpui::KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.input.read(cx).is_composing() {
            return;
        }
        match event.keystroke.key.as_str() {
            "escape" => cx.emit(Command::Cancel),
            "backspace"
                if paseo_desktop::command_center_match::query_is_empty(&self.state.query)
                    && self.state.scope == Scope::Files =>
            {
                self.clear_scope(cx)
            }
            "up" | "down" => {
                if self.state.move_selection(
                    &self.results,
                    if event.keystroke.key == "up" { -1 } else { 1 },
                ) {
                    if let Some(index)=self.items.iter().position(|item|matches!(item,Item::Result(row)if Some(&row.id)==self.state.active.as_ref())){self.reveal_index(index);}
                    cx.notify();
                }
            }
            _ => return,
        }
        cx.stop_propagation();
        window.prevent_default();
    }
    fn render_item(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let palette = active_palette(&self.shared, window);
        // A previously prepared list range may finish after the overlay closes.
        let Some(item) = self.items.get(index).cloned() else {
            return div().into_any_element();
        };
        match item {
            Item::Section(label) => div()
                .w_full()
                .h(px(
                    p::SECTION_HEIGHT + if index > 0 { p::DIVIDER_HEIGHT } else { 0. }
                ))
                .flex_shrink_0()
                .flex()
                .flex_col()
                .justify_end()
                .when(index > 0, |v| {
                    v.child(
                        div()
                            .h(px(1.))
                            .mb(px(p::SECTION_GAP))
                            .bg(rgb(palette.border)),
                    )
                })
                .child(
                    div()
                        .px(px(p::ROW_PADDING_X))
                        .pb(px(p::SECTION_GAP))
                        .text_size(px(p::SUBTITLE_FONT))
                        .line_height(px(p::SECTION_LINE_HEIGHT))
                        .text_color(rgb(palette.foreground_muted))
                        .child(label),
                )
                .into_any_element(),
            Item::Result(row) => {
                let active = self.state.active.as_deref() == Some(&row.id);
                let target = row.target.clone();
                let file = matches!(row.target, Target::File(_));
                let two_line = matches!(row.target, Target::Workspace(_) | Target::Agent(_))
                    || (!file && !row.subtitle.is_empty());
                let glyph = if file {
                    self.icons.render(&row.title, 16., window, cx)
                } else if matches!(row.target, Target::Agent(_)) {
                    crate::chrome::provider_icon_data(&row.icon, 16., self.providers.get(&row.icon))
                } else {
                    icon(&row.icon, 16.).into_any_element()
                };
                div()
                    .id(gpui::SharedString::from(row.id))
                    .w_full()
                    .h(px(if two_line {
                        p::TWO_ROW_HEIGHT
                    } else {
                        p::SINGLE_ROW_HEIGHT
                    }))
                    .px(px(p::ROW_PADDING_X))
                    .flex()
                    .items_start()
                    .py(px(p::ROW_INSET))
                    .gap(px(p::ROW_GAP))
                    .when(active, |row| row.bg(rgb(palette.surface1)))
                    .hover(|row| row.bg(rgb(palette.surface1)))
                    .cursor_pointer()
                    .child(
                        div()
                            .w(px(16.))
                            .h(px(p::SINGLE_LINE_HEIGHT))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(glyph),
                    )
                    .child(if file {
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .text_size(px(p::TITLE_FONT))
                            .line_height(px(p::SINGLE_LINE_HEIGHT))
                            .child(styled_parts(
                                &[
                                    (&row.title, palette.foreground),
                                    (
                                        &if row.subtitle.is_empty() {
                                            String::new()
                                        } else {
                                            format!(" {}", row.subtitle)
                                        },
                                        palette.foreground_muted,
                                    ),
                                ],
                                &self.state.query,
                                &palette,
                            ))
                            .into_any_element()
                    } else {
                        div()
                            .min_w_0()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(p::TITLE_FONT))
                                    .line_height(px(p::TITLE_LINE_HEIGHT))
                                    .child(styled_parts(
                                        &[(&row.title, palette.foreground)],
                                        &self.state.query,
                                        &palette,
                                    )),
                            )
                            .when(two_line, |v| {
                                v.child(
                                    div()
                                        .truncate()
                                        .text_size(px(p::SUBTITLE_FONT))
                                        .line_height(px(p::SUBTITLE_LINE_HEIGHT))
                                        .text_color(rgb(palette.foreground_muted))
                                        .child(styled_parts(
                                            &[(&row.subtitle, palette.foreground_muted)],
                                            &self.state.query,
                                            &palette,
                                        )),
                                )
                            })
                            .into_any_element()
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |_, _, _, cx| {
                            cx.stop_propagation();
                            cx.emit(Command::Pick(target.clone()));
                        }),
                    )
                    .into_any_element()
            }
        }
    }
}
impl Render for CommandCenter {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = active_palette(&self.shared, window);
        let family = crate::fonts::ui_family_for(&self.shared.get().ui_font_family);
        let normal_line = crate::fonts::strut_for(window, p::TITLE_FONT, &family);
        let viewport = window.viewport_size();
        let width = p::WIDTH.min(viewport.width.to_f64() as f32 * p::MAX_WIDTH_RATIO);
        let height = p::HEIGHT.min(viewport.height.to_f64() as f32 * p::MAX_HEIGHT_RATIO);
        let observer = cx.entity();
        let observe = gpui::canvas(
            |_, _, _| {},
            move |_, _, window, _| {
                window.on_mouse_event(move |event: &gpui::ScrollEvent, phase, _, cx| {
                    if phase.capture() {
                        let _ = observer.update(cx, |this, _| {
                            if this.list.viewport_bounds().contains(&event.position) {
                                this.reveal_pending = false;
                            }
                        });
                    }
                });
            },
        )
        .absolute()
        .size_full();
        div()
            .absolute()
            .inset_0()
            .occlude()
            .bg(rgba(0x00000080))
            .flex()
            .items_start()
            .justify_center()
            .pt(px(p::TOP))
            .child(observe)
            .on_key_down(cx.listener(Self::key))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| {
                    cx.stop_propagation();
                    cx.emit(Command::Cancel);
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|_, _, _, cx| cx.stop_propagation()),
            )
            .child(
                div()
                    .w(px(width))
                    .h(px(height))
                    .rounded(px(p::RADIUS))
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.surface0))
                    .shadow(vec![
                        BoxShadow::new(
                            px(if palette.is_dark {
                                p::DARK_SHADOW_X
                            } else {
                                p::LIGHT_SHADOW_X
                            }),
                            px(if palette.is_dark {
                                p::DARK_SHADOW_Y
                            } else {
                                p::LIGHT_SHADOW_Y
                            }),
                            rgba(if palette.is_dark {
                                p::DARK_SHADOW_COLOR
                            } else {
                                p::LIGHT_SHADOW_COLOR
                            })
                            .into(),
                        )
                        .blur_radius(px(if palette.is_dark {
                            p::DARK_SHADOW_BLUR
                        } else {
                            p::LIGHT_SHADOW_BLUR
                        })),
                    ])
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .text_color(rgb(palette.foreground))
                    .line_height(normal_line)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|_, _, _, cx| cx.stop_propagation()),
                    )
                    .child(
                        div()
                            .px(px(p::HEADER_PADDING_X))
                            .py(px(p::HEADER_PADDING_Y))
                            .flex()
                            .items_center()
                            .gap(px(p::HEADER_GAP))
                            .border_b_1()
                            .border_color(rgb(palette.border))
                            .when(self.state.scope == Scope::Files, |row| {
                                row.child(
                                    div()
                                        .id("command-center-files-scope")
                                        .h(normal_line + px(8.))
                                        .px(px(p::CHIP_PADDING_X))
                                        .mr(px(p::CHIP_MARGIN_RIGHT))
                                        .rounded(px(p::CHIP_RADIUS))
                                        .border_1()
                                        .border_color(rgb(palette.border_accent))
                                        .bg(rgb(palette.surface2))
                                        .flex()
                                        .items_center()
                                        .gap(px(p::CHIP_GAP))
                                        .cursor_pointer()
                                        .child(icon("folder", 14.))
                                        .child(
                                            div()
                                                .text_size(px(p::TITLE_FONT))
                                                .font_weight(gpui::FontWeight::MEDIUM)
                                                .child("Files"),
                                        )
                                        .child(icon("x", 12.))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.clear_scope(cx);
                                            }),
                                        ),
                                )
                            })
                            .child(div().flex_1().min_w_0().child(self.input.clone()))
                            .child(div().size(px(14.)).when(self.state.loading, |v| {
                                v.child(loading_spinner(14., palette.foreground_muted))
                            })),
                    )
                    .when_some(self.state.error.clone(), |panel, error| {
                        panel.child(
                            div()
                                .px(px(p::HEADER_PADDING_X))
                                .py(px(p::HEADER_PADDING_Y))
                                .border_b_1()
                                .border_color(rgb(palette.border))
                                .text_size(px(p::TITLE_FONT))
                                .text_color(rgb(palette.status_danger))
                                .child(format!("Error: {error}")),
                        )
                    })
                    .child(
                        if self.results.is_empty()
                            && !self.state.loading
                            && self.state.error.is_none()
                        {
                            div()
                                .flex_1()
                                .child(
                                    div()
                                        .px(px(p::ROW_PADDING_X))
                                        .py(px(p::EMPTY_PADDING_Y))
                                        .text_center()
                                        .text_size(px(p::TITLE_FONT))
                                        .text_color(rgb(palette.foreground_muted))
                                        .child("No matches"),
                                )
                                .into_any_element()
                        } else {
                            div()
                                .flex_1()
                                .min_h_0()
                                .on_scroll(cx.listener(|this, _, _, _| this.reveal_pending = false))
                                .child(
                                    list(self.list.clone(), cx.processor(Self::render_item))
                                        .size_full(),
                                )
                                .into_any_element()
                        },
                    ),
            )
    }
}

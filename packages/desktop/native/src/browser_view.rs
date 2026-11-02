//! Pane browser surface: tabs, location bar, page lifecycle.
//!
//! One surface per browser pane tab. The surface owns the retained navigation
//! state for that tab plus the native child webviews retained for each of its
//! own page tabs; the pane layout owns which tab is shown where. GPUI draws the
//! chrome and measures the page area; the webview is positioned to that
//! rectangle in window coordinates, so the page follows splits, sidebar resizes
//! and window resizes without being captured into the GPUI scene.
//!
//! Overlays are the one thing the native view cannot take part in: a child
//! window paints above its parent's GPU scene, so an open modal or menu would
//! sit underneath the page. The shell hides the webview while an overlay is up
//! (`set_obscured`) rather than letting clicks fall through to the page.

use std::{collections::BTreeMap, time::Duration};

use gpui::{
    Context, Entity, FocusHandle, Focusable, MouseButton, Render, SharedString, Subscription, Task,
    Window, canvas, div, prelude::*, px, rgb,
};
use paseo_desktop::browser_model::{
    BrowserFocusOwner, BrowserModel, DEFAULT_BROWSER_URL, WebViewEvent, browser_focus_owner,
    page_may_show,
};

use crate::browser_webview::{NativeWebView, PlatformSupport, UnsupportedReason, WindowRect};
use crate::inline_input::{InlineInput, InlineInputEvent};
use crate::settings_view::SharedSettings;
use crate::theme::{font_size, radius};

/// WebKitGTK's main context is pumped from the GPUI frame task. 8 ms keeps page
/// scrolling and compositing responsive without a busy loop.
const PUMP_INTERVAL: Duration = Duration::from_millis(8);
/// Platform focus is read every eighth tick (~64 ms). A click that moves the
/// keyboard is not a per-frame question, and on Linux the read costs an X11
/// round trip.
const FOCUS_POLL_TICKS: u32 = 8;
/// Back/forward availability has no webview event on Linux; polling the history
/// list every eighth pump tick (~64 ms) is enough for the toolbar.
const HISTORY_POLL_TICKS: u32 = 8;

pub struct BrowserSurface {
    /// The pane tab that owns this surface. Element ids carry it, or two panes
    /// showing two pages would draw the same tab strip.
    tab_id: String,
    shared: SharedSettings,
    model: BrowserModel,
    /// Keyboard owner for the pane while the page is showing. The native child
    /// window takes real clicks, so GPUI focus has to sit somewhere that is not
    /// an agent composer or the address bar.
    focus: FocusHandle,
    url_input: Entity<InlineInput>,
    platform: PlatformSupport,
    events: async_channel::Sender<(u64, WebViewEvent)>,
    native: BTreeMap<u64, NativeWebView>,
    /// Set when creating the webview failed. Retrying every frame would only
    /// repeat the same failure, so the pane keeps showing the reason.
    native_error: Option<String>,
    /// The shell is showing this surface's page (it is the active workspace tab).
    presented: bool,
    /// A GPUI overlay covers the pane, so the native view must not paint.
    obscured: bool,
    visible: bool,
    /// The last platform read of whether the page itself owns the keyboard.
    /// Clicks inside the page never reach GPUI, so this is what tells the shell
    /// the pane and its session pointer moved.
    page_focused: bool,
    bounds_applied: bool,
    content_bounds: Option<WindowRect>,
    scale_factor: f32,
    ticks: u32,
    _url_events: Subscription,
    _event_task: Task<()>,
    pump_task: Option<Task<()>>,
}

/// What the owning pane has to redraw when it happens.
///
/// A title change reaches the surface, not the window, so the pane tab label is
/// only correct if the surface reports it up.
#[derive(Clone, Debug)]
pub enum BrowserSurfaceEvent {
    /// The active page's title, URL, or selection changed.
    LabelChanged,
    /// The native page took the keyboard from a real platform focus change.
    /// It is the only signal that a click inside the page moved the pane's
    /// keyboard: the page is a child window, and page-scripted messages cannot
    /// produce it.
    PageFocused,
    /// Real input inside the page the shell is already showing. Presence stays
    /// fresh while the user types, clicks or scrolls; it never moves focus.
    PageActivity,
}

impl gpui::EventEmitter<BrowserSurfaceEvent> for BrowserSurface {}

impl Focusable for BrowserSurface {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl BrowserSurface {
    pub fn new(tab_id: String, shared: SharedSettings, cx: &mut Context<Self>) -> Self {
        let model = BrowserModel::new(DEFAULT_BROWSER_URL);
        let url_input = cx.new(|cx| InlineInput::new("Enter URL".to_owned(), shared.clone(), cx));
        let _url_events = cx.subscribe(&url_input, |this, _, event: &InlineInputEvent, cx| {
            if matches!(event, InlineInputEvent::Submit) {
                this.submit_address(cx);
            }
        });

        let (events, receiver) = async_channel::unbounded();
        // wry handlers run on the GTK thread, which is this thread during a
        // layout pass; the queue keeps them from re-entering the entity.
        let _event_task = cx.spawn(async move |this, cx| {
            while let Ok((tab, event)) = receiver.recv().await {
                if this
                    .update(cx, |this, cx| this.on_webview_event(tab, event, cx))
                    .is_err()
                {
                    break;
                }
            }
        });

        BrowserSurface {
            tab_id,
            shared,
            model,
            focus: cx.focus_handle(),
            url_input,
            platform: PlatformSupport::Unsupported(UnsupportedReason::OtherPlatform),
            events,
            native: BTreeMap::new(),
            native_error: None,
            presented: false,
            obscured: false,
            visible: false,
            page_focused: false,
            bounds_applied: false,
            content_bounds: None,
            scale_factor: 1.0,
            ticks: 0,
            _url_events,
            _event_task,
            pump_task: None,
        }
    }

    /// What the owning pane's tab strip shows: the active page's label.
    pub fn title(&self) -> String {
        self.model.active().label()
    }

    /// The shell tells the surface whether it is the visible workspace tab.
    pub fn set_presented(&mut self, presented: bool, cx: &mut Context<Self>) {
        if self.presented == presented {
            return;
        }
        self.presented = presented;
        if presented {
            self.bounds_applied = false;
        }
        self.sync_visibility(cx);
        cx.notify();
    }

    /// The shell hides the page beneath modals and menus, which cannot be drawn
    /// above a native child window.
    pub fn set_obscured(&mut self, obscured: bool, cx: &mut Context<Self>) {
        if self.obscured == obscured {
            return;
        }
        self.obscured = obscured;
        if !obscured {
            self.bounds_applied = false;
        }
        self.sync_visibility(cx);
        cx.notify();
    }

    fn ensure_native(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.platform = crate::browser_webview::platform_support(window);
        if self.native.contains_key(&self.model.active().id)
            || self.native_error.is_some()
            || !self.presented
            || !matches!(
                self.platform,
                PlatformSupport::X11 | PlatformSupport::Native
            )
        {
            return;
        }
        let Some(bounds) = self
            .content_bounds
            .filter(|(_, _, width, height)| *width > 0. && *height > 0.)
        else {
            return;
        };
        let url = self.model.active().url.clone();
        match NativeWebView::create(
            window,
            self.model.active().id,
            &url,
            bounds,
            self.events.clone(),
        ) {
            Ok(mut native) => {
                if let Err(error) = native.set_bounds(bounds, self.scale_factor) {
                    self.native_error = Some(error.to_string());
                    self.model
                        .note_load_failure(format!("Browser view could not resize: {error}"));
                    cx.notify();
                    return;
                }
                self.native.insert(self.model.active().id, native);
                self.visible = false;
                self.page_focused = false;
                // Creation only prepares the widget at the retained bounds of the
                // page it replaced; the current frame's canvas measurement is what
                // applies bounds and reveals it.
                self.bounds_applied = false;
                self.start_pump(cx);
                self.sync_visibility(cx);
            }
            Err(error) => {
                self.native_error = Some(error.to_string());
                self.model
                    .note_load_failure(format!("Unable to embed the browser view: {error}"));
                cx.notify();
            }
        }
    }

    /// Starts the GTK pump for this surface. Dropping the task stops the pump.
    fn start_pump(&mut self, cx: &mut Context<Self>) {
        if self.pump_task.is_some() {
            return;
        }
        self.pump_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(PUMP_INTERVAL).await;
                if this.update(cx, |this, cx| this.tick(cx)).is_err() {
                    break;
                }
            }
        }));
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        if self.native.is_empty() {
            return;
        }
        // WebKitGTK shares this thread's GTK main context; advancing it is what
        // makes the page paint and deliver its navigation callbacks.
        crate::browser_webview::pump();
        self.ticks = self.ticks.wrapping_add(1);
        if self.ticks % FOCUS_POLL_TICKS == 0 {
            self.poll_page_focus(cx);
        }
        if self.ticks % HISTORY_POLL_TICKS != 0 {
            return;
        }
        let Some(native) = self.native.get(&self.model.active().id) else {
            return;
        };
        let can_go_back = native.can_go_back();
        let can_go_forward = native.can_go_forward();
        if self.model.set_history(can_go_back, can_go_forward) {
            cx.notify();
        }
    }

    /// Reads platform focus for the showing page and reports a transition into
    /// the page. Polling is what makes a click inside the page visible to the
    /// pane at all; the platform value itself is what the shell acts on.
    fn poll_page_focus(&mut self, cx: &mut Context<Self>) {
        // A page that is not on screen cannot take a click, and a hidden view
        // must not claim the pane either.
        let focused = self.presented && self.page_has_keyboard_focus();
        if focused == self.page_focused {
            return;
        }
        self.page_focused = focused;
        if focused {
            cx.emit(BrowserSurfaceEvent::PageFocused);
        }
    }

    fn page_has_keyboard_focus(&self) -> bool {
        self.native
            .get(&self.model.active().id)
            .and_then(NativeWebView::has_keyboard_focus)
            .unwrap_or(false)
    }

    /// Hands the keyboard to the page that just reported real platform focus.
    /// The location bar is GPUI chrome, so nothing in the child toolkit tells
    /// it the keyboard left; without this its stale GPUI focus would keep
    /// outranking the page.
    pub fn release_location_bar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.url_input.read(cx).focus_handle(cx).is_focused(window) {
            return;
        }
        self.focus.focus(window, cx);
        cx.notify();
    }

    /// Where keyboard focus sits inside this surface right now. The platform
    /// read is taken live rather than from the last poll, because the shell asks
    /// during the same click that moved it.
    pub fn focus_owner(&self, window: &Window, cx: &gpui::App) -> BrowserFocusOwner {
        browser_focus_owner(
            self.page_has_keyboard_focus(),
            self.url_input.read(cx).focus_handle(cx).is_focused(window),
            self.focus.is_focused(window),
        )
    }

    fn apply_content_bounds(
        &mut self,
        bounds: WindowRect,
        scale_factor: f32,
        cx: &mut Context<Self>,
    ) {
        if bounds.2 <= 0. || bounds.3 <= 0. {
            self.bounds_applied = false;
            self.content_bounds = None;
            self.sync_visibility(cx);
            return;
        }
        if self.content_bounds == Some(bounds)
            && self.scale_factor == scale_factor
            && self.bounds_applied
        {
            self.sync_visibility(cx);
            return;
        }
        self.scale_factor = scale_factor;
        self.bounds_applied = false;
        if let Some(native) = self.native.get_mut(&self.model.active().id) {
            match native.set_bounds(bounds, scale_factor) {
                Ok(()) => {
                    self.bounds_applied = true;
                    self.content_bounds = Some(bounds);
                }
                Err(error) => {
                    self.model
                        .note_load_failure(format!("Browser view could not resize: {error}"));
                    cx.notify();
                }
            }
        } else if bounds.2 > 0. && bounds.3 > 0. {
            self.content_bounds = Some(bounds);
            cx.notify();
        }
        self.sync_visibility(cx);
    }

    /// Whether the page actions can reach a native view on this window.
    fn embedded(&self) -> bool {
        self.native.contains_key(&self.model.active().id)
    }

    fn sync_visibility(&mut self, cx: &mut Context<Self>) {
        let active = self.model.active().id;
        let want = page_may_show(
            self.embedded(),
            self.presented,
            self.obscured,
            self.bounds_applied,
        );
        for (id, native) in self.native.iter_mut() {
            if *id != active {
                let _ = native.set_visible(false);
            }
        }
        if self.visible == want {
            return;
        }
        if let Some(native) = self.native.get_mut(&self.model.active().id)
            && native.set_visible(want).is_ok()
        {
            self.visible = want;
            cx.notify();
        }
    }

    fn on_webview_event(&mut self, tab: u64, event: WebViewEvent, cx: &mut Context<Self>) {
        // Presence freshness never changes page state, and it only counts while
        // this surface is on screen and the live native read says the page owns
        // the keyboard. A page can request the refresh; it cannot grant itself
        // focus or activity behind the user's back.
        if matches!(event, WebViewEvent::UserActivity) {
            if tab == self.model.active().id && self.presented && self.page_has_keyboard_focus() {
                cx.emit(BrowserSurfaceEvent::PageActivity);
            }
            return;
        }
        let previous_id = self.model.active().id;
        let previous_url = self.model.active().url.clone();
        let previous_label = self.model.active().label();
        if !self.model.apply_tab_event(tab, event) {
            return;
        }
        if self.model.active().id != previous_id {
            self.activate_retained_page(cx);
        } else if self.model.active().url != previous_url {
            self.sync_url_input(cx);
        }
        if self.model.active().label() != previous_label {
            cx.emit(BrowserSurfaceEvent::LabelChanged);
        }
        cx.notify();
    }

    /// New pages are created at their measured bounds on the next paint;
    /// selecting an existing tab only reveals its retained native page.
    fn activate_retained_page(&mut self, cx: &mut Context<Self>) {
        self.visible = false;
        self.page_focused = false;
        self.bounds_applied = false;
        self.native_error = None;
        self.sync_url_input(cx);
        self.sync_visibility(cx);
        cx.emit(BrowserSurfaceEvent::LabelChanged);
        cx.notify();
    }

    fn sync_url_input(&mut self, cx: &mut Context<Self>) {
        let url = self.model.active().url.clone();
        self.url_input
            .update(cx, |input, cx| input.set_text(&url, cx));
    }

    fn submit_address(&mut self, cx: &mut Context<Self>) {
        let address = self.url_input.read(cx).text().trim().to_owned();
        if self.model.navigate(&address).is_ok() {
            self.sync_url_input(cx);
            self.navigate_active(cx);
            cx.emit(BrowserSurfaceEvent::LabelChanged);
        }
        cx.notify();
    }

    /// Loads a typed address in the active tab, leaving other pages retained.
    fn navigate_active(&mut self, cx: &mut Context<Self>) {
        if !self.embedded() {
            cx.notify();
            return;
        }
        let url = self.model.active().url.clone();
        let result = match self.native.get(&self.model.active().id) {
            Some(native) => native.navigate(&url).map_err(|error| error.to_string()),
            None => Err("the browser view is not available".to_owned()),
        };
        if let Err(error) = result {
            self.model
                .note_load_failure(format!("Failed to load page: {error}"));
        }
        cx.notify();
    }

    fn go_back(&mut self, cx: &mut Context<Self>) {
        if !self.embedded() {
            return;
        }
        let result = match self.native.get(&self.model.active().id) {
            Some(native) => native.go_back().map_err(|error| error.to_string()),
            None => Err("the browser view is not available".to_owned()),
        };
        match result {
            Ok(()) => self.model.note_navigation_started(),
            Err(error) => self
                .model
                .note_load_failure(format!("Failed to go back: {error}")),
        }
        cx.notify();
    }

    fn go_forward(&mut self, cx: &mut Context<Self>) {
        if !self.embedded() {
            return;
        }
        let result = match self.native.get(&self.model.active().id) {
            Some(native) => native.go_forward().map_err(|error| error.to_string()),
            None => Err("the browser view is not available".to_owned()),
        };
        match result {
            Ok(()) => self.model.note_navigation_started(),
            Err(error) => self
                .model
                .note_load_failure(format!("Failed to go forward: {error}")),
        }
        cx.notify();
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        if !self.embedded() {
            return;
        }
        let result = match self.native.get(&self.model.active().id) {
            Some(native) => native.reload().map_err(|error| error.to_string()),
            None => Err("the browser view is not available".to_owned()),
        };
        match result {
            Ok(()) => self.model.reload(),
            Err(error) => self
                .model
                .note_load_failure(format!("Failed to reload: {error}")),
        }
        cx.notify();
    }

    fn select_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        if !self.model.set_active(index) {
            return;
        }
        self.activate_retained_page(cx);
    }

    fn open_tab(&mut self, cx: &mut Context<Self>) {
        if self.model.open_tab(DEFAULT_BROWSER_URL).is_ok() {
            self.activate_retained_page(cx);
        }
    }

    fn close_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.model.tabs().len() <= 1 || index >= self.model.tabs().len() {
            return;
        }
        let closed_id = self.model.tabs()[index].id;
        let moved = self.model.close_tab(index);
        self.native.remove(&closed_id);
        if moved {
            self.activate_retained_page(cx);
        }
        cx.notify();
    }

    /// What the page area shows instead of a webview, when there is no page.
    fn platform_message(&self) -> Option<(&'static str, String)> {
        match self.platform {
            PlatformSupport::X11 | PlatformSupport::Native => self.native_error.as_ref().map(|error| {
                (
                    "The browser view could not start",
                    format!(
                        "{error}\n\nCheck the platform webview runtime (WebKitGTK on Linux, WebKit on macOS, WebView2 on Windows)."
                    ),
                )
            }),
            PlatformSupport::Unsupported(UnsupportedReason::Wayland) => Some((
                "The embedded browser needs X11",
                "Wayland is not supported for embedded pages yet. Start the desktop app from an X11 session, or unset WAYLAND_DISPLAY so it uses X11.".to_owned(),
            )),
            PlatformSupport::Unsupported(UnsupportedReason::OtherPlatform) => Some((
                "The embedded browser is not available here",
                "This platform does not expose a supported native child-window handle."
                    .to_owned(),
            )),
        }
    }
}

impl Render for BrowserSurface {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_native(window, cx);
        let palette = crate::settings_view::active_palette(&self.shared, window);
        let tab_id = self.tab_id.clone();
        let tabs = self.model.tabs().to_vec();
        let active_index = self.model.active_index();
        let can_go_back = self.model.active().can_go_back;
        let can_go_forward = self.model.active().can_go_forward;
        let loading = self.model.active().loading;
        let embedded = self.embedded();
        let error = self.model.active().error.clone();
        let message = self.platform_message();
        let measure = cx.entity();

        let tab_strip = div()
            .id("browser-tab-strip")
            .h(px(34.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap_1()
            .px_2()
            .border_b_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface1))
            .children(tabs.iter().enumerate().map(|(index, tab)| {
                let selected = index == active_index;
                let element_id = format!("browser-tab-{tab_id}-{}", tab.id);
                let close_id = format!("browser-tab-close-{tab_id}-{}", tab.id);
                let mut element = div()
                    .id(SharedString::from(element_id))
                    .h(px(26.))
                    .max_w(px(200.))
                    .px_2()
                    .rounded(px(radius::MD))
                    .flex()
                    .items_center()
                    .gap_1()
                    .cursor_pointer()
                    .text_size(px(font_size::SM))
                    .when(selected, |tab| tab.bg(rgb(palette.surface2)))
                    .hover(|tab| tab.bg(rgb(palette.surface2)))
                    .text_color(rgb(if selected {
                        palette.foreground
                    } else {
                        palette.foreground_muted
                    }))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.select_tab(index, cx);
                        }),
                    )
                    .child(div().min_w_0().truncate().child(tab.label()));
                if tabs.len() > 1 {
                    element = element.child(
                        div()
                            .id(SharedString::from(close_id))
                            .w(px(16.))
                            .h(px(16.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(radius::SM))
                            .hover(|button| button.bg(rgb(palette.surface3)))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.close_tab(index, cx);
                                }),
                            )
                            .child(crate::chrome::icon("x", 10.)),
                    );
                }
                element
            }))
            .child(
                div()
                    .id("browser-new-tab")
                    .w(px(24.))
                    .h(px(24.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(radius::MD))
                    .cursor_pointer()
                    .hover(|button| button.bg(rgb(palette.surface2)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| this.open_tab(cx)),
                    )
                    .child(crate::chrome::icon("plus", 12.)),
            );

        let toolbar = div()
            .id("browser-toolbar")
            .h(px(44.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap_1()
            .px_2()
            .border_b_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface0))
            .child(
                nav_button("browser-back", "←", can_go_back, palette).on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| this.go_back(cx)),
                ),
            )
            .child(
                nav_button("browser-forward", "→", can_go_forward, palette).on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| this.go_forward(cx)),
                ),
            )
            .child(
                nav_button("browser-reload", "⟳", embedded, palette).on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| this.reload(cx)),
                ),
            )
            .child(div().flex_1().min_w_0().child(self.url_input.clone()))
            .when(loading, |toolbar| {
                toolbar.child(
                    div()
                        .flex_shrink_0()
                        .pr_1()
                        .text_size(px(font_size::SM))
                        .text_color(rgb(palette.foreground_muted))
                        .child("Loading..."),
                )
            });

        let body = match message {
            Some((title, detail)) => div()
                .flex_1()
                .min_h_0()
                .min_w_0()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .p_6()
                .child(
                    div()
                        .text_size(px(font_size::BASE))
                        .text_color(rgb(palette.foreground))
                        .child(title),
                )
                .child(
                    div()
                        .max_w(px(520.))
                        .text_size(px(font_size::SM))
                        .text_color(rgb(palette.foreground_muted))
                        .child(detail),
                )
                .into_any_element(),
            None => div()
                .flex_1()
                .min_h_0()
                .min_w_0()
                .relative()
                .bg(rgb(palette.surface0))
                .child(
                    canvas(
                        move |bounds, window, cx| {
                            let rect = (
                                bounds.origin.x.to_f64(),
                                bounds.origin.y.to_f64(),
                                bounds.size.width.to_f64(),
                                bounds.size.height.to_f64(),
                            );
                            let scale_factor = window.scale_factor();
                            let _ = measure.update(cx, |this, cx| {
                                this.apply_content_bounds(rect, scale_factor, cx)
                            });
                        },
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .inset_0(),
                )
                .into_any_element(),
        };

        div()
            .id(SharedString::from(format!("browser-surface-{tab_id}")))
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .min_h_0()
            .min_w_0()
            .bg(rgb(palette.surface0))
            .text_color(rgb(palette.foreground))
            .child(tab_strip)
            .child(toolbar)
            .when_some(error, |surface, error| {
                surface.child(
                    div()
                        .flex_shrink_0()
                        .px_2()
                        .py_1()
                        .border_b_1()
                        .border_color(rgb(palette.border))
                        .text_size(px(font_size::SM))
                        .text_color(rgb(palette.status_danger))
                        .child(error),
                )
            })
            .child(body)
    }
}

fn nav_button(
    id: &'static str,
    glyph: &'static str,
    enabled: bool,
    palette: paseo_desktop::appearance::Palette,
) -> gpui::Stateful<gpui::Div> {
    let button = div()
        .id(id)
        .w(px(28.))
        .h(px(28.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(radius::MD))
        .text_size(px(font_size::LG))
        .child(glyph);
    if enabled {
        button
            .cursor_pointer()
            .text_color(rgb(palette.foreground_muted))
            .hover(|button| button.bg(rgb(palette.surface2)))
    } else {
        button.text_color(rgb(palette.foreground_extra_muted))
    }
}

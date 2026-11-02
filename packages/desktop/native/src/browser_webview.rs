//! Native child webview behind the workspace browser chrome.
//!
//! GPUI draws the tabs, location bar and error row; the page itself is a real
//! platform webview. On Linux the page is a wry/WebKitGTK child of the GPUI X11
//! window, positioned in window coordinates from the GPUI layout, so it scrolls
//! and resizes with the pane instead of being screenshotted into the GPUI scene.
//!
//! Two facts shape this module:
//!
//! * wry only creates X11 child webviews on Linux. A Wayland GPUI window has no
//!   Xlib handle, so the browser reports [`PlatformSupport::Unsupported`] and
//!   the pane explains why instead of drawing an empty page.
//! * WebKitGTK needs GTK initialised on this thread and its main context pumped
//!   from the same event loop. [`pump`] is called from the GPUI frame task while
//!   the browser surface exists.
//!
//! Downloads use wry's default allow-all handler, so a page download lands in
//! the session's XDG download directory with WebKit's deduplicated file name;
//! this client does not surface download progress yet.

use gpui::Window;

/// Why the embedded webview cannot run on this window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnsupportedReason {
    /// GPUI is running on Wayland; wry's child webview is X11-only.
    Wayland,
    /// macOS and Windows embedding is not wired yet.
    OtherPlatform,
}

/// Which child-webview backend this window supports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlatformSupport {
    X11,
    /// macOS and Windows build with the same wry builder, but only the Linux
    /// path has been exercised; nothing constructs this variant on Linux.
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    Native,
    Unsupported(UnsupportedReason),
}

pub fn platform_support(window: &Window) -> PlatformSupport {
    #[cfg(target_os = "linux")]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        // `Window` also has an inherent `window_handle` returning a typed entity
        // handle; the raw platform handle comes from the trait.
        match HasWindowHandle::window_handle(window).map(|handle| handle.as_raw()) {
            Ok(RawWindowHandle::Xcb(_) | RawWindowHandle::Xlib(_)) => PlatformSupport::X11,
            Ok(RawWindowHandle::Wayland(_)) => {
                PlatformSupport::Unsupported(UnsupportedReason::Wayland)
            }
            _ => PlatformSupport::Unsupported(UnsupportedReason::OtherPlatform),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        match HasWindowHandle::window_handle(window).map(|handle| handle.as_raw()) {
            Ok(RawWindowHandle::AppKit(_) | RawWindowHandle::Win32(_)) => PlatformSupport::Native,
            _ => PlatformSupport::Unsupported(UnsupportedReason::OtherPlatform),
        }
    }
}

/// Window-space rectangle `(x, y, width, height)` in GPUI logical pixels.
pub type WindowRect = (f64, f64, f64, f64);

pub use native::{NativeWebView, pump};

mod native {
    use std::ffi::c_ulong;
    use std::sync::OnceLock;

    use anyhow::{Result, anyhow};
    use gpui::Window;
    use paseo_desktop::browser_model::WebViewEvent;
    use raw_window_handle::{
        HandleError, HasWindowHandle, RawWindowHandle, WindowHandle, XlibWindowHandle,
    };
    use wry::{NewWindowResponse, PageLoadEvent, WebView, WebViewBuilder};

    use super::WindowRect;

    /// GTK's first initialisation result. `gtk::init` may be called again, but
    /// the pump must not touch a main context that was never initialised.
    #[cfg(target_os = "linux")]
    static GTK: OnceLock<std::result::Result<(), String>> = OnceLock::new();

    #[cfg(target_os = "linux")]
    fn gtk_ready() -> Result<()> {
        // wry downcasts GDK's display to X11 without a fallback. Refuse a
        // GDK_BACKEND that forces Wayland before GTK initialises, instead of
        // letting the webview creation panic inside wry.
        if let Ok(backends) = std::env::var("GDK_BACKEND")
            && backends
                .split(',')
                .any(|backend| backend.trim().eq_ignore_ascii_case("wayland"))
        {
            return Err(anyhow!(
                "GDK_BACKEND selects Wayland; the embedded browser needs the X11 backend"
            ));
        }
        let result = GTK.get_or_init(|| gtk::init().map_err(|error| error.to_string()));
        match result {
            Ok(()) => Ok(()),
            Err(error) => Err(anyhow!("GTK initialisation failed: {error}")),
        }
    }

    /// Advances WebKitGTK's main context. Safe to call before any browser view
    /// exists and a no-op until GTK has been initialised on this thread.
    pub fn pump() {
        #[cfg(target_os = "linux")]
        {
            if !GTK.get().is_some_and(|result| result.is_ok()) {
                return;
            }
            while gtk::events_pending() {
                gtk::main_iteration_do(false);
            }
        }
    }

    /// The GPUI window's X11 id, so the Linux focus read can tell the page's
    /// child window from the window that draws the location bar.
    #[cfg(target_os = "linux")]
    fn parent_x11_window(window: &Window) -> Option<u32> {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        match HasWindowHandle::window_handle(window).ok()?.as_raw() {
            RawWindowHandle::Xcb(handle) => Some(handle.window.get()),
            RawWindowHandle::Xlib(handle) => u32::try_from(handle.window).ok(),
            _ => None,
        }
    }

    /// Other platforms read focus through their own native source.
    #[cfg(not(target_os = "linux"))]
    fn parent_x11_window(_window: &Window) -> Option<u32> {
        None
    }

    /// A wry child webview plus the state this client applies to it.
    pub struct NativeWebView {
        // Rust drops fields in declaration order: unregister native callbacks
        // before Wry closes the controller they were registered against.
        page_focus: page_focus::PageFocus,
        webview: WebView,
        bounds: Option<WindowRect>,
        scale: f32,
        visible: bool,
    }

    impl NativeWebView {
        pub fn create(
            window: &Window,
            tab_id: u64,
            url: &str,
            bounds: WindowRect,
            events: async_channel::Sender<(u64, WebViewEvent)>,
        ) -> Result<Self> {
            #[cfg(target_os = "linux")]
            gtk_ready()?;
            #[cfg(target_os = "linux")]
            let target = X11ChildTarget::from_window(window)?;
            #[cfg(not(target_os = "linux"))]
            let target = window;

            let navigation_events = events.clone();
            let title_events = events.clone();
            let load_events = events.clone();
            let native_events = events.clone();
            let new_window_events = events;

            let builder = WebViewBuilder::new()
                .with_url(url)
                .with_bounds(logical_rect(bounds))
                // The first GPUI paint positions it; starting hidden avoids a
                // frame of page at the window origin.
                .with_visible(false)
                .with_autoplay(true)
                .with_navigation_handler(move |url| {
                    let allowed = paseo_desktop::browser_model::is_allowed_navigation(&url);
                    if !allowed {
                        let _ = navigation_events
                            .try_send((tab_id, WebViewEvent::NavigationBlocked { url }));
                    }
                    allowed
                })
                .with_document_title_changed_handler(move |title| {
                    let _ = title_events.try_send((tab_id, WebViewEvent::TitleChanged { title }));
                })
                .with_on_page_load_handler(move |event, url| {
                    let event = match event {
                        PageLoadEvent::Started => WebViewEvent::LoadStarted { url },
                        PageLoadEvent::Finished => WebViewEvent::LoadFinished { url },
                    };
                    let _ = load_events.try_send((tab_id, event));
                })
                .with_new_window_req_handler(move |url, _features| {
                    // Links that ask for a new window become tabs in the same
                    // browser; denying the popup keeps one window per surface.
                    let _ =
                        new_window_events.try_send((tab_id, WebViewEvent::NewWindowTab { url }));
                    NewWindowResponse::Deny
                });
            #[cfg(target_os = "linux")]
            let webview = builder.build_as_child(&target)?;
            #[cfg(not(target_os = "linux"))]
            let webview = builder.build_as_child(target)?;

            let page_focus = page_focus::PageFocus::attach(
                &webview,
                tab_id,
                native_events,
                parent_x11_window(window),
            )?;

            Ok(NativeWebView {
                webview,
                bounds: None,
                scale: 0.,
                visible: false,
                page_focus,
            })
        }

        pub fn set_bounds(&mut self, bounds: WindowRect, scale_factor: f32) -> Result<()> {
            if self.bounds == Some(bounds) && self.scale == scale_factor {
                return Ok(());
            }
            // wry converts the rect from physical to logical pixels with the
            // webview's own scale factor, so the GPUI logical bounds are scaled
            // up first.
            let scale = f64::from(scale_factor.max(1.0));
            let (x, y, width, height) = bounds;
            self.webview.set_bounds(wry::Rect {
                position: wry::dpi::PhysicalPosition::new(x * scale, y * scale).into(),
                size: wry::dpi::PhysicalSize::new(
                    (width * scale).max(1.0),
                    (height * scale).max(1.0),
                )
                .into(),
            })?;
            self.bounds = Some(bounds);
            self.scale = scale_factor;
            Ok(())
        }

        pub fn set_visible(&mut self, visible: bool) -> Result<()> {
            if self.visible == visible {
                return Ok(());
            }
            self.webview.set_visible(visible)?;
            self.visible = visible;
            Ok(())
        }

        pub fn navigate(&self, url: &str) -> Result<()> {
            self.webview.load_url(url)?;
            Ok(())
        }

        pub fn reload(&self) -> Result<()> {
            self.webview.reload()?;
            Ok(())
        }

        pub fn go_back(&self) -> Result<()> {
            self.webview.go_back()?;
            Ok(())
        }

        pub fn go_forward(&self) -> Result<()> {
            self.webview.go_forward()?;
            Ok(())
        }

        pub fn can_go_back(&self) -> bool {
            self.webview.can_go_back().unwrap_or(false)
        }

        pub fn can_go_forward(&self) -> bool {
            self.webview.can_go_forward().unwrap_or(false)
        }

        /// Whether the platform webview owns the keyboard, or `None` where this
        /// client cannot read platform focus yet.
        ///
        /// The page is a native child window: a click inside it never reaches
        /// GPUI, and nothing a page can script may claim the keyboard. Only the
        /// platform's own focus state is trustworthy here, so every platform
        /// reads its own authority (WebView2 controller GotFocus/LostFocus,
        /// NSWindow key window + first responder, GTK widget focus).
        pub fn has_keyboard_focus(&self) -> Option<bool> {
            self.page_focus.read(&self.webview)
        }
    }

    /// Platform authority for "the page owns the keyboard" plus the native input
    /// hooks that keep presence fresh while the user types, clicks or scrolls in
    /// the page. Native signals come first: a page must never be able to claim
    /// the keyboard.
    mod page_focus {
        use anyhow::Result;
        use paseo_desktop::browser_model::WebViewEvent;
        use wry::WebView;

        type Events = async_channel::Sender<(u64, WebViewEvent)>;

        #[cfg(target_os = "linux")]
        pub struct PageFocus {
            /// X11 window of the page's child container inside the GPUI window,
            /// and of the GPUI window itself. GTK keeps reporting widget focus
            /// after the keyboard moves into GPUI chrome (the location bar is
            /// not a GTK widget, so GTK never sees the focus leave), so only the
            /// X11 input focus decides which of the two owns the keyboard.
            child: Option<u32>,
            parent: Option<u32>,
            /// Last X11 verdict and when it was taken: renders and the presence
            /// poll must not each become an X round trip.
            cached: std::cell::Cell<Option<(std::time::Instant, bool)>>,
        }

        /// How long an X11 focus verdict is reused. The presence poll runs every
        /// ~64 ms; this keeps the query off the frame path.
        #[cfg(target_os = "linux")]
        const X11_FOCUS_CACHE: std::time::Duration = std::time::Duration::from_millis(120);

        #[cfg(target_os = "linux")]
        impl PageFocus {
            pub fn attach(
                webview: &WebView,
                tab: u64,
                events: Events,
                parent: Option<u32>,
            ) -> Result<Self> {
                use gtk::prelude::*;
                use wry::WebViewExtUnix;
                let view = WebViewExtUnix::webview(webview);
                let key_events = events.clone();
                view.connect_key_press_event(move |_, _| {
                    let _ = key_events.try_send((tab, WebViewEvent::UserActivity));
                    gtk::glib::Propagation::Proceed
                });
                let button_events = events.clone();
                view.connect_button_press_event(move |_, _| {
                    let _ = button_events.try_send((tab, WebViewEvent::UserActivity));
                    gtk::glib::Propagation::Proceed
                });
                let scroll_events = events;
                view.connect_scroll_event(move |_, _| {
                    let _ = scroll_events.try_send((tab, WebViewEvent::UserActivity));
                    gtk::glib::Propagation::Proceed
                });
                Ok(PageFocus {
                    child: child_x11_window(&view),
                    parent,
                    cached: std::cell::Cell::new(None),
                })
            }

            pub fn read(&self, webview: &WebView) -> Option<bool> {
                use gtk::prelude::*;
                use wry::WebViewExtUnix;
                let view = WebViewExtUnix::webview(webview);
                let gtk_focused = view.has_focus()
                    || view
                        .toplevel()
                        .and_then(|top| top.downcast::<gtk::Window>().ok())
                        .is_some_and(|window| {
                            // WebKitGTK hands focus to an inner widget for some
                            // pages, so the focus owner is checked against the
                            // view rather than only on it.
                            let focused = window.focused_widget();
                            window.is_active()
                                && focused.map_or(true, |focused| focused.is_ancestor(&view))
                        });
                if !gtk_focused {
                    return Some(false);
                }
                Some(self.owns_x11_input_focus(child_x11_window(&view)))
            }

            /// GTK's answer, but only while the X11 input focus is actually in
            /// this page's child window. When the parent GPUI window owns the
            /// focus, the location bar is the one taking the keystrokes.
            fn owns_x11_input_focus(&self, realized_child: Option<u32>) -> bool {
                let (Some(child), Some(parent)) = (realized_child.or(self.child), self.parent)
                else {
                    // Never turn an unavailable authority read into page focus.
                    return false;
                };
                if let Some((taken, verdict)) = self.cached.get()
                    && taken.elapsed() < X11_FOCUS_CACHE
                {
                    return verdict;
                }
                let verdict = x11_input_focus_within(child, parent).unwrap_or(false);
                self.cached.set(Some((std::time::Instant::now(), verdict)));
                verdict
            }
        }

        /// The X11 window wry created for the page: its GTK toplevel wraps the
        /// child container window, so the GdkWindow is that X window.
        #[cfg(target_os = "linux")]
        fn child_x11_window<W: gtk::prelude::IsA<gtk::Widget>>(view: &W) -> Option<u32> {
            use gtk::prelude::*;
            let window = view.toplevel()?.downcast::<gtk::Window>().ok()?;
            let gdk_window = window.window()?;
            let x11 = gdk_window.downcast::<gdkx11::X11Window>().ok()?;
            u32::try_from(x11.xid()).ok()
        }

        /// Whether the X11 input focus is `child` or a window inside it, rather
        /// than `parent` (the GPUI window drawing the location bar).
        #[cfg(target_os = "linux")]
        fn x11_input_focus_within(child: u32, parent: u32) -> Option<bool> {
            use std::sync::{Mutex, OnceLock};
            use x11rb::protocol::xproto;

            static CONNECTION: OnceLock<
                Mutex<Option<(x11rb::rust_connection::RustConnection, usize)>>,
            > = OnceLock::new();

            let cell = CONNECTION.get_or_init(|| Mutex::new(x11rb::connect(None).ok()));
            let mut guard = cell.lock().ok()?;
            let (connection, _screen) = guard.as_mut()?;
            let focus = match xproto::get_input_focus(connection) {
                Ok(cookie) => cookie.reply().ok()?.focus,
                Err(_) => return None,
            };
            // `0` is PointerRoot/None: no client window owns the keyboard.
            if focus == 0 || focus == parent || focus == child {
                return Some(focus == child);
            }
            // A deeper window (an embedded input, an IM window): walk up to the
            // window that separates the page from the GPUI parent.
            let mut current = focus;
            for _ in 0..8 {
                let tree = match xproto::query_tree(connection, current) {
                    Ok(cookie) => cookie.reply().ok()?,
                    Err(_) => return None,
                };
                if tree.parent == child {
                    return Some(true);
                }
                if tree.parent == parent || tree.parent == 0 {
                    return Some(false);
                }
                current = tree.parent;
            }
            Some(false)
        }

        /// WebView2 reports focus changes through the controller, which is the
        /// only authority that is not a guess: `GetFocus()` sees a separate
        /// input queue and can name the wrong window.
        #[cfg(target_os = "windows")]
        pub struct PageFocus {
            focused: std::rc::Rc<std::cell::Cell<bool>>,
            controller: webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Controller,
            focus_tokens: [i64; 2],
            accelerator_token: Option<i64>,
        }

        #[cfg(target_os = "windows")]
        impl PageFocus {
            pub fn attach(
                webview: &WebView,
                tab: u64,
                events: Events,
                _parent: Option<u32>,
            ) -> Result<Self> {
                use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Controller;
                use webview2_com::{AcceleratorKeyPressedEventHandler, FocusChangedEventHandler};
                use wry::WebViewExtWindows;

                let controller: ICoreWebView2Controller = webview.controller();
                let focused = std::rc::Rc::new(std::cell::Cell::new(false));

                let got_cell = focused.clone();
                let got_events = events.clone();
                let got = FocusChangedEventHandler::create(Box::new(move |_, _| {
                    got_cell.set(true);
                    // A focus change is real user input, not just a mode change.
                    let _ = got_events.try_send((tab, WebViewEvent::UserActivity));
                    Ok(())
                }));
                let lost_cell = focused.clone();
                let lost = FocusChangedEventHandler::create(Box::new(move |_, _| {
                    lost_cell.set(false);
                    Ok(())
                }));

                let mut got_token = 0i64;
                let mut lost_token = 0i64;
                unsafe { controller.add_GotFocus(&got, &mut got_token)? };
                if let Err(error) = unsafe { controller.add_LostFocus(&lost, &mut lost_token) } {
                    // Roll the first registration back: a page that reports focus
                    // but never its loss would keep claiming the pane.
                    let _ = unsafe { controller.remove_GotFocus(got_token) };
                    return Err(anyhow::anyhow!(
                        "WebView2 focus events could not start: {error}"
                    ));
                }

                let key_events = events;
                let accelerator =
                    AcceleratorKeyPressedEventHandler::create(Box::new(move |_, _| {
                        let _ = key_events.try_send((tab, WebViewEvent::UserActivity));
                        Ok(())
                    }));
                let mut accelerator_token = 0i64;
                let accelerator_token = match unsafe {
                    controller.add_AcceleratorKeyPressed(&accelerator, &mut accelerator_token)
                } {
                    Ok(()) => Some(accelerator_token),
                    // Keyboard freshness is a bonus; focus authority stands.
                    Err(_) => None,
                };

                Ok(PageFocus {
                    focused,
                    controller,
                    focus_tokens: [got_token, lost_token],
                    accelerator_token,
                })
            }

            pub fn read(&self, _webview: &WebView) -> Option<bool> {
                Some(self.focused.get())
            }
        }

        #[cfg(target_os = "windows")]
        impl Drop for PageFocus {
            fn drop(&mut self) {
                // Handlers are removed before the webview closes, or the
                // controller would call into a view this client released.
                if let Some(token) = self.accelerator_token {
                    let _ = unsafe { self.controller.remove_AcceleratorKeyPressed(token) };
                }
                let _ = unsafe { self.controller.remove_GotFocus(self.focus_tokens[0]) };
                let _ = unsafe { self.controller.remove_LostFocus(self.focus_tokens[1]) };
            }
        }

        /// macOS has no controller event for the child view, so the key window's
        /// first responder chain is the authority.
        #[cfg(target_os = "macos")]
        pub struct PageFocus;

        #[cfg(target_os = "macos")]
        impl PageFocus {
            pub fn attach(
                _webview: &WebView,
                _tab: u64,
                _events: Events,
                _parent: Option<u32>,
            ) -> Result<Self> {
                Ok(PageFocus)
            }

            pub fn read(&self, webview: &WebView) -> Option<bool> {
                use objc2_app_kit::NSView;
                use wry::WebViewExtMacOS;
                let view = WebViewExtMacOS::webview(webview);
                let Some(window) = view.window() else {
                    return Some(false);
                };
                if !window.isKeyWindow() {
                    return Some(false);
                }
                Some(
                    window
                        .firstResponder()
                        .and_then(|responder| {
                            responder
                                .downcast_ref::<NSView>()
                                .map(|focused| focused.isDescendantOf(&view))
                        })
                        .unwrap_or(false),
                )
            }
        }

        #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
        pub struct PageFocus;

        #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
        impl PageFocus {
            pub fn attach(
                _webview: &WebView,
                _tab: u64,
                _events: Events,
                _parent: Option<u32>,
            ) -> Result<Self> {
                Ok(PageFocus)
            }

            pub fn read(&self, _webview: &WebView) -> Option<bool> {
                None
            }
        }
    }

    fn logical_rect(bounds: WindowRect) -> wry::Rect {
        let (x, y, width, height) = bounds;
        wry::Rect {
            position: wry::dpi::LogicalPosition::new(x, y).into(),
            size: wry::dpi::LogicalSize::new(width.max(1.0), height.max(1.0)).into(),
        }
    }

    /// GPUI hands out an XCB window handle; wry's Linux child path only accepts
    /// an Xlib handle. Both carry the same XID, so the same window is described
    /// to wry in the form it understands.
    #[cfg(target_os = "linux")]
    struct X11ChildTarget {
        window: c_ulong,
        visual_id: c_ulong,
    }

    #[cfg(target_os = "linux")]
    impl X11ChildTarget {
        fn from_window(window: &Window) -> Result<Self> {
            let handle = HasWindowHandle::window_handle(window)
                .map_err(|error| anyhow!("GPUI window has no X11 handle: {error:?}"))?;
            match handle.as_raw() {
                RawWindowHandle::Xcb(handle) => Ok(X11ChildTarget {
                    window: c_ulong::from(handle.window.get()),
                    visual_id: handle
                        .visual_id
                        .map_or(0, |visual| c_ulong::from(visual.get())),
                }),
                RawWindowHandle::Xlib(handle) => Ok(X11ChildTarget {
                    window: handle.window,
                    visual_id: handle.visual_id,
                }),
                other => Err(anyhow!(
                    "GPUI window is not X11, cannot embed a webview: {other:?}"
                )),
            }
        }
    }

    #[cfg(target_os = "linux")]
    impl HasWindowHandle for X11ChildTarget {
        fn window_handle(&self) -> std::result::Result<WindowHandle<'_>, HandleError> {
            let mut handle = XlibWindowHandle::new(self.window);
            handle.visual_id = self.visual_id;
            Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::Xlib(handle)) })
        }
    }
}

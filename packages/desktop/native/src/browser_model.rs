//! Workspace browser navigation model.
//!
//! Two sources change the page: the location bar the user edits, and the
//! webview reporting loads, redirects, titles and history after the fact. This
//! model is the single truth the chrome draws from, so the ordering rules that
//! are hard to observe in a running window (a blocked navigation must not
//! overwrite the committed URL, a failed load must keep the last good URL, a
//! blank title must not erase the previous one) are testable without a window.
//!
//! URL handling mirrors `packages/app/src/desktop/browser/store/index.ts`:
//! `normalizeBrowserUrl` for what the location bar shows and
//! `getUnsafeNavigationMessage` for what may actually load.

/// Where a fresh tab points before the user types anything. Matches the app.
pub const DEFAULT_BROWSER_URL: &str = "https://example.com";

/// Who owns the keyboard inside a showing browser pane.
///
/// The page is a native child window, so GPUI never sees its clicks; only the
/// platform focus read knows the page is focused. The location bar and the
/// surface chrome are GPUI handles. The shell needs this split because a pane
/// click runs after the clicked element took focus, and re-focusing the surface
/// then would take the keyboard back from it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrowserFocusOwner {
    /// The native page owns the keyboard; GPUI focus stays where it is.
    Page,
    /// The location bar owns the keyboard.
    AddressBar,
    /// The pane chrome owns the keyboard.
    Surface,
    None,
}

impl BrowserFocusOwner {
    /// Whether the pane's keyboard belongs to this browser at all.
    pub fn is_focused(self) -> bool {
        !matches!(self, BrowserFocusOwner::None)
    }

    /// Whether the shell may move GPUI focus onto the surface. It may not while
    /// the page or the location bar already holds the keyboard: the click that
    /// got there is still bubbling up to the pane.
    pub fn surface_may_take_focus(self) -> bool {
        matches!(self, BrowserFocusOwner::Surface | BrowserFocusOwner::None)
    }
}

/// Resolves the focus owner from the three places that can hold it.
///
/// The location bar outranks the page read: a GTK/WebView2 focus state can
/// outlive the keyboard moving into GPUI chrome, because the location bar is not
/// a widget of the child toolkit, so nothing tells it the keyboard left. A GPUI
/// handle is only true after a real click inside this window, so it wins;
/// `BrowserSurface` clears it when the page reports a real platform focus.
pub fn browser_focus_owner(
    page_focused: bool,
    address_bar_focused: bool,
    surface_focused: bool,
) -> BrowserFocusOwner {
    if address_bar_focused {
        BrowserFocusOwner::AddressBar
    } else if page_focused {
        BrowserFocusOwner::Page
    } else if surface_focused {
        BrowserFocusOwner::Surface
    } else {
        BrowserFocusOwner::None
    }
}

/// Whether the native page may be shown. `bounds_applied` is the current
/// frame's applied measurement: a page that was just created, or re-selected
/// from a hidden tab, carries the bounds of the page it replaced and stays
/// hidden until this frame measures the area it now belongs in.
pub fn page_may_show(
    embedded: bool,
    presented: bool,
    obscured: bool,
    bounds_applied: bool,
) -> bool {
    embedded && presented && !obscured && bounds_applied
}
/// The only page allowed outside http(s); the webview starts here.
pub const BLANK_URL: &str = "about:blank";

/// Why the location bar refuses to load what the user typed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NavigationError {
    /// The URL has no host, e.g. `https://`.
    InvalidUrl,
    /// A protocol the embedded browser will not load, e.g. `file:` or `javascript:`.
    UnsupportedProtocol(String),
}

impl NavigationError {
    /// The error row text, matching the app's English resource strings.
    pub fn message(&self) -> String {
        match self {
            NavigationError::InvalidUrl => "Invalid browser URL".to_owned(),
            NavigationError::UnsupportedProtocol(protocol) => {
                format!("Blocked unsupported browser URL: {protocol}")
            }
        }
    }
}

/// One browser tab. `url` is the committed (or optimistically requested) page;
/// the location bar draft lives in the input entity, not here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrowserTab {
    pub id: u64,
    pub url: String,
    pub title: String,
    pub loading: bool,
    pub can_go_back: bool,
    pub can_go_forward: bool,
    pub error: Option<String>,
}

impl BrowserTab {
    /// What the tab strip shows: the page title, else the host.
    pub fn label(&self) -> String {
        let title = self.title.trim();
        if title.is_empty() {
            url_host(&self.url)
        } else {
            title.to_owned()
        }
    }
}

/// Page state reported by the native webview.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WebViewEvent {
    /// A navigation started, possibly to a redirect target.
    LoadStarted { url: String },
    /// The page finished loading.
    LoadFinished { url: String },
    /// The document title changed.
    TitleChanged { title: String },
    /// The webview's back/forward availability changed.
    HistoryChanged {
        can_go_back: bool,
        can_go_forward: bool,
    },
    /// A top-level navigation was refused before it reached the page.
    NavigationBlocked { url: String },
    /// A page asked for a new window (`target="_blank"`, `window.open`).
    /// The browser opens it as a tab instead of a second window.
    NewWindowTab { url: String },
    /// Real user input inside the page: a native key/pointer/scroll event, a
    /// WebView2 controller focus change, or a page-side request. It only keeps
    /// presence fresh; the surface decides whether it counts, and it never
    /// grants keyboard focus.
    UserActivity,
}

/// Retained browser state for one workspace.
#[derive(Clone, Debug)]
pub struct BrowserModel {
    tabs: Vec<BrowserTab>,
    active: usize,
    next_id: u64,
}

impl BrowserModel {
    pub fn new(initial_url: &str) -> Self {
        let mut model = BrowserModel {
            tabs: Vec::new(),
            active: 0,
            next_id: 1,
        };
        let url = normalize_url(initial_url);
        model.push_tab(url);
        model
    }

    pub fn tabs(&self) -> &[BrowserTab] {
        &self.tabs
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    pub fn active(&self) -> &BrowserTab {
        &self.tabs[self.active]
    }

    /// Selects a retained tab without reloading its native page.
    pub fn set_active(&mut self, index: usize) -> bool {
        if index >= self.tabs.len() || index == self.active {
            return false;
        }
        self.active = index;
        true
    }

    /// Opens a tab for `input` and selects it. A refused URL records its error
    /// on the active tab instead of creating one.
    pub fn open_tab(&mut self, input: &str) -> Result<usize, NavigationError> {
        match resolve_navigation(input) {
            Ok(url) => {
                self.push_tab(url);
                self.active = self.tabs.len() - 1;
                Ok(self.active)
            }
            Err(error) => {
                self.record_error(&error);
                Err(error)
            }
        }
    }

    /// Closes a tab. The last tab stays open so the browser always has a page
    /// to show. Returns whether the active tab selection moved.
    pub fn close_tab(&mut self, index: usize) -> bool {
        if self.tabs.len() <= 1 || index >= self.tabs.len() {
            return false;
        }
        let active_before = self.active().id;
        self.tabs.remove(index);
        if index < self.active {
            self.active -= 1;
        } else if index == self.active {
            self.active = index.min(self.tabs.len() - 1);
        }
        self.active().id != active_before
    }

    /// Loads `input` in the active tab. The URL is applied optimistically like
    /// the app does; the webview's own load events correct it on redirects.
    pub fn navigate(&mut self, input: &str) -> Result<String, NavigationError> {
        match resolve_navigation(input) {
            Ok(url) => {
                let tab = &mut self.tabs[self.active];
                tab.url = url.clone();
                tab.loading = true;
                tab.error = None;
                Ok(url)
            }
            Err(error) => {
                self.record_error(&error);
                Err(error)
            }
        }
    }

    /// Reloads the active page. There is no webview stop API, so a reload is
    /// also how a loading page is retried.
    pub fn reload(&mut self) {
        self.note_navigation_started();
    }

    /// A native history action (back/forward) was issued and the page will
    /// report its own load events next.
    pub fn note_navigation_started(&mut self) {
        let tab = &mut self.tabs[self.active];
        tab.loading = true;
        tab.error = None;
    }

    /// A native action (navigation, reload, tab switch) could not be issued.
    pub fn note_load_failure(&mut self, message: impl Into<String>) {
        let tab = &mut self.tabs[self.active];
        tab.loading = false;
        tab.error = Some(message.into());
    }

    /// Records webview back/forward availability. Returns whether it changed.
    pub fn set_history(&mut self, can_go_back: bool, can_go_forward: bool) -> bool {
        let tab = &mut self.tabs[self.active];
        if tab.can_go_back == can_go_back && tab.can_go_forward == can_go_forward {
            return false;
        }
        tab.can_go_back = can_go_back;
        tab.can_go_forward = can_go_forward;
        true
    }

    /// Routes callbacks to their originating retained tab. Closed tabs cannot
    /// mutate the active page. Popups create and select a new tab even when
    /// their origin is backgrounded; ordinary callbacks preserve selection.
    pub fn apply_tab_event(&mut self, tab_id: u64, event: WebViewEvent) -> bool {
        let Some(index) = self.tabs.iter().position(|tab| tab.id == tab_id) else {
            return false;
        };
        let selected = self.active().id;
        let popup = matches!(event, WebViewEvent::NewWindowTab { .. });
        self.active = index;
        let changed = self.apply_webview_event(event);
        if !popup || self.active().id == tab_id {
            self.active = self.tabs.iter().position(|tab| tab.id == selected).unwrap();
        }
        changed
    }

    /// Applies an event to the active tab. Native callers use `apply_tab_event`.
    pub fn apply_webview_event(&mut self, event: WebViewEvent) -> bool {
        match event {
            // Presence freshness carries no page state; the surface consumes it
            // before it reaches the model.
            WebViewEvent::UserActivity => false,
            WebViewEvent::LoadStarted { url } => {
                let tab = &mut self.tabs[self.active];
                let next = normalize_url(&url);
                let changed = tab.url != next || !tab.loading || tab.error.is_some();
                tab.url = next;
                tab.loading = true;
                tab.error = None;
                changed
            }
            WebViewEvent::LoadFinished { url } => {
                let tab = &mut self.tabs[self.active];
                let next = normalize_url(&url);
                let changed = tab.url != next || tab.loading || tab.error.is_some();
                tab.url = next;
                tab.loading = false;
                tab.error = None;
                changed
            }
            WebViewEvent::TitleChanged { title } => {
                let title = title.trim().to_owned();
                let tab = &mut self.tabs[self.active];
                if title.is_empty() || tab.title == title {
                    return false;
                }
                tab.title = title;
                true
            }
            WebViewEvent::HistoryChanged {
                can_go_back,
                can_go_forward,
            } => self.set_history(can_go_back, can_go_forward),
            WebViewEvent::NavigationBlocked { url } => {
                let message = match resolve_navigation(&url) {
                    Err(error) => error.message(),
                    // The native handler only refuses URLs this policy refuses;
                    // keep the reason visible if the two ever disagree.
                    Ok(_) => format!("Blocked browser URL: {url}"),
                };
                let tab = &mut self.tabs[self.active];
                tab.loading = false;
                tab.error = Some(message);
                true
            }
            WebViewEvent::NewWindowTab { url } => {
                // A refused URL records its error on the current tab instead of
                // opening, which is still a visible state change.
                let _ = self.open_tab(&url);
                true
            }
        }
    }

    fn push_tab(&mut self, url: String) {
        let id = self.next_id;
        self.next_id += 1;
        self.tabs.push(BrowserTab {
            id,
            url,
            title: String::new(),
            loading: false,
            can_go_back: false,
            can_go_forward: false,
            error: None,
        });
    }

    fn record_error(&mut self, error: &NavigationError) {
        let tab = &mut self.tabs[self.active];
        tab.loading = false;
        tab.error = Some(error.message());
    }
}

/// Fills in the scheme the way `normalizeBrowserUrl` does.
pub fn normalize_url(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return DEFAULT_BROWSER_URL.to_owned();
    }
    if looks_like_local_address(trimmed) {
        return format!("http://{trimmed}");
    }
    if split_scheme(trimmed).is_some() {
        return trimmed.to_owned();
    }
    if trimmed.starts_with("//") {
        return format!("https:{trimmed}");
    }
    format!("https://{trimmed}")
}

/// Filters top-level navigation requests delivered by the native policy
/// callback; it is not a sandbox for page JavaScript or subresource requests.
pub fn resolve_navigation(input: &str) -> Result<String, NavigationError> {
    let trimmed = input.trim();
    let normalized = normalize_url(trimmed);
    if normalized == BLANK_URL {
        return Ok(normalized);
    }
    let Some((scheme, rest)) = split_scheme(&normalized) else {
        return Err(NavigationError::InvalidUrl);
    };
    let lower = scheme.to_ascii_lowercase();
    if lower != "http" && lower != "https" {
        return Err(NavigationError::UnsupportedProtocol(format!("{scheme}:")));
    }
    if host_of(rest).is_empty() {
        return Err(NavigationError::InvalidUrl);
    }
    Ok(normalized)
}

/// Whether the navigation handler should let `url` load.
pub fn is_allowed_navigation(url: &str) -> bool {
    resolve_navigation(url).is_ok()
}

/// The host of a URL, used as the tab label before a page reports a title.
pub fn url_host(url: &str) -> String {
    let Some((scheme, rest)) = split_scheme(url) else {
        return url.to_owned();
    };
    if scheme.eq_ignore_ascii_case("about") {
        return url.to_owned();
    }
    let host = host_of(rest);
    if host.is_empty() {
        return url.to_owned();
    }
    host.strip_prefix("www.").unwrap_or(host).to_owned()
}

fn split_scheme(value: &str) -> Option<(&str, &str)> {
    let index = value.find(':')?;
    let scheme = &value[..index];
    let mut chars = scheme.chars();
    let first = chars.next()?;
    if !first.is_ascii_alphabetic() {
        return None;
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')) {
        return None;
    }
    Some((scheme, &value[index + 1..]))
}

fn host_of(rest: &str) -> &str {
    let rest = rest.strip_prefix("//").unwrap_or(rest);
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    &rest[..end]
}

/// `localhost:3000`, `127.0.0.1:8080` and `[::1]:3000` get plain http; a
/// hostname that merely starts with those characters does not.
fn looks_like_local_address(value: &str) -> bool {
    let rest = if let Some(rest) = value.strip_prefix("localhost") {
        rest
    } else if let Some(rest) = ipv4_prefix(value) {
        rest
    } else if let Some(rest) = bracketed_ipv6_prefix(value) {
        rest
    } else {
        return false;
    };
    rest.is_empty()
        || rest.starts_with(':')
        || rest.starts_with('/')
        || rest.starts_with('?')
        || rest.starts_with('#')
}

fn ipv4_prefix(value: &str) -> Option<&str> {
    let bytes = value.as_bytes();
    let mut index = 0;
    for part in 0..4 {
        let start = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        let digits = index - start;
        if digits == 0 || digits > 3 {
            return None;
        }
        if part < 3 {
            if index >= bytes.len() || bytes[index] != b'.' {
                return None;
            }
            index += 1;
        }
    }
    Some(&value[index..])
}

fn bracketed_ipv6_prefix(value: &str) -> Option<&str> {
    let rest = value.strip_prefix('[')?;
    let end = rest.find(']')?;
    Some(&rest[end + 1..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_events_update_the_origin_and_ignore_closed_tabs() {
        let mut model = BrowserModel::new("https://one.example");
        let one = model.active().id;
        model.open_tab("https://two.example").unwrap();
        let two = model.active().id;
        assert!(model.apply_tab_event(
            one,
            WebViewEvent::TitleChanged {
                title: "One".into()
            }
        ));
        assert_eq!(model.active().id, two);
        assert_eq!(model.tabs()[0].title, "One");
        assert!(!model.close_tab(0));
        assert!(!model.apply_tab_event(
            one,
            WebViewEvent::LoadFinished {
                url: "https://stale.example".into()
            }
        ));
        assert_eq!(model.active().id, two);
        assert_eq!(model.active().url, "https://two.example");
    }

    #[test]
    fn background_popup_selects_a_new_retained_tab_without_clobbering_others() {
        let mut model = BrowserModel::new("https://one.example");
        let one = model.active().id;
        model.open_tab("https://two.example").unwrap();
        let two = model.active().id;
        assert!(model.apply_tab_event(
            one,
            WebViewEvent::NewWindowTab {
                url: "https://three.example".into()
            }
        ));
        assert_eq!(model.active().url, "https://three.example");
        assert_ne!(model.active().id, two);
        assert_eq!(model.tabs()[0].url, "https://one.example");
        assert_eq!(model.tabs()[1].url, "https://two.example");
        // Invalid popup records the origin error, preserving selected page.
        let three = model.active().id;
        assert!(model.apply_tab_event(
            one,
            WebViewEvent::NewWindowTab {
                url: "file:///etc/passwd".into()
            }
        ));
        assert_eq!(model.active().id, three);
        assert!(model.tabs()[0].error.is_some());
    }

    #[test]
    fn normalizes_addresses_like_the_app() {
        assert_eq!(normalize_url(""), DEFAULT_BROWSER_URL);
        assert_eq!(normalize_url("  example.com  "), "https://example.com");
        assert_eq!(normalize_url("localhost:3000"), "http://localhost:3000");
        assert_eq!(normalize_url("127.0.0.1:8080/x"), "http://127.0.0.1:8080/x");
        assert_eq!(normalize_url("[::1]:3000"), "http://[::1]:3000");
        // A hostname that merely starts with a local-looking prefix is not one.
        assert_eq!(
            normalize_url("localhost.example.com"),
            "https://localhost.example.com"
        );
        assert_eq!(normalize_url("//example.com/a"), "https://example.com/a");
        assert_eq!(
            normalize_url("https://example.com/a?b#c"),
            "https://example.com/a?b#c"
        );
    }

    #[test]
    fn blocks_unsupported_protocols_and_keeps_the_committed_url() {
        let mut model = BrowserModel::new("https://example.com/docs");
        let error = model.navigate("file:///etc/passwd").unwrap_err();
        assert_eq!(error.message(), "Blocked unsupported browser URL: file:");
        assert_eq!(model.active().url, "https://example.com/docs");
        assert!(!model.active().loading);
        assert_eq!(
            model.active().error.as_deref(),
            Some("Blocked unsupported browser URL: file:")
        );

        assert!(model.navigate("javascript:alert(1)").is_err());
        assert!(model.navigate("data:text/html,<p>x</p>").is_err());
        assert!(model.navigate("about:config").is_err());
        assert_eq!(model.navigate("about:blank").unwrap(), "about:blank");
        assert_eq!(
            resolve_navigation("https://").unwrap_err().message(),
            "Invalid browser URL"
        );
    }

    #[test]
    fn navigation_waits_for_a_committed_load() {
        let mut model = BrowserModel::new("https://example.com");
        assert_eq!(
            model.navigate("example.com/docs").unwrap(),
            "https://example.com/docs"
        );
        assert!(model.active().loading);
        assert_eq!(model.active().url, "https://example.com/docs");

        // A redirect reports a different URL than the one requested.
        assert!(model.apply_webview_event(WebViewEvent::LoadStarted {
            url: "https://example.com/docs/".into()
        }));
        assert_eq!(model.active().url, "https://example.com/docs/");
        assert!(model.active().loading);

        assert!(model.apply_webview_event(WebViewEvent::LoadFinished {
            url: "https://example.com/docs/final".into()
        }));
        assert_eq!(model.active().url, "https://example.com/docs/final");
        assert!(!model.active().loading);
        assert!(model.active().error.is_none());
    }

    #[test]
    fn load_failures_keep_the_last_good_url_until_the_next_load() {
        let mut model = BrowserModel::new("https://example.com");
        model.navigate("https://example.com/offline").unwrap();
        model.note_load_failure("Failed to load page");
        assert_eq!(model.active().url, "https://example.com/offline");
        assert!(!model.active().loading);
        assert_eq!(model.active().error.as_deref(), Some("Failed to load page"));

        model.reload();
        assert!(model.active().loading);
        assert!(model.active().error.is_none());
        assert!(model.apply_webview_event(WebViewEvent::LoadFinished {
            url: "https://example.com/offline".into()
        }));
        assert!(model.active().error.is_none());
    }

    #[test]
    fn blocked_navigation_keeps_the_page_and_reports_the_protocol() {
        let mut model = BrowserModel::new("https://example.com");
        assert!(model.apply_webview_event(WebViewEvent::NavigationBlocked {
            url: "file:///etc/passwd".into()
        }));
        assert_eq!(model.active().url, "https://example.com");
        assert_eq!(
            model.active().error.as_deref(),
            Some("Blocked unsupported browser URL: file:")
        );
        assert!(!model.active().loading);
    }

    #[test]
    fn new_window_requests_open_as_tabs() {
        let mut model = BrowserModel::new("https://example.com");
        assert!(model.apply_webview_event(WebViewEvent::NewWindowTab {
            url: "https://example.org/target".into()
        }));
        assert_eq!(model.tabs().len(), 2);
        assert_eq!(model.active().url, "https://example.org/target");
    }

    #[test]
    fn titles_and_history_flags_only_report_real_changes() {
        let mut model = BrowserModel::new("https://example.com");
        assert!(!model.apply_webview_event(WebViewEvent::TitleChanged {
            title: "   ".into()
        }));
        assert!(model.apply_webview_event(WebViewEvent::TitleChanged {
            title: "Example Domain".into()
        }));
        assert_eq!(model.active().label(), "Example Domain");
        assert!(!model.apply_webview_event(WebViewEvent::TitleChanged {
            title: "Example Domain".into()
        }));

        assert!(model.set_history(true, false));
        assert!(!model.set_history(true, false));
        assert!(model.set_history(true, true));
        assert!(model.active().can_go_forward);
    }

    #[test]
    fn tab_labels_fall_back_to_the_host() {
        let model = BrowserModel::new("https://www.example.com/docs");
        assert_eq!(model.active().label(), "example.com");
        assert_eq!(url_host("about:blank"), "about:blank");
    }

    #[test]
    fn new_tabs_start_selected_and_blocked_targets_create_none() {
        let mut model = BrowserModel::new("https://example.com");
        assert_eq!(model.open_tab("example.org").unwrap(), 1);
        assert_eq!(model.tabs().len(), 2);
        assert_eq!(model.active().url, "https://example.org");
        assert_eq!(model.tabs()[0].url, "https://example.com");

        assert!(model.open_tab("file:///etc/passwd").is_err());
        assert_eq!(model.tabs().len(), 2);
        assert_eq!(
            model.active().error.as_deref(),
            Some("Blocked unsupported browser URL: file:")
        );
    }

    #[test]
    fn closing_tabs_selects_a_neighbour_and_keeps_the_last_one() {
        let mut model = BrowserModel::new("https://a.example");
        model.open_tab("https://b.example").unwrap();
        model.open_tab("https://c.example").unwrap();
        assert_eq!(model.active_index(), 2);

        // Closing the active last tab falls back to the new last tab.
        assert!(model.close_tab(2));
        assert_eq!(model.active_index(), 1);
        assert_eq!(model.active().url, "https://b.example");

        // Closing a tab before the active one keeps the same page selected.
        model.open_tab("https://c.example").unwrap();
        assert!(!model.close_tab(0));
        assert_eq!(model.active_index(), 1);
        assert_eq!(model.active().url, "https://c.example");

        // The browser always keeps one page: the last close is refused.
        assert!(model.close_tab(model.active_index()));
        assert_eq!(model.tabs().len(), 1);
        assert!(!model.close_tab(0));
    }

    #[test]
    fn page_focus_ownership_keeps_the_keyboard_with_the_address_bar_and_the_page() {
        // A GTK/WebView2 read can go stale while the keyboard sits in GPUI
        // chrome: the location bar is not one of its widgets, so nothing tells
        // it the keyboard left. The GPUI handle wins.
        let address_bar = browser_focus_owner(true, true, false);
        assert_eq!(address_bar, BrowserFocusOwner::AddressBar);
        assert!(address_bar.is_focused());
        assert!(!address_bar.surface_may_take_focus());
        // With nothing in GPUI chrome focused, the platform read decides.
        let page = browser_focus_owner(true, false, true);
        assert_eq!(page, BrowserFocusOwner::Page);
        assert!(page.is_focused());
        assert!(!page.surface_may_take_focus());
        // Nothing focused yet: the pane may point GPUI focus at the surface.
        assert_eq!(
            browser_focus_owner(false, false, false),
            BrowserFocusOwner::None
        );
        assert!(browser_focus_owner(false, false, false).surface_may_take_focus());
        assert!(!browser_focus_owner(false, false, false).is_focused());
        assert!(browser_focus_owner(false, false, true).surface_may_take_focus());
    }

    #[test]
    fn a_page_stays_hidden_until_this_frame_applied_bounds() {
        // A page created or re-selected with the previous page's retained
        // bounds is not measured for this frame yet: showing it there would
        // paint it at the old size until the next measurement.
        assert!(!page_may_show(true, true, false, false));
        assert!(page_may_show(true, true, false, true));
        // The other gates are unchanged: a hidden view, an overlay, or no
        // embedded page keeps hiding it.
        assert!(!page_may_show(true, true, true, true));
        assert!(!page_may_show(true, false, false, true));
        assert!(!page_may_show(false, true, false, true));
    }
}

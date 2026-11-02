use std::sync::{Arc, RwLock};

use gpui::{
    App, Bounds, Context, Div, FocusHandle, Focusable, FontWeight, MouseButton, Pixels, Render,
    ScrollHandle, Stateful, Window, canvas, div, point, prelude::*, px, rgb,
};
use paseo_desktop::appearance::{self, Palette};
use paseo_desktop::locales::{self, Language, Locale, Tx};
use paseo_desktop::settings::{self, Settings, SettingsStore, ThemePreference};
use serde_json::json;

use crate::{
    choice_menu::{ChoiceMenu, Pick},
    chrome::icon,
    theme::font_size,
};

/// Settings shared between the window shell and every pane. The shell owns
/// writes; panes read the active appearance for their own rendering.
#[derive(Clone, Default)]
pub struct SharedSettings(Arc<RwLock<Settings>>);

impl SharedSettings {
    pub fn new(settings: Settings) -> Self {
        Self(Arc::new(RwLock::new(settings)))
    }

    pub fn get(&self) -> Settings {
        self.0.read().expect("settings lock").clone()
    }

    pub fn set(&self, settings: Settings) {
        *self.0.write().expect("settings lock") = settings;
    }
}

pub fn system_dark(window: &Window) -> bool {
    matches!(
        window.appearance(),
        gpui::WindowAppearance::Dark | gpui::WindowAppearance::VibrantDark
    )
}

pub fn active_palette(shared: &SharedSettings, window: &Window) -> Palette {
    appearance::resolve(shared.get().theme, system_dark(window))
}

/// The theme picker option's catalog key (`settings.appearance.theme.options`).
fn theme_option_key(theme: ThemePreference) -> &'static str {
    match theme {
        ThemePreference::Auto => "auto",
        ThemePreference::Light => "light",
        ThemePreference::Dark => "dark",
        ThemePreference::Zinc => "zinc",
        ThemePreference::Midnight => "midnight",
        ThemePreference::Claude => "claude",
        ThemePreference::Ghostty => "ghostty",
        ThemePreference::PureBlack => "pureBlack",
    }
}

pub enum SettingsPaneEvent {
    /// Settings were persisted; the shell re-resolves its appearance.
    Changed,
    Close,
    OpenPlugins,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FieldId {
    InterfaceFont,
    InterfaceSize,
    ContentSize,
    CodeFont,
    CodeSize,
    ContentWidth,
    Scrollback,
}

impl FieldId {
    fn bounds(self) -> (i64, i64) {
        match self {
            Self::InterfaceSize => (
                settings::MIN_UI_BASE_FONT_SIZE,
                settings::MAX_UI_BASE_FONT_SIZE,
            ),
            Self::ContentSize => (
                settings::MIN_CONTENT_FONT_SIZE,
                settings::MAX_CONTENT_FONT_SIZE,
            ),
            Self::CodeSize => (settings::MIN_CODE_FONT_SIZE, settings::MAX_CODE_FONT_SIZE),
            Self::ContentWidth => (
                settings::MIN_CONTENT_MAX_WIDTH,
                settings::MAX_CONTENT_MAX_WIDTH,
            ),
            Self::Scrollback => (
                settings::MIN_TERMINAL_SCROLLBACK_LINES,
                settings::MAX_TERMINAL_SCROLLBACK_LINES,
            ),
            Self::InterfaceFont | Self::CodeFont => (0, 0),
        }
    }

    fn numeric(self) -> bool {
        !matches!(self, Self::InterfaceFont | Self::CodeFont)
    }

    fn committed(self, settings: &Settings) -> String {
        match self {
            Self::InterfaceFont => settings.ui_font_family.clone(),
            Self::CodeFont => settings.mono_font_family.clone(),
            Self::InterfaceSize => settings.ui_base_font_size.to_string(),
            Self::ContentSize => settings.content_font_size.to_string(),
            Self::CodeSize => settings.code_font_size.to_string(),
            Self::ContentWidth => settings
                .content_max_width
                .unwrap_or(settings::DEFAULT_CONTENT_MAX_WIDTH)
                .to_string(),
            Self::Scrollback => settings.terminal_scrollback_lines.to_string(),
        }
    }

    fn with_value(self, settings: &Settings, value: i64) -> Settings {
        let mut next = settings.clone();
        match self {
            Self::InterfaceSize => next.ui_base_font_size = value,
            Self::ContentSize => next.content_font_size = value,
            Self::CodeSize => next.code_font_size = value,
            Self::ContentWidth => next.content_max_width = Some(value),
            Self::Scrollback => next.terminal_scrollback_lines = value,
            Self::InterfaceFont | Self::CodeFont => unreachable!("font families commit as text"),
        }
        next
    }

    /// Whether `text` is a rendering of `committed` rather than a different
    /// value: equal, equal after clamping digits, or equal after trimming a
    /// family.
    fn matches_committed(self, text: &str, committed: &str) -> bool {
        if text == committed {
            return true;
        }
        if self.numeric() {
            let (min, max) = self.bounds();
            text.parse::<i64>().ok().is_some_and(|value| {
                paseo_desktop::settings::parse_clamped_font_size(
                    &serde_json::json!(value),
                    min,
                    max,
                ) == committed.parse::<i64>().ok()
            })
        } else {
            paseo_desktop::settings::sanitize_font_family(&serde_json::json!(text))
                == Some(committed.to_owned())
        }
    }
}

/// A field's visible text is stale when it is only a rendering of the value it
/// was last synchronized from (`shown`) or of the newly committed value. Text
/// that differs from both is a draft the user typed and must survive a merge
/// that changed another window's fields.
fn field_text_is_stale(field: FieldId, text: &str, shown: Option<&str>, committed: &str) -> bool {
    if text == committed {
        return false;
    }
    shown.is_some_and(|shown| field.matches_committed(text, shown))
        || field.matches_committed(text, committed)
}

pub struct SettingsPane {
    store: Option<SettingsStore>,
    store_error: Option<String>,
    shared: SharedSettings,
    settings: Settings,
    load_error: Option<String>,
    save_error: Option<String>,
    pending_save: Option<Settings>,
    fields: Vec<(FieldId, gpui::Entity<crate::text_field::TextField>)>,
    /// Committed text each field was last synchronized from. A field whose text
    /// still matches it is a stale rendering, not an uncommitted draft.
    field_committed: Vec<(FieldId, String)>,
    field_subscriptions: Vec<gpui::Subscription>,
    theme_menu: Option<(gpui::Entity<ChoiceMenu>, gpui::Subscription)>,
    theme_bounds: Bounds<Pixels>,
    language_menu: Option<(gpui::Entity<ChoiceMenu>, gpui::Subscription)>,
    language_bounds: Bounds<Pixels>,
    scroll: ScrollHandle,
    focus: FocusHandle,
    _appearance: gpui::Subscription,
}

impl SettingsPane {
    pub fn new(
        store: Option<SettingsStore>,
        store_error: Option<String>,
        shared: SharedSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (settings, load_error) = match &store {
            Some(store) => match store.load() {
                Ok(settings) => (settings, None),
                Err(error) => (Settings::default(), Some(format!("{error:#}"))),
            },
            None => (
                Settings::default(),
                Some(store_error.clone().unwrap_or_else(|| {
                    locales::translate(
                        locales::active_locale(Settings::default().language),
                        Tx::key("desktop.nativeSettings.directoryUnavailable"),
                    )
                })),
            ),
        };
        let mut fields = Vec::new();
        for field in [
            FieldId::InterfaceFont,
            FieldId::InterfaceSize,
            FieldId::ContentSize,
            FieldId::CodeFont,
            FieldId::CodeSize,
            FieldId::ContentWidth,
            FieldId::Scrollback,
        ] {
            let entity = cx.new(|cx| {
                crate::text_field::TextField::new(
                    &field.committed(&settings),
                    "",
                    field.numeric(),
                    match field {
                        FieldId::InterfaceFont | FieldId::CodeFont => 280.,
                        FieldId::ContentWidth => 80.,
                        FieldId::Scrollback => 112.,
                        _ => 64.,
                    },
                    field.numeric(),
                    shared.clone(),
                    cx,
                )
            });
            entity.update(cx, |field_view, cx| {
                field_view.arm_commit_on_blur(window, cx)
            });
            fields.push((field, entity));
        }
        let field_committed = fields
            .iter()
            .map(|(field, _)| (*field, field.committed(&settings)))
            .collect();
        let mut pane = Self {
            store,
            store_error,
            shared,
            settings,
            load_error,
            save_error: None,
            pending_save: None,
            fields,
            field_committed,
            field_subscriptions: Vec::new(),
            theme_menu: None,
            theme_bounds: Bounds::default(),
            language_menu: None,
            language_bounds: Bounds::default(),
            scroll: ScrollHandle::new(),
            focus: cx.focus_handle(),
            _appearance: cx.observe_window_appearance(window, |_, _, cx| cx.notify()),
        };
        let subscriptions = pane
            .fields
            .clone()
            .into_iter()
            .map(|(field, entity)| {
                cx.subscribe_in(
                    &entity,
                    window,
                    move |this, _, event: &crate::text_field::FieldEvent, _, cx| match event {
                        crate::text_field::FieldEvent::Edited => {}
                        crate::text_field::FieldEvent::Committed => this.commit_field(field, cx),
                    },
                )
            })
            .collect::<Vec<_>>();
        pane.field_subscriptions = subscriptions;
        pane
    }

    fn locale(&self) -> Locale {
        locales::active_locale(self.settings.language)
    }

    fn tr(&self, key: &str) -> String {
        locales::translate(self.locale(), Tx::key(key))
    }

    fn tr_args(&self, key: &str, args: &[(&str, &str)]) -> String {
        locales::translate(
            self.locale(),
            Tx {
                key,
                args,
                ..Tx::key("")
            },
        )
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let loaded = match &self.store {
            Some(store) => store.load(),
            None => Err(anyhow::anyhow!(
                "{}",
                self.store_error
                    .clone()
                    .unwrap_or_else(|| { self.tr("desktop.nativeSettings.directoryUnavailable") })
            )),
        };
        match loaded {
            Ok(settings) => {
                self.settings = settings.clone();
                self.load_error = None;
                // The document on disk is authoritative after an explicit
                // reload; a failed-save attempt no longer applies.
                self.pending_save = None;
                self.save_error = None;
                self.shared.set(settings);
                self.reset_fields(cx);
                self.remember_committed_fields();
                self.notify_fields(cx);
                cx.emit(SettingsPaneEvent::Changed);
            }
            Err(error) => self.load_error = Some(format!("{error:#}")),
        }
        cx.notify();
    }

    /// The snapshot the next edit builds on: the failed-save attempt when one
    /// is pending, so a later commit never drops an earlier unacknowledged
    /// change.
    fn base(&self) -> Settings {
        self.pending_save
            .clone()
            .unwrap_or_else(|| self.settings.clone())
    }

    /// Rewrite a field's visible text when it is a stale rendering of a
    /// committed value (trimmed family, "007" instead of 7); a genuinely
    /// uncommitted draft is never touched. The compare uses the text the field
    /// was last synchronized from as well as the new value, so another window's
    /// merged change replaces the old rendering instead of being mistaken for a
    /// draft.
    fn sync_field_text(&self, field: FieldId, cx: &mut Context<Self>) {
        let Some((_, entity)) = self.fields.iter().find(|(id, _)| *id == field) else {
            return;
        };
        let committed = field.committed(&self.settings);
        let shown = self
            .field_committed
            .iter()
            .find(|(id, _)| *id == field)
            .map(|(_, text)| text.as_str());
        let text = entity.read(cx).text().to_owned();
        if field_text_is_stale(field, &text, shown, &committed) {
            entity.update(cx, |view, cx| view.set_text(&committed, cx));
        }
    }

    /// Record the committed value each field now renders from.
    fn remember_committed_fields(&mut self) {
        self.field_committed = self
            .fields
            .iter()
            .map(|(field, _)| (*field, field.committed(&self.settings)))
            .collect();
    }

    /// Repaint every input so a theme or interface size change reaches chrome
    /// the fields render from shared settings.
    fn notify_fields(&self, cx: &mut Context<Self>) {
        for (_, entity) in &self.fields {
            entity.update(cx, |view, cx| view.settings_changed(cx));
        }
    }

    /// Refresh every field rendering after the committed settings changed.
    fn sync_fields(&mut self, cx: &mut Context<Self>) {
        let fields = self.fields.clone();
        for (field, _) in fields {
            self.sync_field_text(field, cx);
        }
    }

    /// Rewrite every field from the committed settings; used when the disk
    /// document is reloaded wholesale.
    fn reset_fields(&mut self, cx: &mut Context<Self>) {
        for (field, entity) in &self.fields {
            let committed = field.committed(&self.settings);
            entity.update(cx, |view, cx| view.set_text(&committed, cx));
        }
    }

    fn commit_field(&mut self, field: FieldId, cx: &mut Context<Self>) {
        let Some((_, entity)) = self.fields.iter().find(|(id, _)| *id == field) else {
            return;
        };
        let text = entity.read(cx).text().to_owned();
        // Compare against and build on the pending snapshot so an edit made
        // after a failed save is not lost when another field commits.
        let base = self.base();
        let committed_text = field.committed(&base);
        if field.numeric() {
            let (min, max) = field.bounds();
            let parsed = if text.is_empty() {
                None
            } else {
                paseo_desktop::settings::parse_clamped_font_size(&json!(text), min, max)
            };
            match parsed {
                Some(value) => {
                    if text != committed_text {
                        self.persist(field.with_value(&base, value), cx);
                    }
                }
                None => {
                    // Invalid input keeps the committed value, like the app.
                    entity.update(cx, |view, cx| view.set_text(&committed_text, cx));
                }
            }
        } else {
            match paseo_desktop::settings::sanitize_font_family(&json!(text)) {
                Some(value) => {
                    if value != committed_text {
                        let mut next = base;
                        match field {
                            FieldId::InterfaceFont => next.ui_font_family = value,
                            FieldId::CodeFont => next.mono_font_family = value,
                            _ => unreachable!("handled above"),
                        }
                        self.persist(next, cx);
                    } else if text != value {
                        // The committed family only differs by trimming.
                        entity.update(cx, |view, cx| view.set_text(&value, cx));
                    }
                }
                None => entity.update(cx, |view, cx| view.set_text(&committed_text, cx)),
            }
        }
        cx.notify();
    }

    fn persist(&mut self, next: Settings, cx: &mut Context<Self>) {
        let Some(store) = self.store.clone() else {
            self.pending_save = Some(next);
            let detail = self
                .store_error
                .clone()
                .unwrap_or_else(|| self.tr("desktop.nativeSettings.directoryUnavailable"));
            self.save_error = Some(self.tr_args(
                "desktop.nativeSettings.saveFailed",
                &[("message", detail.as_str())],
            ));
            cx.notify();
            return;
        };
        // Merge this window's edits over the document currently on disk. The
        // base is the last acknowledged value, so a retry re-applies every
        // pending edit while another window's fields stay intact.
        match store.update(&self.settings, &next) {
            Ok(merged) => {
                self.settings = merged;
                self.save_error = None;
                self.pending_save = None;
                self.shared.set(self.settings.clone());
                // Only refresh stale renderings; concurrent uncommitted drafts
                // in other fields survive.
                self.sync_fields(cx);
                self.remember_committed_fields();
                self.notify_fields(cx);
                cx.emit(SettingsPaneEvent::Changed);
            }
            Err(error) => {
                // `next` already carries every earlier pending edit because it
                // was derived from the pending snapshot.
                self.pending_save = Some(next);
                let detail = format!("{error:#}");
                self.save_error = Some(self.tr_args(
                    "desktop.nativeSettings.saveFailed",
                    &[("message", detail.as_str())],
                ));
            }
        }
        cx.notify();
    }

    fn retry_save(&mut self, cx: &mut Context<Self>) {
        if let Some(next) = self.pending_save.clone() {
            self.persist(next, cx);
        } else {
            self.save_error = None;
            cx.notify();
        }
    }

    fn open_theme_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.theme_menu.is_some() {
            self.theme_menu = None;
            cx.notify();
            return;
        }
        let rows = [
            ThemePreference::Light,
            ThemePreference::Dark,
            ThemePreference::Auto,
            ThemePreference::Zinc,
            ThemePreference::Midnight,
            ThemePreference::Claude,
            ThemePreference::Ghostty,
            ThemePreference::PureBlack,
        ]
        .into_iter()
        .map(|theme| paseo_desktop::agent_controls::OptionRow {
            id: theme.as_str().to_owned(),
            label: self.tr(&format!(
                "settings.appearance.theme.options.{}",
                theme_option_key(theme)
            )),
            description: None,
            icon: None,
            is_default: false,
        })
        .collect();
        let theme_title = self.tr("settings.appearance.theme.title");
        let menu = cx.new(|cx| {
            ChoiceMenu::new(
                rows,
                Some(self.settings.theme.as_str().to_owned()),
                &theme_title,
                false,
                self.shared.clone(),
                cx,
            )
        });
        let subscription = cx.subscribe_in(&menu, window, |this, _, event: &Pick, window, cx| {
            this.theme_menu = None;
            this.focus.focus(window, cx);
            if let Some(value) = &event.0
                && let Some(theme) = ThemePreference::parse(Some(value.as_str()))
            {
                let base = this.base();
                if theme != base.theme {
                    let mut next = base;
                    next.theme = theme;
                    this.persist(next, cx);
                }
            }
            cx.notify();
        });
        menu.read(cx).focus_handle(cx).focus(window, cx);
        self.theme_menu = Some((menu, subscription));
        cx.notify();
    }

    fn theme_trigger(&self, palette: &Palette, base: f32, cx: &mut Context<Self>) -> Stateful<Div> {
        let entity = cx.entity();
        let theme = self.settings.theme;
        div()
            .id("theme-trigger")
            .h(px(32.))
            .px(px(12.))
            .flex()
            .items_center()
            .gap_2()
            .rounded(px(12.))
            .border_1()
            .border_color(rgb(palette.surface3))
            .bg(rgb(palette.surface3))
            .text_size(px(base))
            .cursor_pointer()
            .hover(|trigger| trigger.opacity(0.85))
            .when_some(theme.swatch(), |trigger, swatch| {
                trigger.child(
                    div()
                        .size(px(16.))
                        .rounded_full()
                        .border_1()
                        .border_color(rgb(palette.border))
                        .bg(rgb(swatch)),
                )
            })
            .when(theme == ThemePreference::Auto, |trigger| {
                trigger.child(icon("settings", 16.).text_color(rgb(palette.foreground_muted)))
            })
            .child(self.tr(&format!(
                "settings.appearance.theme.options.{}",
                theme_option_key(theme)
            )))
            .child(icon("chevron-down", 14.).text_color(rgb(palette.foreground_muted)))
            .child(
                canvas(
                    move |bounds, _, cx| {
                        entity.update(cx, |this, _| this.theme_bounds = bounds);
                    },
                    |_, (), _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    cx.stop_propagation();
                    this.open_theme_menu(window, cx);
                }),
            )
    }

    fn open_language_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.language_menu.is_some() {
            self.language_menu = None;
            cx.notify();
            return;
        }
        let active = self.locale();
        let system_label = self.tr("settings.general.language.options.system");
        let mut rows = vec![paseo_desktop::agent_controls::OptionRow {
            id: Language::System.as_str().to_owned(),
            label: system_label.clone(),
            description: None,
            icon: None,
            is_default: false,
        }];
        rows.extend(
            Locale::ALL
                .iter()
                .map(|locale| paseo_desktop::agent_controls::OptionRow {
                    id: locale.as_str().to_owned(),
                    label: locales::language_option_label(*locale, active),
                    description: None,
                    icon: None,
                    is_default: false,
                }),
        );
        let title = self.tr("settings.general.language.label");
        let menu = cx.new(|cx| {
            ChoiceMenu::new(
                rows,
                Some(self.settings.language.as_str().to_owned()),
                &title,
                false,
                self.shared.clone(),
                cx,
            )
        });
        let subscription = cx.subscribe_in(&menu, window, |this, _, event: &Pick, window, cx| {
            this.language_menu = None;
            this.focus.focus(window, cx);
            if let Some(value) = &event.0
                && let Some(language) = Language::parse(value.as_str())
            {
                let base = this.base();
                if language != base.language {
                    let mut next = base;
                    next.language = language;
                    this.persist(next, cx);
                }
            }
            cx.notify();
        });
        menu.read(cx).focus_handle(cx).focus(window, cx);
        self.language_menu = Some((menu, subscription));
        cx.notify();
    }

    fn language_trigger(
        &self,
        palette: &Palette,
        base: f32,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let entity = cx.entity();
        let system_label = self.tr("settings.general.language.options.system");
        let label = match self.settings.language {
            Language::System => system_label,
            Language::Explicit(locale) => locales::language_option_label(locale, self.locale()),
        };
        div()
            .id("language-trigger")
            .h(px(32.))
            .px(px(12.))
            .flex()
            .items_center()
            .gap_2()
            .rounded(px(12.))
            .border_1()
            .border_color(rgb(palette.surface3))
            .bg(rgb(palette.surface3))
            .text_size(px(base))
            .cursor_pointer()
            .hover(|trigger| trigger.opacity(0.85))
            .child(label)
            .child(icon("chevron-down", 14.).text_color(rgb(palette.foreground_muted)))
            .child(
                canvas(
                    move |bounds, _, cx| {
                        entity.update(cx, |this, _| this.language_bounds = bounds);
                    },
                    |_, (), _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    cx.stop_propagation();
                    this.open_language_menu(window, cx);
                }),
            )
    }

    fn section_header(&self, title: &str, palette: &Palette, sm: f32) -> Div {
        div()
            .mb(px(12.))
            .ml(px(4.))
            .text_size(px(sm))
            .text_color(rgb(palette.foreground_muted))
            .child(title.to_owned())
    }

    fn row(&self, title: &str, hint: &str, palette: &Palette, base: f32, sm: f32) -> Div {
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(12.))
            .px(px(16.))
            .py(px(16.))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .child(
                        div()
                            .text_size(px(base))
                            .text_color(rgb(palette.foreground))
                            .child(title.to_owned()),
                    )
                    .when(!hint.is_empty(), |title_block| {
                        title_block.child(
                            div()
                                .mt(px(4.))
                                .text_size(px(sm))
                                .text_color(rgb(palette.foreground_muted))
                                .child(hint.to_owned()),
                        )
                    }),
            )
    }

    fn numeric_control(&self, field: FieldId, palette: &Palette, base: f32) -> impl IntoElement {
        let entity = self
            .fields
            .iter()
            .find(|(id, _)| *id == field)
            .map(|(_, entity)| entity.clone());
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .children(entity)
            .child(
                div()
                    .text_size(px(base))
                    .text_color(rgb(palette.foreground_muted))
                    .child("px"),
            )
    }
}

impl gpui::EventEmitter<SettingsPaneEvent> for SettingsPane {}

impl Focusable for SettingsPane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for SettingsPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = active_palette(&self.shared, window);
        let base = appearance::scale_ui_font(font_size::BASE, self.settings.ui_base_font_size);
        let sm = appearance::scale_ui_font(font_size::SM, self.settings.ui_base_font_size);
        let lg = appearance::scale_ui_font(font_size::LG, self.settings.ui_base_font_size);

        let header = div()
            .h(px(44.))
            .flex_shrink_0()
            .px(px(16.))
            .flex()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(
                div()
                    .font_weight(FontWeight::LIGHT)
                    .text_size(px(lg))
                    .child(self.tr("settings.title")),
            )
            .child(
                div()
                    .id("close-settings")
                    .size(px(28.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .cursor_pointer()
                    .hover(|button| button.bg(rgb(palette.surface2)))
                    .child(icon("x", 16.).text_color(rgb(palette.foreground_muted)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|_, _, _, cx| {
                            cx.stop_propagation();
                            cx.emit(SettingsPaneEvent::Close);
                        }),
                    ),
            );

        let card = |rows: Vec<Div>| {
            div()
                .rounded(px(8.))
                .border_1()
                .border_color(rgb(palette.border))
                .bg(rgb(palette.surface1))
                .overflow_hidden()
                .children(rows.into_iter().enumerate().map(|(index, row)| {
                    if index == 0 {
                        row.into_any_element()
                    } else {
                        div()
                            .border_t_1()
                            .border_color(rgb(palette.border))
                            .child(row)
                            .into_any_element()
                    }
                }))
        };

        let theme_card = card(vec![
            self.row(
                &self.tr("settings.appearance.theme.title"),
                "",
                &palette,
                base,
                sm,
            )
            .child(self.theme_trigger(&palette, base, cx)),
        ]);

        let font_rows = vec![
            self.row(
                &self.tr("settings.appearance.fonts.interfaceFont"),
                &self.tr("settings.appearance.fonts.interfaceFontHint"),
                &palette,
                base,
                sm,
            )
            .child(
                self.fields
                    .iter()
                    .find(|(id, _)| *id == FieldId::InterfaceFont)
                    .map(|(_, entity)| entity.clone())
                    .unwrap(),
            ),
            self.row(
                &self.tr("settings.appearance.fonts.interfaceSize"),
                &self.tr("settings.appearance.fonts.interfaceSizeHint"),
                &palette,
                base,
                sm,
            )
            .child(self.numeric_control(FieldId::InterfaceSize, &palette, base)),
            self.row(
                &self.tr("settings.appearance.fonts.contentSize"),
                &self.tr("settings.appearance.fonts.contentSizeHint"),
                &palette,
                base,
                sm,
            )
            .child(self.numeric_control(FieldId::ContentSize, &palette, base)),
            self.row(
                &self.tr("settings.appearance.fonts.codeFont"),
                &self.tr("settings.appearance.fonts.codeFontHint"),
                &palette,
                base,
                sm,
            )
            .child(
                self.fields
                    .iter()
                    .find(|(id, _)| *id == FieldId::CodeFont)
                    .map(|(_, entity)| entity.clone())
                    .unwrap(),
            ),
            self.row(
                &self.tr("settings.appearance.fonts.codeSize"),
                &self.tr("settings.appearance.fonts.codeSizeHint"),
                &palette,
                base,
                sm,
            )
            .child(self.numeric_control(FieldId::CodeSize, &palette, base)),
        ];
        let fonts_card = card(font_rows);

        let content_width = self.settings.content_max_width;
        let mut layout_row = self.row(
            &self.tr("settings.appearance.layout.contentWidth"),
            &self.tr("settings.appearance.layout.contentWidthHint"),
            &palette,
            base,
            sm,
        );
        let mut width_controls = div().flex().items_center().gap(px(8.));
        if content_width.is_some() {
            width_controls = width_controls.child(
                div()
                    .id("reset-content-width")
                    .px_2()
                    .h(px(24.))
                    .flex()
                    .items_center()
                    .rounded(px(6.))
                    .text_size(px(sm))
                    .cursor_pointer()
                    .text_color(rgb(palette.foreground_muted))
                    .hover(|button| button.bg(rgb(palette.surface2)))
                    .child(self.tr("settings.appearance.layout.reset"))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            let mut next = this.base();
                            next.content_max_width = None;
                            this.persist(next, cx);
                        }),
                    ),
            );
        }
        width_controls =
            width_controls.child(self.numeric_control(FieldId::ContentWidth, &palette, base));
        layout_row = layout_row.child(width_controls);
        let layout_card = card(vec![layout_row]);

        let scrollback_card = card(vec![
            self.row(
                &self.tr("settings.general.terminalScrollback.label"),
                &self.tr("settings.general.terminalScrollback.description"),
                &palette,
                base,
                sm,
            )
            .child(
                self.fields
                    .iter()
                    .find(|(id, _)| *id == FieldId::Scrollback)
                    .map(|(_, entity)| entity.clone())
                    .unwrap(),
            ),
        ]);

        let plugins_row = self.row(
            &self.tr("settings.plugins.title"),
            &self.tr("desktop.nativeSettings.pluginsHint"),
            &palette,
            base,
            sm,
        );
        let plugins_card = card(vec![plugins_row])
            .id("open-plugins")
            .cursor_pointer()
            .hover(|row| row.bg(rgb(palette.surface2)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| {
                    cx.stop_propagation();
                    cx.emit(SettingsPaneEvent::OpenPlugins);
                }),
            );

        let language_row = self
            .row(
                &self.tr("settings.general.language.label"),
                &self.tr("settings.general.language.description"),
                &palette,
                base,
                sm,
            )
            .child(self.language_trigger(&palette, base, cx));
        let language_card = card(vec![language_row]);

        let mut column = div()
            .w(px(720.))
            .max_w_full()
            .flex()
            .flex_col()
            .px(px(24.))
            .py(px(24.))
            .gap(px(24.));
        if let Some(error) = self.load_error.clone() {
            column = column.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .p(px(16.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.surface1))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .text_color(rgb(palette.status_danger))
                            .text_size(px(sm))
                            .child(self.tr_args(
                                "desktop.nativeSettings.loadFailed",
                                &[("message", error.as_str())],
                            )),
                    )
                    .child(
                        div()
                            .id("retry-load-settings")
                            .text_size(px(sm))
                            .text_color(rgb(palette.foreground_muted))
                            .cursor_pointer()
                            .hover(|button| button.text_color(rgb(palette.foreground)))
                            .child(self.tr("common.actions.retry"))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.reload(cx);
                                }),
                            ),
                    ),
            );
        }
        if let Some(error) = self.save_error.clone() {
            column = column.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .p(px(16.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.surface1))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .text_color(rgb(palette.status_danger))
                            .text_size(px(sm))
                            .child(error),
                    )
                    .child(
                        div()
                            .id("retry-save-settings")
                            .text_size(px(sm))
                            .text_color(rgb(palette.foreground_muted))
                            .cursor_pointer()
                            .hover(|button| button.text_color(rgb(palette.foreground)))
                            .child(self.tr("common.actions.retry"))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.retry_save(cx);
                                }),
                            ),
                    ),
            );
        }
        column = column
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(self.section_header(&self.tr("settings.sections.general"), &palette, sm))
                    .child(language_card),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(self.section_header(
                        &self.tr("settings.sections.appearance"),
                        &palette,
                        sm,
                    ))
                    .child(theme_card),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(self.section_header(
                        &self.tr("settings.appearance.fonts.title"),
                        &palette,
                        sm,
                    ))
                    .child(fonts_card),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(self.section_header(
                        &self.tr("settings.appearance.layout.title"),
                        &palette,
                        sm,
                    ))
                    .child(layout_card),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(self.section_header(
                        &self.tr("settings.sections.terminal"),
                        &palette,
                        sm,
                    ))
                    .child(scrollback_card),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(self.section_header(&self.tr("settings.plugins.title"), &palette, sm))
                    .child(plugins_card),
            );

        let body = div()
            .id("settings-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .flex()
            // A row container stretches its children on the cross axis, which
            // would give the column the viewport height and leave the scroll
            // container with no overflow. Start-align it and keep the
            // horizontal centring.
            .items_start()
            .justify_center()
            .child(column);

        let mut root = div()
            .track_focus(&self.focus)
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .font_family(crate::fonts::ui_family_for(&self.settings.ui_font_family))
            .text_color(rgb(palette.foreground))
            .bg(rgb(palette.surface0))
            .child(header)
            .child(body);

        if let Some((menu, _)) = &self.theme_menu {
            let bounds = self.theme_bounds;
            root = root.child(gpui::deferred(
                gpui::anchored()
                    .position(point(bounds.left(), bounds.bottom() + px(4.)))
                    .anchor(gpui::Anchor::TopLeft)
                    .snap_to_window_with_margin(px(8.))
                    .child(div().w(px(200.)).child(menu.clone())),
            ));
        }
        if let Some((menu, _)) = &self.language_menu {
            let bounds = self.language_bounds;
            root = root.child(gpui::deferred(
                gpui::anchored()
                    .position(point(bounds.left(), bounds.bottom() + px(4.)))
                    .anchor(gpui::Anchor::TopLeft)
                    .snap_to_window_with_margin(px(8.))
                    .child(div().w(px(240.)).child(menu.clone())),
            ));
        }
        root
    }
}

#[cfg(test)]
mod tests {
    use super::{FieldId, field_text_is_stale};

    #[test]
    fn a_merged_value_replaces_the_old_rendering_but_not_an_edit() {
        // Window B still shows its old committed 12 while the merge returned 16.
        assert!(field_text_is_stale(
            FieldId::CodeSize,
            "12",
            Some("12"),
            "16"
        ));
        // A digit value the user actually typed survives.
        assert!(!field_text_is_stale(
            FieldId::CodeSize,
            "14",
            Some("12"),
            "16"
        ));
        // Already showing the merged value; nothing to rewrite.
        assert!(!field_text_is_stale(
            FieldId::CodeSize,
            "16",
            Some("12"),
            "16"
        ));
        // A draft that goes stale by rendering only (padding, whitespace).
        assert!(field_text_is_stale(
            FieldId::InterfaceSize,
            "014",
            Some("14"),
            "16"
        ));
    }

    #[test]
    fn a_merged_font_family_replaces_the_old_family_but_not_a_typed_family() {
        assert!(field_text_is_stale(
            FieldId::InterfaceFont,
            "Inter",
            Some("Inter"),
            "Iosevka"
        ));
        assert!(field_text_is_stale(
            FieldId::InterfaceFont,
            " Inter ",
            Some("Inter"),
            "Iosevka"
        ));
        assert!(!field_text_is_stale(
            FieldId::InterfaceFont,
            "My Font",
            Some("Inter"),
            "Iosevka"
        ));
        assert!(field_text_is_stale(
            FieldId::CodeFont,
            "JetBrains Mono",
            Some("JetBrains Mono"),
            ""
        ));
    }
}

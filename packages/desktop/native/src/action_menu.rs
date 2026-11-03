use crate::{
    action_menu_presentation as p,
    chrome::icon,
    settings_view::{SharedSettings, active_palette},
    theme::{font_size, radius},
};
use gpui::{
    AnyElement, App, Bounds, BoxShadow, Context, Display, Element, ElementId, EventEmitter,
    FocusHandle, Focusable, GlobalElementId, InspectorElementId, LayoutId, MouseButton, Pixels,
    Position, Render, Style, Window, div, point, prelude::*, px, rgb, rgba,
};

pub struct Item {
    pub id: String,
    pub label: String,
    pub icon: &'static str,
    pub enabled: bool,
    pub profile_icon: Option<Option<String>>,
    pub heading: bool,
    pub separator: bool,
    pub shortcut: Option<String>,
    pub hint: Option<String>,
    pub tooltip: Option<String>,
}
impl Item {
    pub fn new(id: &str, label: impl Into<String>, icon: &'static str) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            icon,
            enabled: true,
            profile_icon: None,
            heading: false,
            separator: false,
            shortcut: None,
            hint: None,
            tooltip: None,
        }
    }
}
pub struct Chosen(pub Option<String>);
pub struct ActionMenu {
    items: Vec<Item>,
    active: Option<usize>,
    focus: FocusHandle,
    shared: SharedSettings,
    compact_icons: bool,
    fixed_width: Option<f32>,
    measured_width: Option<(gpui::SharedString, f32, f32)>,
}
impl EventEmitter<Chosen> for ActionMenu {}
impl Focusable for ActionMenu {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
impl ActionMenu {
    pub fn new(items: Vec<Item>, shared: SharedSettings, cx: &mut Context<Self>) -> Self {
        Self {
            items,
            active: None,
            focus: cx.focus_handle(),
            shared,
            compact_icons: false,
            fixed_width: None,
            measured_width: None,
        }
    }
    pub fn launcher(mut self) -> Self {
        self.compact_icons = true;
        self
    }
    pub fn pane_menu(mut self) -> Self {
        self.compact_icons = true;
        self.fixed_width = Some(paseo_desktop::workspace_tab_layout::p::DROPDOWN_WIDTH);
        self
    }
    pub fn context_menu(mut self) -> Self {
        self.fixed_width = Some(paseo_desktop::workspace_tab_layout::p::DROPDOWN_WIDTH);
        self
    }
    fn key_down(&mut self, event: &gpui::KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let enabled: Vec<usize> = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.enabled && !item.separator && !item.heading)
            .map(|(i, _)| i)
            .collect();
        match event.keystroke.key.as_str() {
            "escape" => cx.emit(Chosen(None)),
            "enter" => {
                if let Some(item) = self
                    .active
                    .and_then(|i| self.items.get(i))
                    .filter(|item| item.enabled)
                {
                    cx.emit(Chosen(Some(item.id.clone())));
                }
            }
            "up" | "down" | "home" | "end" if !enabled.is_empty() => {
                let current = self
                    .active
                    .and_then(|i| enabled.iter().position(|j| *j == i));
                let next = match event.keystroke.key.as_str() {
                    "home" => 0,
                    "end" => enabled.len() - 1,
                    "up" => current.map_or(enabled.len() - 1, |i| {
                        (i + enabled.len() - 1) % enabled.len()
                    }),
                    _ => current.map_or(0, |i| (i + 1) % enabled.len()),
                };
                self.active = Some(enabled[next]);
                cx.notify();
            }
            _ => {}
        }
        // A menu owns keyboard input until it closes; text must not reach a PTY.
        cx.stop_propagation();
    }
}
impl Render for ActionMenu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = active_palette(&self.shared, window);
        let font_size = paseo_desktop::appearance::scale_ui_font(
            font_size::BASE,
            self.shared.get().ui_base_font_size,
        );
        let width = if let Some(fixed) = self.fixed_width {
            fixed
        } else if self.compact_icons {
            let family = crate::fonts::ui_family_for(&self.shared.get().ui_font_family);
            if self
                .measured_width
                .as_ref()
                .is_none_or(|(font, size, _)| font != &family || *size != font_size)
            {
                let width_of = |text: &str, size: f32| {
                    window
                        .text_system()
                        .shape_line(
                            text.to_owned().into(),
                            px(size),
                            &[gpui::TextRun {
                                len: text.len(),
                                font: gpui::font(family.clone()),
                                color: rgb(palette.foreground).into(),
                                background_color: None,
                                underline: None,
                                strikethrough: None,
                            }],
                            None,
                        )
                        .width
                        .to_f64() as f32
                };
                let width = self
                    .items
                    .iter()
                    .filter(|item| !item.separator)
                    .map(|item| {
                        if item.heading {
                            return 26. + width_of(&item.label, 12.);
                        }
                        28. + width_of(&item.label, font_size)
                            + if !item.icon.is_empty() || item.profile_icon.is_some() {
                                24.
                            } else {
                                0.
                            }
                            + item
                                .shortcut
                                .as_ref()
                                .map_or(0., |shortcut| 16. + width_of(shortcut, 12.))
                    })
                    .fold(200., f32::max);
                self.measured_width = Some((family, font_size, width));
            }
            self.measured_width
                .as_ref()
                .unwrap()
                .2
                .min((window.viewport_size().width.to_f64() as f32 - 16.).max(0.))
        } else {
            paseo_desktop::workspace_tab_layout::p::DROPDOWN_WIDTH
        };
        let font_sm = paseo_desktop::appearance::scale_ui_font(
            font_size::SM,
            self.shared.get().ui_base_font_size,
        );
        div()
            .id("action-menu")
            .occlude()
            .track_focus(&self.focus)
            .w(px(width))
            .max_h(window.viewport_size().height - px(16.))
            .overflow_y_scroll()
            .py_1()
            .border_1()
            .border_color(rgb(palette.border_accent))
            .rounded(px(radius::LG))
            .bg(rgb(palette.surface1))
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
            .text_color(rgb(palette.foreground))
            .text_size(px(font_size))
            .line_height(px(p::LINE_HEIGHT))
            .on_key_down(cx.listener(Self::key_down))
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(Chosen(None))))
            .children(self.items.iter().enumerate().map(|(index, item)| {
                let enabled = item.enabled;
                if item.separator {
                    return div()
                        .h(px(1.))
                        .my_1()
                        .bg(rgb(palette.border_accent))
                        .into_any_element();
                }
                if item.heading {
                    return div()
                        .px_3()
                        .pt_2()
                        .pb_1()
                        .line_height(px(20.))
                        .text_size(px(12.))
                        .text_color(rgb(palette.foreground_muted))
                        .child(item.label.clone())
                        .into_any_element();
                }
                div()
                    .id(("menu-item", index))
                    .min_h(px(p::ITEM_HEIGHT))
                    .py_1()
                    .mx_1()
                    .px_2()
                    .border_1()
                    .border_color(gpui::rgba(0))
                    .rounded(px(radius::MD))
                    .flex()
                    .items_center()
                    .gap_2()
                    .when(!enabled, |row| row.opacity(0.5))
                    .when(enabled, |row| row.cursor_pointer())
                    .when(self.active == Some(index) && enabled, |row| {
                        row.bg(rgb(palette.surface2))
                    })
                    .when(
                        !item.icon.is_empty() || item.profile_icon.is_some(),
                        |row| {
                            row.child(
                                div()
                                    .size(px(16.))
                                    .flex_shrink_0()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(
                                        if let Some(key) = &item.profile_icon {
                                            crate::chrome::terminal_profile_icon(
                                                key.as_ref().map(String::as_str),
                                                if self.compact_icons { 14. } else { 16. },
                                            )
                                        } else {
                                            icon(
                                                item.icon,
                                                if self.compact_icons { 14. } else { 16. },
                                            )
                                        }
                                        .text_color(rgb(palette.foreground_muted)),
                                    ),
                            )
                        },
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .child(item.label.clone()),
                    )
                    .when_some(item.hint.clone(), |row, hint| {
                        row.child(
                            div()
                                .flex_shrink_0()
                                .text_size(px(font_sm))
                                .text_color(rgb(palette.foreground_muted))
                                .child(hint),
                        )
                    })
                    .when_some(item.shortcut.clone(), |row, shortcut| {
                        row.child(crate::chrome::shortcut_badge(shortcut, &palette))
                    })
                    .when_some(item.tooltip.clone(), |row, tooltip| {
                        row.tooltip(move |_, cx| {
                            cx.new(|_| crate::chrome::Tooltip(tooltip.clone())).into()
                        })
                    })
                    .on_mouse_move(cx.listener(move |this, _, _, cx| {
                        if enabled && this.active != Some(index) {
                            this.active = Some(index);
                            cx.notify();
                        }
                    }))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            if let Some(item) = this.items.get(index).filter(|item| item.enabled) {
                                cx.emit(Chosen(Some(item.id.clone())));
                            }
                        }),
                    )
                    .into_any_element()
            }))
    }
}

// Layout and placement resolve in one prepaint pass, including content that changes after opening.
pub struct AnchoredMenu {
    trigger: Bounds<Pixels>,
    alignment: paseo_desktop::menu_position::Alignment,
    offset: f64,
    child: AnyElement,
}

pub fn anchored_menu(
    trigger: Bounds<Pixels>,
    alignment: paseo_desktop::menu_position::Alignment,
    offset: f64,
    child: impl IntoElement,
) -> AnchoredMenu {
    AnchoredMenu {
        trigger,
        alignment,
        offset,
        child: child.into_any_element(),
    }
}

impl IntoElement for AnchoredMenu {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for AnchoredMenu {
    type RequestLayoutState = LayoutId;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, LayoutId) {
        let child = self.child.request_layout(window, cx);
        let layout = window.request_layout(
            Style {
                position: Position::Absolute,
                display: Display::Flex,
                ..Style::default()
            },
            [child],
            cx,
        );
        (layout, child)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        child: &mut LayoutId,
        window: &mut Window,
        cx: &mut App,
    ) {
        use paseo_desktop::menu_position::{Input, Placement, Rect, Size, compute_position};
        let content = window.layout_bounds(*child);
        let viewport = window.viewport_size();
        let position = compute_position(Input {
            trigger_rect: Rect {
                x: self.trigger.left().to_f64(),
                y: self.trigger.top().to_f64(),
                width: self.trigger.size.width.to_f64(),
                height: self.trigger.size.height.to_f64(),
            },
            content_size: Size {
                width: content.size.width.to_f64(),
                height: content.size.height.to_f64(),
            },
            display_area: Rect {
                width: viewport.width.to_f64(),
                height: viewport.height.to_f64(),
                ..Rect::default()
            },
            placement: Placement::Bottom,
            alignment: self.alignment,
            offset: self.offset,
            flip_horizontal: false,
        });
        let offset = point(px(position.x as f32), px(position.y as f32)) - content.origin;
        window.with_element_offset(offset, |window| {
            self.child.prepaint(window, cx);
        });
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut LayoutId,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.paint(window, cx);
    }
}

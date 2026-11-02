use crate::{
    action_menu_presentation as p,
    chrome::icon,
    settings_view::{SharedSettings, active_palette},
    theme::{font_size, radius},
};
use gpui::{
    App, BoxShadow, Context, EventEmitter, FocusHandle, Focusable, MouseButton, Render, Window,
    div, prelude::*, px, rgb, rgba,
};

pub struct Item {
    pub id: String,
    pub label: String,
    pub icon: &'static str,
    pub enabled: bool,
}
impl Item {
    pub fn new(id: &str, label: impl Into<String>, icon: &'static str) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            icon,
            enabled: true,
        }
    }
}
pub struct Chosen(pub Option<String>);
pub struct ActionMenu {
    items: Vec<Item>,
    active: Option<usize>,
    focus: FocusHandle,
    shared: SharedSettings,
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
        }
    }
    fn key_down(&mut self, event: &gpui::KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let enabled: Vec<usize> = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.enabled)
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
        div()
            .id("action-menu")
            .occlude()
            .track_focus(&self.focus)
            .w(px(220.))
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
                div()
                    .id(("menu-item", index))
                    .h(px(p::ITEM_HEIGHT))
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
                    .child(icon(item.icon, 16.).text_color(rgb(palette.foreground_muted)))
                    .child(item.label.clone())
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
            }))
    }
}

use crate::{action_menu_presentation as p, chrome::icon, window_geometry};
use gpui::{MouseButton, Window, WindowControlArea, div, prelude::*, px, rgb};
use paseo_desktop::appearance::{Palette, interaction_background};

pub fn configure(window: &Window) -> anyhow::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        use x11rb::{
            connection::Connection,
            protocol::xproto::{ConnectionExt, PropMode},
            wrapper::ConnectionExt as _,
        };
        let id = match HasWindowHandle::window_handle(window)?.as_raw() {
            RawWindowHandle::Xcb(handle) => Some(handle.window.get()),
            RawWindowHandle::Xlib(handle) => Some(u32::try_from(handle.window)?),
            _ => None,
        };
        if let Some(id) = id {
            // GPUI requires a compositor AND _GTK_FRAME_EXTENTS before honoring Client
            // decorations on X11. Paseo's opaque, frameless content needs neither. Set
            // the same Motif no-decoration hint used by the Electron window explicitly.
            let (connection, _) = x11rb::connect(None)?;
            let atom = connection
                .intern_atom(false, b"_MOTIF_WM_HINTS")?
                .reply()?
                .atom;
            connection
                .change_property32(PropMode::REPLACE, id, atom, atom, &[1 << 1, 0, 0, 0, 0])?
                .check()?;
            connection.flush()?;
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = window;
    }
    Ok(())
}

pub fn resize_handles(window: &Window) -> Vec<gpui::AnyElement> {
    use gpui::{CursorStyle, ResizeEdge};
    if !cfg!(target_os = "linux") || window.is_maximized() || window.is_fullscreen() {
        return Vec::new();
    }
    let extent = px(4.);
    [
        ResizeEdge::Top,
        ResizeEdge::Right,
        ResizeEdge::Bottom,
        ResizeEdge::Left,
        ResizeEdge::TopLeft,
        ResizeEdge::TopRight,
        ResizeEdge::BottomLeft,
        ResizeEdge::BottomRight,
    ]
    .into_iter()
    .enumerate()
    .map(|(index, edge)| {
        let cursor = match edge {
            ResizeEdge::Top | ResizeEdge::Bottom => CursorStyle::ResizeUpDown,
            ResizeEdge::Left | ResizeEdge::Right => CursorStyle::ResizeLeftRight,
            ResizeEdge::TopLeft | ResizeEdge::BottomRight => CursorStyle::ResizeUpLeftDownRight,
            _ => CursorStyle::ResizeUpRightDownLeft,
        };
        div()
            .id(("window-resize", index))
            .absolute()
            .map(|handle| match edge {
                ResizeEdge::Top => handle.top_0().left(extent).right(extent).h(extent),
                ResizeEdge::Bottom => handle.bottom_0().left(extent).right(extent).h(extent),
                ResizeEdge::Left => handle.left_0().top(extent).bottom(extent).w(extent),
                ResizeEdge::Right => handle.right_0().top(extent).bottom(extent).w(extent),
                ResizeEdge::TopLeft => handle.top_0().left_0().size(extent),
                ResizeEdge::TopRight => handle.top_0().right_0().size(extent),
                ResizeEdge::BottomLeft => handle.bottom_0().left_0().size(extent),
                ResizeEdge::BottomRight => handle.bottom_0().right_0().size(extent),
            })
            .cursor(cursor)
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                cx.stop_propagation();
                window.start_window_resize(edge);
            })
            .into_any_element()
    })
    .collect()
}

fn control_hover(palette: &Palette, background: u32, close: bool) -> u32 {
    let (color, opacity) = if close {
        (
            window_geometry::CLOSE_HOVER_COLOR,
            window_geometry::CLOSE_HOVER_OPACITY,
        )
    } else if palette.is_dark {
        (
            p::DARK_HEADER_HIGHLIGHT_COLOR,
            p::DARK_HEADER_HIGHLIGHT_OPACITY,
        )
    } else {
        (
            p::LIGHT_HEADER_HIGHLIGHT_COLOR,
            p::LIGHT_HEADER_HIGHLIGHT_OPACITY,
        )
    };
    interaction_background(color, background, opacity)
}

pub fn controls(window: &Window, palette: &Palette, background: u32) -> impl IntoElement {
    let (mode, geometry) = if cfg!(target_os = "windows") {
        ("custom-windows", &window_geometry::WINDOWS)
    } else {
        ("custom-linux", &window_geometry::LINUX)
    };
    let palette = *palette;
    let maximize = if window.is_maximized() {
        "restore"
    } else {
        "maximize"
    };
    div()
        .id("desktop-window-controls")
        .h(px(geometry.control_height))
        .flex_shrink_0()
        .flex()
        .children(["minimize", maximize, "close"].into_iter().map(|kind| {
            let id = match kind {
                "minimize" => "window-minimize",
                "close" => "window-close",
                _ => "window-maximize",
            };
            let hover_background = rgb(control_hover(&palette, background, kind == "close"));
            let control = div()
                .id(id)
                .group(id)
                .w(px(geometry.control_width))
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .id("surface")
                        .flex()
                        .items_center()
                        .justify_center()
                        .map(|surface| {
                            if geometry.circular {
                                surface
                                    .size(px(window_geometry::CIRCLE_SIZE))
                                    .rounded(px(window_geometry::CIRCLE_RADIUS))
                            } else {
                                surface.size_full()
                            }
                        })
                        .group_hover(id, move |surface| surface.bg(hover_background))
                        .group_active(id, move |surface| surface.bg(hover_background))
                        .child(
                            icon(&format!("window-{mode}-{kind}"), geometry.glyph_size)
                                .id("glyph")
                                .text_color(rgb(palette.foreground_muted))
                                .when(kind == "close", |glyph| {
                                    glyph
                                        .group_hover(id, |glyph| glyph.text_color(rgb(0xffffff)))
                                        .group_active(id, |glyph| glyph.text_color(rgb(0xffffff)))
                                }),
                        ),
                );
            if cfg!(target_os = "windows") {
                // Let Windows own non-client buttons, including the maximize snap menu.
                control.window_control_area(match kind {
                    "minimize" => WindowControlArea::Min,
                    "close" => WindowControlArea::Close,
                    _ => WindowControlArea::Max,
                })
            } else {
                control
                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                        cx.stop_propagation();
                    })
                    .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        match kind {
                            "minimize" => window.minimize_window(),
                            "close" => window.dispatch_action(Box::new(crate::ui::CloseWindow), cx),
                            _ => window.zoom_window(),
                        }
                        window.refresh();
                    })
            }
        }))
}

pub fn drag_region() -> impl IntoElement {
    div()
        .id("titlebar-drag-region")
        .flex_1()
        .h_full()
        .window_control_area(WindowControlArea::Drag)
        .on_mouse_down(MouseButton::Left, |event, window, _| {
            if !cfg!(target_os = "windows") {
                if event.click_count == 2 {
                    window.zoom_window();
                } else {
                    window.start_window_move();
                }
            }
        })
        .on_mouse_down(MouseButton::Right, |event, window, _| {
            window.show_window_menu(event.position)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use paseo_desktop::settings::ThemePreference;

    #[test]
    fn window_control_hover_uses_the_current_rail_and_theme() {
        let dark = paseo_desktop::appearance::resolve(ThemePreference::Dark, true);
        let light = paseo_desktop::appearance::resolve(ThemePreference::Light, false);
        assert_eq!(control_hover(&dark, 0x181b1a, false), 0x2a2c2b);
        assert_eq!(control_hover(&light, 0xffffff, false), 0xf0f0f0);
        assert_eq!(control_hover(&dark, dark.surface_sidebar, false), 0x262928);
        assert_eq!(
            control_hover(&light, light.surface_sidebar, false),
            0xe5e5e6
        );
        assert_eq!(control_hover(&dark, dark.surface0, true), 0xd31122);
        assert_eq!(control_hover(&dark, dark.surface_sidebar, true), 0xd31122);
        assert_eq!(control_hover(&light, light.surface0, true), 0xea2839);
        assert_eq!(control_hover(&light, light.surface_sidebar, true), 0xe92738);
    }
}

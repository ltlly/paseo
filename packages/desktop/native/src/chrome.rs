use crate::theme::{colors as c, font_size};
use gpui::{Animation, AnimationExt, Transformation, percentage};
use gpui::{Context, Render, SharedString, Svg, Window, div, prelude::*, px, rgb, svg};

pub fn icon(name: &str, size: f32) -> Svg {
    svg()
        .path(SharedString::from(format!("icons/{name}.svg")))
        .size(px(size))
        .text_color(rgb(c::FOREGROUND_MUTED))
}

pub fn provider_icon(provider: &str, size: f32) -> Svg {
    let name = match provider {
        "antigravity" | "claude" | "codex" | "copilot" | "kiro" | "minimax" | "omp"
        | "opencode" | "pi" => provider,
        _ => "square-terminal",
    };
    icon(name, size)
}

// For a control over a solid rail, flatten its CSS group opacity before drawing.
// GPUI's separate premultiplied-alpha rounding shifts the disabled fill by a channel.
pub fn control_color(foreground: u32, background: u32, opacity: f32) -> u32 {
    let alpha = (opacity * 255.).floor() as u32;
    [16, 8, 0].into_iter().fold(0, |color, shift| {
        let foreground = (foreground >> shift) & 255;
        let background = (background >> shift) & 255;
        color | (((foreground * alpha + background * (255 - alpha) + 127) / 255) << shift)
    })
}

pub fn loading_spinner(size: f32, color: u32) -> impl IntoElement {
    use crate::sync_presentation as p;
    icon("loading-spinner", size)
        .text_color(rgb(color))
        .with_animation(
            "timeline-spinner",
            Animation::new(std::time::Duration::from_millis(
                p::SPINNER_DURATION_MS as u64,
            ))
            .repeat(),
            |icon, phase| icon.with_transformation(Transformation::rotate(percentage(phase))),
        )
}

pub fn project_color(key: &str) -> u32 {
    let hash = key.chars().fold(0u32, |hash, ch| {
        let mut utf16 = [0; 2];
        hash.wrapping_mul(31)
            .wrapping_add(u32::from(ch.encode_utf16(&mut utf16)[0]))
    });
    crate::identity_colors::COLORS[hash as usize % crate::identity_colors::COLORS.len()]
}

pub fn agent_icon(provider: &str, running: bool) -> impl IntoElement {
    div()
        .relative()
        .size(px(26.))
        .flex()
        .items_center()
        .justify_center()
        .when(running, |frame| {
            frame
                .child(
                    div()
                        .absolute()
                        .left(px(1.))
                        .top(px(1.))
                        .size(px(24.))
                        .rounded_full()
                        .border(px(1.5))
                        .border_color(rgb(c::STATUS_DOT_RUNNING))
                        .opacity(0.3),
                )
                .child(
                    icon("status-ring-arc", 24.)
                        .absolute()
                        .left(px(1.))
                        .top(px(1.))
                        .text_color(rgb(c::STATUS_DOT_RUNNING))
                        .opacity(0.9)
                        .with_animation(
                            "running-ring",
                            Animation::new(std::time::Duration::from_millis(900)).repeat(),
                            |arc, _| {
                                let phase = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_millis()
                                    % 900;
                                arc.with_transformation(Transformation::rotate(percentage(
                                    phase as f32 / 900.,
                                )))
                            },
                        ),
                )
        })
        .child(provider_icon(provider, 16.))
}

pub struct Tooltip(pub String);
impl Render for Tooltip {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(rgb(c::SURFACE2))
            .text_color(rgb(c::FOREGROUND))
            .text_size(px(font_size::SM))
            .child(self.0.clone())
    }
}

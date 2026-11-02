//! Tool-call and reasoning rows. The badge, hover icon swap, expansion and detail content follow
//! the desktop message components; every value comes from the projected timeline item and the
//! shared message style, so the shell, code details and diff bands follow the active theme.
use crate::{
    code_view,
    line_diff::{self, Kind as LineKind},
    message_style::MessageStyle,
    settings_view::{SharedSettings, active_palette},
    theme::{font_size, radius, spacing},
    timeline::{self, ToolPresentation},
};
use gpui::{Context, MouseButton, Render, SharedString, Window, div, prelude::*, px, rgb};
use serde_json::Value;

pub struct ToolRow {
    item: Value,
    cwd: Option<String>,
    presentation: ToolPresentation,
    expanded: bool,
    last_in_sequence: bool,
    shared: SharedSettings,
}

impl ToolRow {
    pub fn new(
        item: &Value,
        cwd: Option<&str>,
        shared: SharedSettings,
        _cx: &mut Context<Self>,
    ) -> Self {
        Self {
            presentation: timeline::tool_presentation(item, cwd),
            item: item.clone(),
            cwd: cwd.map(str::to_owned),
            expanded: false,
            last_in_sequence: false,
            shared,
        }
    }
    pub fn set_item(&mut self, item: &Value, cx: &mut Context<Self>) {
        if self.item == *item {
            return;
        }
        self.presentation = timeline::tool_presentation(item, self.cwd.as_deref());
        self.item = item.clone();
        cx.notify();
    }
    pub fn set_last_in_sequence(&mut self, last: bool) {
        self.last_in_sequence = last;
    }
    fn detail(&self, style: &MessageStyle) -> gpui::AnyElement {
        detail_element(
            &self.item["detail"],
            self.presentation.error_text.as_deref(),
            style,
        )
    }
    fn header(&self, group: &SharedString, style: &MessageStyle) -> gpui::AnyElement {
        let active = self.expanded;
        let loading = self.presentation.loading;
        let icon = if self.presentation.error_text.is_some() {
            "triangle-alert"
        } else {
            self.presentation.icon
        };
        let icon_color = if self.presentation.error_text.is_some() {
            style.palette.destructive
        } else if active {
            style.palette.foreground
        } else {
            style.palette.muted_foreground
        };
        div()
            .w_full()
            .min_w_0()
            .flex()
            .items_center()
            .child(
                div()
                    .relative()
                    .size(px(22.))
                    .mr_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(active, |slot| {
                        slot.child(
                            crate::chrome::icon("chevron-down", 15.6)
                                .text_color(rgb(style.palette.foreground))
                                .ml(px(-4.)),
                        )
                    })
                    .when(!active, |slot| {
                        slot.child(
                            div()
                                .ml(px(-1.))
                                .child(crate::chrome::icon(icon, 12.).text_color(rgb(icon_color))),
                        )
                        .child(
                            div()
                                .absolute()
                                .ml(px(-4.))
                                .invisible()
                                .group_hover(group.clone(), |chevron| chevron.visible())
                                .child(
                                    crate::chrome::icon("chevron-right", 15.6)
                                        .text_color(rgb(style.palette.foreground))
                                        .ml(px(-4.)),
                                ),
                        )
                    }),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .text_color(rgb(if active {
                        style.palette.foreground
                    } else {
                        style.palette.foreground_muted
                    }))
                    .when(loading && !active, |label| label.opacity(0.72))
                    .child(self.presentation.display_name.clone()),
            )
            .when_some(self.presentation.summary.clone(), |row, summary| {
                row.child(
                    div()
                        .ml_2()
                        .min_w_0()
                        .truncate()
                        .text_color(rgb(if active {
                            style.palette.foreground
                        } else {
                            style.palette.foreground_muted
                        }))
                        .child(summary),
                )
            })
            .child(div().flex_1())
            .into_any_element()
    }
}

impl Render for ToolRow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let style = MessageStyle::resolve(&self.shared.get(), active_palette(&self.shared, window));
        let group: SharedString = format!("tool-{}", cx.entity_id()).into();
        let can_open = self.presentation.can_open;
        let expanded = self.expanded;
        div()
            .mx(px(-13.))
            .mb(px(if self.last_in_sequence {
                spacing::S_4
            } else {
                spacing::S_1
            }))
            .font_family(style.ui_family.clone())
            .child(
                div()
                    .id(("tool-badge", cx.entity_id()))
                    .w_full()
                    .min_w_0()
                    .rounded(px(radius::LG))
                    .border_1()
                    .border_color(rgb(if expanded {
                        style.palette.border
                    } else {
                        style.palette.surface0
                    }))
                    .px_2()
                    .py_1()
                    .when(expanded, |badge| {
                        badge
                            .bg(rgb(style.palette.surface1))
                            .rounded_bl(px(0.))
                            .rounded_br(px(0.))
                    })
                    .when(can_open, |badge| badge.cursor_pointer())
                    .group(group.clone())
                    .child(self.header(&group, &style))
                    .when(can_open, |badge| {
                        badge.on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.expanded = !this.expanded;
                                cx.notify();
                            }),
                        )
                    }),
            )
            .when(expanded, |row| {
                row.child(
                    div()
                        .w_full()
                        .min_w_0()
                        .border_1()
                        .border_t_0()
                        .border_color(rgb(style.palette.border))
                        .rounded_bl(px(radius::LG))
                        .rounded_br(px(radius::LG))
                        .overflow_hidden()
                        .child(self.detail(&style)),
                )
            })
    }
}

pub fn detail_element(
    detail: &Value,
    error: Option<&str>,
    style: &MessageStyle,
) -> gpui::AnyElement {
    let mut sections = Vec::new();
    match detail["type"].as_str().unwrap_or("unknown") {
        "shell" => sections.push(shell_section(detail, style)),
        "edit" => sections.push(edit_section(detail, style)),
        "write" => {
            if let Some(content) = detail["content"].as_str().filter(|value| !value.is_empty()) {
                sections.push(block(
                    "write",
                    code_view::lines(
                        content,
                        detail["filePath"]
                            .as_str()
                            .map_or("", code_view::language_for_path),
                        None,
                        style,
                    ),
                    style,
                ));
            }
        }
        "read" => {
            if let Some(content) = detail["content"].as_str().filter(|value| !value.is_empty()) {
                let start = detail["offset"]
                    .as_u64()
                    .and_then(|offset| u32::try_from(offset).ok());
                sections.push(block(
                    "read",
                    code_view::lines(
                        content,
                        detail["filePath"]
                            .as_str()
                            .map_or("", code_view::language_for_path),
                        start,
                        style,
                    ),
                    style,
                ));
            }
        }
        "search" => sections.extend(search_sections(detail, style)),
        "fetch" => sections.push(fetch_section(detail, style)),
        "plain_text" => {
            if let Some(text) = detail["text"].as_str().filter(|value| !value.is_empty()) {
                sections.push(
                    div()
                        .p_3()
                        .text_size(px(font_size::BASE))
                        .line_height(px(22.))
                        .child(prose(text))
                        .into_any_element(),
                );
            }
        }
        "worktree_setup" | "sub_agent" => {}
        _ => sections.extend(unknown_sections(detail, style)),
    }
    if let Some(error) = error {
        sections.push(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .p_3()
                .child(
                    div()
                        .text_size(px(font_size::SM))
                        .text_color(rgb(style.palette.foreground_muted))
                        .child("ERROR"),
                )
                .child(
                    div()
                        .text_color(rgb(style.palette.destructive))
                        .text_size(px(style.geometry.code_size))
                        .child(prose(error)),
                )
                .into_any_element(),
        );
    }
    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .children(sections)
        .into_any_element()
}

pub fn prose(text: &str) -> gpui::AnyElement {
    gpui::StyledText::new(SharedString::from(text.to_owned())).into_any_element()
}

fn block(id: &'static str, content: gpui::AnyElement, style: &MessageStyle) -> gpui::AnyElement {
    div()
        .id(id)
        .w_full()
        .min_w_0()
        .bg(rgb(style.palette.surface1))
        .p_3()
        .child(content)
        .into_any_element()
}

fn mono_text_block(text: &str, style: &MessageStyle) -> gpui::AnyElement {
    div()
        .p_3()
        .text_size(px(style.geometry.code_size))
        .line_height(px(style.geometry.code_line))
        .font_family(style.mono_family.clone())
        .child(prose(text))
        .into_any_element()
}

fn shell_section(detail: &Value, style: &MessageStyle) -> gpui::AnyElement {
    let command = detail["command"]
        .as_str()
        .unwrap_or("")
        .trim_end_matches('\n');
    let output = detail["output"]
        .as_str()
        .unwrap_or("")
        .trim_start_matches('\n');
    let text = if output.is_empty() {
        format!("$ {command}")
    } else {
        format!("$ {command}\n\n{output}")
    };
    block(
        "shell-detail",
        code_view::lines(&text, "", None, style),
        style,
    )
}

fn edit_section(detail: &Value, style: &MessageStyle) -> gpui::AnyElement {
    let lines = match detail["unifiedDiff"]
        .as_str()
        .filter(|diff| !diff.is_empty())
    {
        Some(diff) => diff
            .lines()
            .map(|line| {
                let kind = if line.starts_with('+') && !line.starts_with("+++") {
                    LineKind::Added
                } else if line.starts_with('-') && !line.starts_with("---") {
                    LineKind::Removed
                } else {
                    LineKind::Context
                };
                (kind, line.to_owned())
            })
            .collect::<Vec<_>>(),
        None => line_diff::diff(
            detail["oldString"].as_str().unwrap_or(""),
            detail["newString"].as_str().unwrap_or(""),
        ),
    };
    let mut column = div()
        .flex()
        .flex_col()
        .w_full()
        .text_size(px(style.geometry.code_size))
        .font_family(style.mono_family.clone());
    for (kind, line) in lines {
        let (color, background) = line_diff::colors(&style.palette, kind);
        column =
            column.child(
                div()
                    .min_h(px(style.geometry.code_line))
                    .when_some(background, |row, background| row.bg(gpui::rgba(background)))
                    .px_3()
                    .child(div().whitespace_nowrap().text_color(rgb(color)).child(
                        SharedString::from(if line.is_empty() {
                            "\u{200b}".into()
                        } else {
                            line
                        }),
                    )),
            );
    }
    block(
        "edit-detail",
        div()
            .id("edit-diff")
            .w_full()
            .overflow_x_scroll()
            .child(column)
            .into_any_element(),
        style,
    )
}

fn search_sections(detail: &Value, style: &MessageStyle) -> Vec<gpui::AnyElement> {
    let mut sections = Vec::new();
    let mut entries = Vec::new();
    if let Some(content) = detail["content"].as_str().filter(|value| !value.is_empty()) {
        entries.push(content.to_owned());
    }
    if let Some(paths) = detail["filePaths"]
        .as_array()
        .filter(|paths| !paths.is_empty())
    {
        entries.push(
            paths
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    if let Some(results) = detail["webResults"]
        .as_array()
        .filter(|results| !results.is_empty())
    {
        entries.push(
            results
                .iter()
                .map(|result| {
                    format!(
                        "{}\n{}",
                        result["title"].as_str().unwrap_or(""),
                        result["url"].as_str().unwrap_or("")
                    )
                })
                .collect::<Vec<_>>()
                .join("\n\n"),
        );
    }
    if let Some(annotations) = detail["annotations"]
        .as_array()
        .filter(|annotations| !annotations.is_empty())
    {
        entries.push(
            annotations
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n\n"),
        );
    }
    for (index, entry) in entries.iter().enumerate() {
        sections.push(
            div()
                .id(("search-section", index))
                .p_3()
                .child(mono_text_block(entry, style))
                .into_any_element(),
        );
    }
    sections
}

fn fetch_section(detail: &Value, style: &MessageStyle) -> gpui::AnyElement {
    let url = detail["url"].as_str().unwrap_or("");
    let result = detail["result"].as_str().unwrap_or("");
    let text = if result.is_empty() {
        url.to_owned()
    } else {
        format!("{url}\n\n{result}")
    };
    block(
        "fetch-detail",
        code_view::lines(&text, "", None, style),
        style,
    )
}

fn unknown_sections(detail: &Value, style: &MessageStyle) -> Vec<gpui::AnyElement> {
    if let Some(text) = detail["input"]
        .as_str()
        .filter(|_| detail["output"].is_null())
    {
        return vec![
            div()
                .p_3()
                .text_size(px(font_size::BASE))
                .line_height(px(22.))
                .child(prose(text))
                .into_any_element(),
        ];
    }
    let mut sections = Vec::new();
    for (title, value) in [("INPUT", &detail["input"]), ("OUTPUT", &detail["output"])] {
        if value.is_null() {
            continue;
        }
        let text = match value {
            Value::String(text) => text.clone(),
            value => serde_json::to_string_pretty(value).unwrap_or_default(),
        };
        if text.is_empty() {
            continue;
        }
        sections.push(
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .px_3()
                        .py_2()
                        .border_b_1()
                        .border_color(rgb(style.palette.border))
                        .text_size(px(font_size::BASE))
                        .text_color(rgb(style.palette.foreground_muted))
                        .child(title),
                )
                .child(block(
                    "unknown-detail",
                    code_view::lines(&text, "", None, style),
                    style,
                ))
                .into_any_element(),
        );
    }
    sections
}

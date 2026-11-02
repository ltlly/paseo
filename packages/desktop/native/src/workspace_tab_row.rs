use super::{EditorKey, PaneTabKind, Paseo};
use crate::{chrome, file_icon_view::FileIconCache};
use gpui::{
    Context, MouseButton, MouseDownEvent, ScrollHandle, SharedString, Window, div, prelude::*, px,
    rgb, rgba,
};
use paseo_desktop::{
    split_layout::{SplitLayout, SplitPane},
    workspace_tab_layout::{self, p},
};
use std::collections::{BTreeMap, BTreeSet};

struct LabelWidth {
    label: String,
    family: SharedString,
    size: f32,
    width: f32,
}
#[derive(Default)]
struct RailScroll {
    handle: ScrollHandle,
    active: Option<String>,
}
#[derive(Default)]
pub(super) struct RailCache {
    labels: BTreeMap<String, LabelWidth>,
    scrolls: BTreeMap<String, RailScroll>,
    icons: FileIconCache,
}
impl RailCache {
    pub(super) fn retain(&mut self, layout: &SplitLayout) {
        let panes = layout.panes();
        let tabs: BTreeSet<_> = panes.iter().flat_map(|pane| pane.tabs.iter()).collect();
        self.labels.retain(|tab, _| tabs.contains(tab));
        self.scrolls
            .retain(|id, _| panes.iter().any(|pane| &pane.id == id));
    }
    fn label_width(
        &mut self,
        id: &str,
        label: &str,
        family: SharedString,
        size: f32,
        window: &Window,
    ) -> f32 {
        if let Some(cached) = self.labels.get(id)
            && cached.label == label
            && cached.family == family
            && cached.size == size
        {
            return cached.width;
        }
        let run = gpui::TextRun {
            len: label.len(),
            font: gpui::font(family.clone()),
            color: rgb(0xffffff).into(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let width = window
            .text_system()
            .shape_line(label.to_owned().into(), px(size), &[run], None)
            .width
            .to_f64() as f32;
        self.labels.insert(
            id.to_owned(),
            LabelWidth {
                label: label.into(),
                family,
                size,
                width,
            },
        );
        width
    }
}

impl Paseo {
    pub(super) fn render_workspace_tab_row(
        &mut self,
        pane: &SplitPane,
        drop_target: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let palette = crate::settings_view::active_palette(&self.shared_settings, window);
        let settings = self.shared_settings.get();
        let font_size = paseo_desktop::appearance::scale_ui_font(
            crate::theme::font_size::BASE,
            settings.ui_base_font_size,
        );
        let family = crate::fonts::ui_family_for(&settings.ui_font_family);
        let line = crate::fonts::strut_for(window, font_size, &family);
        let pane_focused = self
            .selected_workspace
            .as_ref()
            .and_then(|id| self.splits.get(id))
            .is_some_and(|layout| layout.focused_pane_id.as_deref() == Some(&pane.id));
        let rows: Vec<_> = pane
            .tabs
            .iter()
            .filter_map(|id| {
                let kind = self.pane_tab_kind(id)?;
                let title = self.pane_tab_title(id, cx)?;
                let file = self
                    .editors
                    .iter()
                    .find(|(key, _)| key.tab_id() == *id)
                    .map(|(key, editor)| (key.clone(), editor.read(cx).is_dirty()));
                Some((id.clone(), title, kind, file))
            })
            .collect();
        let labels: Vec<_> = rows
            .iter()
            .map(|(id, title, _, file)| {
                self.tab_rail
                    .label_width(id, title, family.clone(), font_size, window)
                    + p::LABEL_ALLOWANCE
                    + if file.as_ref().is_some_and(|(_, dirty)| *dirty) {
                        p::CONTENT_GAP + p::MODIFIED_SIZE
                    } else {
                        0.
                    }
            })
            .collect();
        let width = self
            .pane_bounds
            .get(&pane.id)
            .map_or(window.viewport_size().width, |bounds| bounds.size.width)
            .to_f64() as f32;
        // The authored row reserves the 36px add-button slot before shrinking labels.
        let layout = workspace_tab_layout::layout(width, &labels, 36., false);
        let scroll = self.tab_rail.scrolls.entry(pane.id.clone()).or_default();
        if !layout.overflow {
            scroll.handle.set_offset(gpui::point(px(0.), px(0.)));
        } else if scroll.active != pane.focused_tab {
            if let Some(index) = rows
                .iter()
                .position(|(id, _, _, _)| Some(id) == pane.focused_tab.as_ref())
            {
                scroll.handle.scroll_to_item(index);
            }
        }
        scroll.active = pane.focused_tab.clone();
        let handle = scroll.handle.clone();
        let mut chips = vec![];
        for (index, (id, title, kind, file)) in rows.into_iter().enumerate() {
            let active = pane.focused_tab.as_ref() == Some(&id);
            let highlighted = active && pane_focused;
            let base = if highlighted {
                palette.surface2
            } else if active {
                palette.surface1
            } else {
                palette.surface0
            };
            let hover = if highlighted {
                palette.surface2
            } else {
                palette.surface1
            };
            let foreground = if highlighted {
                palette.foreground
            } else {
                palette.foreground_muted
            };
            let group = SharedString::from(format!("workspace-tab-{}-{id}", pane.id));
            let glyph = match kind {
                PaneTabKind::File => self.tab_rail.icons.render(&title, p::ICON_SIZE, window, cx),
                PaneTabKind::Agent => {
                    let agent = &self.directory.agents[&id];
                    chrome::provider_icon_data(
                        &agent.provider,
                        p::ICON_SIZE,
                        self.provider_images.get(&agent.provider),
                    )
                }
                _ => chrome::icon(
                    match kind {
                        PaneTabKind::Terminal => "terminal",
                        PaneTabKind::Launcher => "plus",
                        _ => "eye",
                    },
                    p::ICON_SIZE,
                )
                .text_color(rgb(foreground))
                .group_hover(group.clone(), |v| v.text_color(rgb(palette.foreground)))
                .into_any_element(),
            };
            let owner = pane.id.clone();
            let selected = id.clone();
            let tooltip = title.clone();
            let mut chip = div()
                .id(group.clone())
                .group(group.clone())
                .relative()
                .w(px(layout.widths[index]))
                .flex_shrink_0()
                .h(px(p::CHIP_HEIGHT))
                .px(px(p::CHIP_PADDING_X))
                .rounded(px(p::CHIP_RADIUS))
                .bg(rgb(base))
                .hover(|v| v.bg(rgb(hover)))
                .flex()
                .items_center()
                .gap(px(p::CONTENT_GAP))
                .cursor_pointer()
                .text_size(px(font_size))
                .line_height(line)
                .text_color(rgb(foreground))
                .tooltip(move |_, cx| cx.new(|_| chrome::Tooltip(tooltip.clone())).into())
                .child(div().size(px(p::ICON_SIZE)).flex_shrink_0().child(glyph))
                .when(layout.show_labels[index], |v| {
                    v.child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .group_hover(group.clone(), |v| v.text_color(rgb(palette.foreground)))
                            .child(title),
                    )
                })
                .when(file.as_ref().is_some_and(|(_, dirty)| *dirty), |v| {
                    v.child(
                        div()
                            .size(px(p::MODIFIED_SIZE))
                            .flex_shrink_0()
                            .rounded_full()
                            .bg(rgb(palette.foreground_muted)),
                    )
                })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        this.begin_tab_drag(
                            owner.clone(),
                            selected.clone(),
                            event.position,
                            window,
                            cx,
                        );
                    }),
                );
            // Closing a terminal stops its process only after system confirmation.
            if matches!(
                kind,
                PaneTabKind::Terminal
                    | PaneTabKind::Launcher
                    | PaneTabKind::File
                    | PaneTabKind::Browser
            ) {
                let key = file.as_ref().map(|(key, _)| key.clone());
                let close_id = id.clone();
                let middle_key = key.clone();
                let middle_id = close_id.clone();
                chip = chip.on_mouse_down(
                    MouseButton::Middle,
                    cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.close_passive_pane_tab(middle_key.as_ref(), &middle_id, window, cx);
                    }),
                );
                chip = chip.child(
                    div()
                        .absolute()
                        .right_0()
                        .top_0()
                        .bottom_0()
                        .w(px(p::SCRIM_WIDTH))
                        .rounded_r(px(p::CHIP_RADIUS))
                        .overflow_hidden()
                        .invisible()
                        .group_hover(group, |v| v.visible())
                        .bg(gpui::linear_gradient(
                            90.,
                            gpui::linear_color_stop(rgba(hover << 8), 0.),
                            gpui::linear_color_stop(rgb(hover), p::SCRIM_SOLID),
                        ))
                        .child(
                            div()
                                .id(SharedString::from(format!(
                                    "close-pane-tab-{}-{id}",
                                    pane.id
                                )))
                                .absolute()
                                .right(px(p::CLOSE_RIGHT))
                                .top(px((p::CHIP_HEIGHT - p::CLOSE_SIZE) / 2.))
                                .size(px(p::CLOSE_SIZE))
                                .rounded(px(p::CLOSE_RADIUS))
                                .flex()
                                .items_center()
                                .justify_center()
                                .hover(|v| v.bg(rgb(palette.surface3)))
                                .child(
                                    chrome::icon("x", 12.)
                                        .text_color(rgb(palette.foreground_muted)),
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.close_passive_pane_tab(
                                            key.as_ref(),
                                            &close_id,
                                            window,
                                            cx,
                                        );
                                    }),
                                ),
                        ),
                );
            }
            chips.push(chip.into_any_element());
        }
        let owner = pane.id.clone();
        let measure = cx.entity();
        let measured = owner.clone();
        let add = div()
            .w(px(36.))
            .h_full()
            .flex_shrink_0()
            .ml(px(-2.))
            .flex()
            .items_center()
            .p_1()
            .child(
                div()
                    .id(SharedString::from(format!("pane-add-{owner}")))
                    .relative()
                    .size(px(28.))
                    .rounded(px(8.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(|v| v.bg(rgb(palette.surface2)))
                    .child(chrome::icon("plus", 16.).text_color(rgb(palette.foreground_muted)))
                    .child(
                        gpui::canvas(
                            move |bounds, _, cx| {
                                measure.update(cx, |this, _| {
                                    this.terminal_commands
                                        .anchors
                                        .insert(measured.clone(), bounds);
                                });
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .size_full(),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.open_new_tab_menu(owner.clone(), window, cx);
                        }),
                    ),
            )
            .into_any_element();
        let mut fixed = None;
        if layout.overflow {
            fixed = Some(add);
        } else {
            chips.push(add);
        }
        div()
            .h(px(p::ROW_HEIGHT))
            .flex_shrink_0()
            .flex()
            .items_center()
            .border_b_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface0))
            .when(drop_target, |v| v.bg(rgb(palette.surface2)))
            .child(
                div()
                    .id(SharedString::from(format!("pane-tabs-{}", pane.id)))
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .px(px(p::ROW_PADDING_X + p::CHIP_GAP / 2.))
                    .flex()
                    .items_center()
                    .gap(px(p::CHIP_GAP))
                    .overflow_x_scroll()
                    .track_scroll(&handle)
                    .children(chips),
            )
            .when_some(fixed, |row, add| row.child(add))
            .into_any_element()
    }
    fn close_passive_pane_tab(
        &mut self,
        file: Option<&EditorKey>,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(key) = file {
            self.close_file(key, window, cx);
        } else if self.directory.terminals.contains_key(id) {
            self.confirm_close_terminal(id, window, cx);
        } else if id.starts_with(super::terminal_commands::PREFIX) {
            self.close_launcher_tab(id, window, cx);
        } else {
            self.forget_browser(id, window, cx);
        }
    }
}

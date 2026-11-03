//! Authored workspace-tab shrink/overflow policy, independent of native layout.
#[path = "workspace_tab_presentation.rs"]
pub mod p;

#[derive(Debug)]
pub struct Layout {
    pub widths: Vec<f32>,
    pub show_labels: Vec<bool>,
    pub overflow: bool,
}
pub fn layout(viewport: f32, labels: &[f32], reserved: f32, compact: bool) -> Layout {
    let chrome =
        f64::from(p::ICON_SIZE + p::CONTENT_GAP + p::CHIP_PADDING_X * 2. + p::CLOSE_RESERVED);
    let min = f64::from(p::MIN_WIDTH);
    let max = f64::from(p::MAX_WIDTH);
    let available = ((f64::from(viewport) - f64::from(reserved)).max(0.)
        - f64::from(p::ROW_PADDING_X * 2. + labels.len().saturating_sub(1) as f32 * p::CHIP_GAP))
    .max(0.);
    let natural: Vec<f64> = labels
        .iter()
        .map(|width| (chrome + f64::from(*width)).clamp(min, max))
        .collect();
    let total: f64 = natural.iter().sum();
    let minimum = min * labels.len() as f64;
    let overflow = !labels.is_empty()
        && if compact {
            total > available
        } else {
            available < minimum
        };
    let ratio = if !overflow && !compact && total > available {
        (total - available) / (total - minimum)
    } else {
        0.
    };
    let widths: Vec<f32> = natural
        .into_iter()
        .map(|width| (width - (width - min) * ratio).clamp(min, max).round() as f32)
        .collect();
    let show_labels = widths
        .iter()
        .map(|width| f64::from(*width) > chrome)
        .collect();
    Layout {
        widths,
        show_labels,
        overflow,
    }
}

/// Ports `layoutMetrics.actionsReservedWidth` from `workspace-desktop-tabs-row.tsx`:
/// the inline New Tab button slot is always reserved, and main panes add the
/// split-actions menu slot plus the maximize-action slot when multiple main
/// panes are visible.
pub fn actions_reserved_width(show_split_actions: bool, show_maximize_action: bool) -> f32 {
    p::ADD_INLINE_RESERVED
        + if show_split_actions {
            p::SPLIT_ACTIONS_RESERVED
        } else {
            0.
        }
        + if show_maximize_action {
            p::MAXIMIZE_ACTION_RESERVED
        } else {
            0.
        }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContextMenuTarget {
    Agent(String),
    Terminal(String),
    File(String),
    Passive,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContextMenuEntry {
    Item {
        id: &'static str,
        label: String,
        icon: &'static str,
        hint: Option<String>,
        tooltip: Option<String>,
        enabled: bool,
    },
    Separator,
}

/// Ports `buildWorkspaceTabMenuEntries({ surface: "desktop", ... })` from
/// `packages/app/src/screens/workspace/workspace-tab-menu.ts`.
pub fn build_context_menu_entries(
    locale: crate::locales::Locale,
    target: &ContextMenuTarget,
    index: usize,
    tab_count: usize,
) -> Vec<ContextMenuEntry> {
    use crate::locales::{Tx, translate};
    let text = |key| translate(locale, Tx::key(key));
    let is_first = index == 0;
    let is_last = index.saturating_add(1) >= tab_count;
    let is_only = tab_count <= 1;
    let mut entries = Vec::new();
    match target {
        ContextMenuTarget::Agent(agent_id) => {
            entries.push(ContextMenuEntry::Item {
                id: "copy-resume-command",
                label: text("workspace.tabs.menu.copyResumeCommand"),
                icon: "copy",
                hint: None,
                tooltip: None,
                enabled: true,
            });
            entries.push(ContextMenuEntry::Item {
                id: "copy-agent-id",
                label: text("workspace.tabs.menu.copyAgentId"),
                icon: "copy",
                hint: Some(agent_id.chars().take(7).collect()),
                tooltip: None,
                enabled: true,
            });
        }
        ContextMenuTarget::Terminal(terminal_id) => {
            entries.push(ContextMenuEntry::Item {
                id: "copy-terminal-id",
                label: text("workspace.tabs.menu.copyTerminalId"),
                icon: "copy",
                hint: Some(terminal_id.chars().take(7).collect()),
                tooltip: None,
                enabled: true,
            });
        }
        ContextMenuTarget::File(_) => {
            entries.push(ContextMenuEntry::Item {
                id: "copy-file-path",
                label: text("workspace.tabs.menu.copyFilePath"),
                icon: "copy",
                hint: None,
                tooltip: None,
                enabled: true,
            });
        }
        ContextMenuTarget::Passive => {}
    }
    if matches!(
        target,
        ContextMenuTarget::Agent(_) | ContextMenuTarget::Terminal(_)
    ) {
        entries.push(ContextMenuEntry::Item {
            id: "rename",
            label: text("workspace.tabs.menu.rename"),
            icon: "pencil",
            hint: None,
            tooltip: None,
            enabled: true,
        });
        entries.push(ContextMenuEntry::Separator);
    }
    entries.push(ContextMenuEntry::Item {
        id: "close-before",
        label: text("workspace.tabs.menu.closeLeft"),
        icon: "arrow-left-to-line",
        hint: None,
        tooltip: None,
        enabled: !is_first,
    });
    entries.push(ContextMenuEntry::Item {
        id: "close-after",
        label: text("workspace.tabs.menu.closeRight"),
        icon: "arrow-right-to-line",
        hint: None,
        tooltip: None,
        enabled: !is_last,
    });
    entries.push(ContextMenuEntry::Item {
        id: "close-others",
        label: text("workspace.tabs.menu.closeOthers"),
        icon: "copy-x",
        hint: None,
        tooltip: None,
        enabled: !is_only,
    });
    if matches!(target, ContextMenuTarget::Agent(_)) {
        entries.push(ContextMenuEntry::Item {
            id: "reload-agent",
            label: text("workspace.tabs.menu.reloadAgent"),
            icon: "rotate-cw",
            hint: None,
            tooltip: Some(text("workspace.tabs.menu.reloadAgentTooltip")),
            enabled: true,
        });
    }
    entries.push(ContextMenuEntry::Item {
        id: "close",
        label: text("workspace.tabs.menu.close"),
        icon: "x",
        hint: None,
        tooltip: None,
        enabled: true,
    });
    entries
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BulkCloseGroups {
    pub archive_agent_tabs: Vec<(String, String)>,
    pub layout_only_agent_tabs: Vec<(String, String)>,
    pub terminal_tabs: Vec<(String, String)>,
    pub other_tabs: Vec<String>,
}

impl BulkCloseGroups {
    pub fn passive_tab_count(&self) -> usize {
        self.layout_only_agent_tabs.len() + self.other_tabs.len()
    }
    pub fn has_destructive_tabs(&self) -> bool {
        !self.archive_agent_tabs.is_empty() || !self.terminal_tabs.is_empty()
    }
}

/// Ports `buildBulkCloseConfirmationMessage` and `handleBulkCloseTabs` message
/// assembly from `packages/app/src/screens/workspace/workspace-bulk-close.ts`.
pub fn build_bulk_close_confirmation_message(
    locale: crate::locales::Locale,
    groups: &BulkCloseGroups,
    modified_count: usize,
) -> String {
    use crate::locales::{Tx, translate};
    let agents = groups.archive_agent_tabs.len().to_string();
    let terminals = groups.terminal_tabs.len().to_string();
    let tabs = groups.passive_tab_count().to_string();
    let has_agents = !groups.archive_agent_tabs.is_empty();
    let has_terminals = !groups.terminal_tabs.is_empty();
    let has_tabs = groups.passive_tab_count() > 0;
    let base = if has_agents && has_terminals && has_tabs {
        translate(
            locale,
            Tx::key("workspace.tabs.confirmations.bulk.all").args(&[
                ("agents", &agents),
                ("terminals", &terminals),
                ("tabs", &tabs),
            ]),
        )
    } else if has_agents && has_terminals {
        translate(
            locale,
            Tx::key("workspace.tabs.confirmations.bulk.agentsAndTerminals")
                .args(&[("agents", &agents), ("terminals", &terminals)]),
        )
    } else if has_terminals && has_tabs {
        translate(
            locale,
            Tx::key("workspace.tabs.confirmations.bulk.terminalsAndTabs")
                .args(&[("terminals", &terminals), ("tabs", &tabs)]),
        )
    } else if has_agents && has_tabs {
        translate(
            locale,
            Tx::key("workspace.tabs.confirmations.bulk.agentsAndTabs")
                .args(&[("agents", &agents), ("tabs", &tabs)]),
        )
    } else if has_terminals {
        translate(
            locale,
            Tx::key("workspace.tabs.confirmations.bulk.terminals")
                .args(&[("terminals", &terminals)]),
        )
    } else if has_tabs {
        translate(
            locale,
            Tx::key("workspace.tabs.confirmations.bulk.tabs").args(&[("tabs", &tabs)]),
        )
    } else {
        translate(
            locale,
            Tx::key("workspace.tabs.confirmations.bulk.agents").args(&[("agents", &agents)]),
        )
    };
    if modified_count > 0 {
        let unsaved = translate(
            locale,
            Tx::key("workspace.tabs.confirmations.bulkUnsaved").count(modified_count as i64),
        );
        format!("{base}\n\n{unsaved}")
    } else {
        base
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn matches_authored_widths_and_overflow_decisions() {
        let oracle: serde_json::Value =
            serde_json::from_str(include_str!("testdata/workspace-tabs.json")).unwrap();
        for case in oracle["cases"].as_array().unwrap() {
            let labels: Vec<_> = case["labels"]
                .as_array()
                .unwrap()
                .iter()
                .map(|n| n.as_f64().unwrap() as f32)
                .collect();
            let compact = case["compact"].as_bool().unwrap();
            let result = super::layout(
                case["viewport"].as_f64().unwrap() as f32,
                &labels,
                case["reserved"].as_f64().unwrap() as f32,
                compact,
            );
            let actual = serde_json::json!({"items":result.widths.iter().zip(result.show_labels).map(|(width,show)|serde_json::json!({"width":*width as u32,"showLabel":show})).collect::<Vec<_>>(),"closeButtonPolicy":if compact{"none"}else{"all"},"requiresHorizontalScrollFallback":result.overflow});
            assert_eq!(actual, case["expected"], "{case}");
        }
    }

    #[test]
    fn actions_reserved_width_matches_authored_main_and_explorer_rows() {
        assert_eq!(super::actions_reserved_width(false, false), 36.0);
        assert_eq!(super::actions_reserved_width(true, false), 63.0);
        assert_eq!(super::actions_reserved_width(true, true), 84.0);
    }

    #[test]
    fn context_menu_entries_match_authored_desktop_menus_and_generated_locales() {
        use super::{ContextMenuEntry, ContextMenuTarget, build_context_menu_entries};
        use crate::locales::{Locale, Tx, translate};

        assert_eq!(
            build_context_menu_entries(
                Locale::En,
                &ContextMenuTarget::Agent("agent-123456789".into()),
                1,
                3,
            ),
            vec![
                ContextMenuEntry::Item {
                    id: "copy-resume-command",
                    label: "Copy resume command".into(),
                    icon: "copy",
                    hint: None,
                    tooltip: None,
                    enabled: true,
                },
                ContextMenuEntry::Item {
                    id: "copy-agent-id",
                    label: "Copy agent id".into(),
                    icon: "copy",
                    hint: Some("agent-1".into()),
                    tooltip: None,
                    enabled: true,
                },
                ContextMenuEntry::Item {
                    id: "rename",
                    label: "Rename".into(),
                    icon: "pencil",
                    hint: None,
                    tooltip: None,
                    enabled: true,
                },
                ContextMenuEntry::Separator,
                ContextMenuEntry::Item {
                    id: "close-before",
                    label: "Close to the left".into(),
                    icon: "arrow-left-to-line",
                    hint: None,
                    tooltip: None,
                    enabled: true,
                },
                ContextMenuEntry::Item {
                    id: "close-after",
                    label: "Close to the right".into(),
                    icon: "arrow-right-to-line",
                    hint: None,
                    tooltip: None,
                    enabled: true,
                },
                ContextMenuEntry::Item {
                    id: "close-others",
                    label: "Close other tabs".into(),
                    icon: "copy-x",
                    hint: None,
                    tooltip: None,
                    enabled: true,
                },
                ContextMenuEntry::Item {
                    id: "reload-agent",
                    label: "Reload agent".into(),
                    icon: "rotate-cw",
                    hint: None,
                    tooltip: Some("Reload agent to update skills, MCPs or login status.".into()),
                    enabled: true,
                },
                ContextMenuEntry::Item {
                    id: "close",
                    label: "Close".into(),
                    icon: "x",
                    hint: None,
                    tooltip: None,
                    enabled: true,
                },
            ]
        );

        assert_eq!(
            build_context_menu_entries(
                Locale::En,
                &ContextMenuTarget::Terminal("term".into()),
                0,
                1,
            ),
            vec![
                ContextMenuEntry::Item {
                    id: "copy-terminal-id",
                    label: "Copy terminal id".into(),
                    icon: "copy",
                    hint: Some("term".into()),
                    tooltip: None,
                    enabled: true,
                },
                ContextMenuEntry::Item {
                    id: "rename",
                    label: "Rename".into(),
                    icon: "pencil",
                    hint: None,
                    tooltip: None,
                    enabled: true,
                },
                ContextMenuEntry::Separator,
                ContextMenuEntry::Item {
                    id: "close-before",
                    label: "Close to the left".into(),
                    icon: "arrow-left-to-line",
                    hint: None,
                    tooltip: None,
                    enabled: false,
                },
                ContextMenuEntry::Item {
                    id: "close-after",
                    label: "Close to the right".into(),
                    icon: "arrow-right-to-line",
                    hint: None,
                    tooltip: None,
                    enabled: false,
                },
                ContextMenuEntry::Item {
                    id: "close-others",
                    label: "Close other tabs".into(),
                    icon: "copy-x",
                    hint: None,
                    tooltip: None,
                    enabled: false,
                },
                ContextMenuEntry::Item {
                    id: "close",
                    label: "Close".into(),
                    icon: "x",
                    hint: None,
                    tooltip: None,
                    enabled: true,
                },
            ]
        );

        assert_eq!(
            build_context_menu_entries(
                Locale::En,
                &ContextMenuTarget::File("/repo/README.md".into()),
                2,
                3,
            ),
            vec![
                ContextMenuEntry::Item {
                    id: "copy-file-path",
                    label: "Copy file path".into(),
                    icon: "copy",
                    hint: None,
                    tooltip: None,
                    enabled: true,
                },
                ContextMenuEntry::Item {
                    id: "close-before",
                    label: "Close to the left".into(),
                    icon: "arrow-left-to-line",
                    hint: None,
                    tooltip: None,
                    enabled: true,
                },
                ContextMenuEntry::Item {
                    id: "close-after",
                    label: "Close to the right".into(),
                    icon: "arrow-right-to-line",
                    hint: None,
                    tooltip: None,
                    enabled: false,
                },
                ContextMenuEntry::Item {
                    id: "close-others",
                    label: "Close other tabs".into(),
                    icon: "copy-x",
                    hint: None,
                    tooltip: None,
                    enabled: true,
                },
                ContextMenuEntry::Item {
                    id: "close",
                    label: "Close".into(),
                    icon: "x",
                    hint: None,
                    tooltip: None,
                    enabled: true,
                },
            ]
        );

        assert_eq!(
            build_context_menu_entries(Locale::En, &ContextMenuTarget::Passive, 0, 2),
            vec![
                ContextMenuEntry::Item {
                    id: "close-before",
                    label: "Close to the left".into(),
                    icon: "arrow-left-to-line",
                    hint: None,
                    tooltip: None,
                    enabled: false,
                },
                ContextMenuEntry::Item {
                    id: "close-after",
                    label: "Close to the right".into(),
                    icon: "arrow-right-to-line",
                    hint: None,
                    tooltip: None,
                    enabled: true,
                },
                ContextMenuEntry::Item {
                    id: "close-others",
                    label: "Close other tabs".into(),
                    icon: "copy-x",
                    hint: None,
                    tooltip: None,
                    enabled: true,
                },
                ContextMenuEntry::Item {
                    id: "close",
                    label: "Close".into(),
                    icon: "x",
                    hint: None,
                    tooltip: None,
                    enabled: true,
                },
            ]
        );

        for locale in Locale::ALL {
            let entries = build_context_menu_entries(
                locale,
                &ContextMenuTarget::Agent("abcdef12345".into()),
                1,
                3,
            );
            let labels: Vec<_> = entries
                .iter()
                .filter_map(|entry| match entry {
                    ContextMenuEntry::Item { label, .. } => Some(label.clone()),
                    ContextMenuEntry::Separator => None,
                })
                .collect();
            let expected: Vec<_> = [
                "workspace.tabs.menu.copyResumeCommand",
                "workspace.tabs.menu.copyAgentId",
                "workspace.tabs.menu.rename",
                "workspace.tabs.menu.closeLeft",
                "workspace.tabs.menu.closeRight",
                "workspace.tabs.menu.closeOthers",
                "workspace.tabs.menu.reloadAgent",
                "workspace.tabs.menu.close",
            ]
            .into_iter()
            .map(|key| translate(locale, Tx::key(key)))
            .collect();
            assert_eq!(labels, expected, "{locale:?}");
        }
    }

    #[test]
    fn bulk_close_groups_and_confirmation_messages_match_authored_copy_and_locales() {
        use super::{BulkCloseGroups, build_bulk_close_confirmation_message};
        use crate::locales::{Locale, Tx, translate};

        let mut groups = BulkCloseGroups {
            archive_agent_tabs: vec![
                ("tab-a1".into(), "a1".into()),
                ("tab-a2".into(), "a2".into()),
            ],
            layout_only_agent_tabs: vec![("tab-sub".into(), "sub".into())],
            terminal_tabs: vec![("tab-t1".into(), "t1".into())],
            other_tabs: vec!["tab-file".into()],
        };
        assert_eq!(groups.passive_tab_count(), 2);
        assert!(groups.has_destructive_tabs());
        assert_eq!(
            build_bulk_close_confirmation_message(Locale::En, &groups, 0),
            "This will archive 2 agent(s), close 1 terminal(s), and close 2 tab(s). Any running process in a closed terminal will be stopped immediately."
        );
        assert_eq!(
            build_bulk_close_confirmation_message(Locale::En, &groups, 3),
            "This will archive 2 agent(s), close 1 terminal(s), and close 2 tab(s). Any running process in a closed terminal will be stopped immediately.\n\n3 tab(s) have unsaved changes. Closing will discard those drafts."
        );

        groups.layout_only_agent_tabs.clear();
        groups.other_tabs.clear();
        assert_eq!(groups.passive_tab_count(), 0);
        assert!(groups.has_destructive_tabs());
        assert_eq!(
            build_bulk_close_confirmation_message(Locale::En, &groups, 0),
            "This will archive 2 agent(s) and close 1 terminal(s). Any running process in a closed terminal will be stopped immediately."
        );

        groups.archive_agent_tabs.clear();
        groups.other_tabs = vec!["tab-file".into()];
        assert!(groups.has_destructive_tabs());
        assert_eq!(
            build_bulk_close_confirmation_message(Locale::En, &groups, 0),
            "This will close 1 terminal(s) and close 1 tab(s). Any running process in a closed terminal will be stopped immediately."
        );

        groups.terminal_tabs.clear();
        groups.archive_agent_tabs = vec![("tab-a1".into(), "a1".into())];
        assert!(groups.has_destructive_tabs());
        assert_eq!(
            build_bulk_close_confirmation_message(Locale::En, &groups, 0),
            "This will archive 1 agent(s) and close 1 tab(s)."
        );

        groups.archive_agent_tabs.clear();
        groups.terminal_tabs = vec![("tab-t1".into(), "t1".into())];
        groups.other_tabs.clear();
        assert!(groups.has_destructive_tabs());
        assert_eq!(
            build_bulk_close_confirmation_message(Locale::En, &groups, 0),
            "This will close 1 terminal(s). Any running process in a closed terminal will be stopped immediately."
        );

        groups.terminal_tabs.clear();
        groups.layout_only_agent_tabs = vec![("tab-sub".into(), "sub".into())];
        assert!(!groups.has_destructive_tabs());
        assert_eq!(
            build_bulk_close_confirmation_message(Locale::En, &groups, 0),
            "This will close 1 tab(s)."
        );

        groups.layout_only_agent_tabs.clear();
        groups.archive_agent_tabs = vec![("tab-a1".into(), "a1".into())];
        assert!(groups.has_destructive_tabs());
        assert_eq!(
            build_bulk_close_confirmation_message(Locale::En, &groups, 0),
            "This will archive 1 agent(s)."
        );

        let all_groups = BulkCloseGroups {
            archive_agent_tabs: vec![("tab-a1".into(), "a1".into())],
            layout_only_agent_tabs: vec![("tab-sub".into(), "sub".into())],
            terminal_tabs: vec![("tab-t1".into(), "t1".into())],
            other_tabs: vec!["tab-file".into()],
        };
        for locale in Locale::ALL {
            let expected_base = translate(
                locale,
                Tx::key("workspace.tabs.confirmations.bulk.all").args(&[
                    ("agents", "1"),
                    ("terminals", "1"),
                    ("tabs", "2"),
                ]),
            );
            let expected_unsaved = translate(
                locale,
                Tx::key("workspace.tabs.confirmations.bulkUnsaved").count(2),
            );
            assert_eq!(
                build_bulk_close_confirmation_message(locale, &all_groups, 2),
                format!("{expected_base}\n\n{expected_unsaved}"),
                "{locale:?}"
            );
        }
    }
}

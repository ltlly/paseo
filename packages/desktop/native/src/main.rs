mod agent_history;
mod assets;
mod browser_view;
mod browser_webview;
mod choice_menu;
mod chrome;
mod code_view;
mod composer_view;
mod conversation;
mod file_editor;
mod fonts;
mod hosts_surface;
mod identity_colors;
mod image_view;
mod inline_input;
mod line_diff;
#[allow(dead_code)]
mod message_geometry;
mod message_style;
mod message_view;
mod pane_actions;
mod permission_board;
mod plan_row;
mod plugin_logs_view;
mod plugins_view;
mod question_form;
mod rich_text;
mod schedule_form_view;
mod schedules_view;
mod settings_view;
mod sync_presentation;
mod syntax;
mod terminal_view;
mod text_field;
mod theme;
mod thought_row;
mod timeline;
mod todo_row;
mod tool_row;
mod turn_footer;
mod ui;
mod window_chrome;
mod window_geometry;
mod workspace_form;
mod workspace_tools;

use gpui::{App, KeyBinding};
use paseo_desktop::activation::{self, ActivationTarget};

fn main() {
    // A cold launch with `paseo://h/<serverId>/agent/<agentId>` in the
    // arguments starts on that agent, like the desktop app's protocol launch.
    let pending_link: Option<ActivationTarget> = activation::parse_agent_deep_link_from_args(
        &std::env::args().collect::<Vec<_>>(),
    )
    .map(|link| ActivationTarget::Agent {
        server_id: link.server_id,
        agent_id: link.agent_id,
    });
    let (open_urls, open_urls_receiver) = async_channel::unbounded::<Vec<String>>();
    let app = gpui_platform::application().with_assets(assets::Assets);
    app.on_open_urls(move |urls| {
        // macOS warm activation. The pinned GPUI backends only deliver
        // open-URL callbacks on macOS; Linux and Windows store the
        // callback but never fire it, so their warm links need an OS
        // single-instance forwarder that GPUI does not provide yet.
        let _ = open_urls.send_blocking(urls);
    });
    app.on_reopen(|cx: &mut App| {
        // Dock click / activate: only a window-less app opens a window.
        ui::restore_when_activated(cx);
    });
    app.run(move |cx: &mut App| {
        fonts::configure(cx);
        // Pane actions mirror the original app's macOS bindings, with Ctrl
        // taking Cmd's place elsewhere. On non-mac the handlers yield these
        // keystrokes to a focused terminal program instead.
        let pane_modifier = if cfg!(target_os = "macos") {
            "cmd"
        } else {
            "ctrl"
        };
        cx.bind_keys([
            KeyBinding::new(&format!("{pane_modifier}-\\"), ui::SplitPaneRight, None),
            KeyBinding::new(
                &format!("{pane_modifier}-shift-\\"),
                ui::SplitPaneDown,
                None,
            ),
            KeyBinding::new(
                &format!("{pane_modifier}-shift-left"),
                ui::FocusPaneLeft,
                None,
            ),
            KeyBinding::new(
                &format!("{pane_modifier}-shift-right"),
                ui::FocusPaneRight,
                None,
            ),
            KeyBinding::new(&format!("{pane_modifier}-shift-up"), ui::FocusPaneUp, None),
            KeyBinding::new(
                &format!("{pane_modifier}-shift-down"),
                ui::FocusPaneDown,
                None,
            ),
            KeyBinding::new(
                &format!("{pane_modifier}-alt-shift-left"),
                ui::MoveTabLeft,
                None,
            ),
            KeyBinding::new(
                &format!("{pane_modifier}-alt-shift-right"),
                ui::MoveTabRight,
                None,
            ),
            KeyBinding::new(
                &format!("{pane_modifier}-alt-shift-up"),
                ui::MoveTabUp,
                None,
            ),
            KeyBinding::new(
                &format!("{pane_modifier}-alt-shift-down"),
                ui::MoveTabDown,
                None,
            ),
            KeyBinding::new(
                &format!("{pane_modifier}-shift-w"),
                ui::CloseFocusedPane,
                None,
            ),
            KeyBinding::new(&format!("{pane_modifier}-n"), ui::NewWindow, None),
        ]);
        cx.text_system()
            .add_fonts(fonts::terminal_faces())
            .expect("load bundled terminal fonts");
        // Process identity for the OS. The Windows backend registers
        // `HKCU\Software\Classes\AppUserModelId\<id>` (DisplayName) on the
        // first notification, so the native client uses its own id: the
        // packaged desktop app keeps `sh.paseo.desktop`, and the two builds
        // never share a notification identity or its registry entry.
        cx.set_app_identity("sh.paseo.desktop.native", "Paseo Native");
        // OS notification clicks arrive on the main thread on every GPUI
        // backend; the tag carries the activation target and the window that
        // posted it, so the click returns to that window's host.
        cx.on_system_notification_response(move |response, cx| {
            if let Some(tag) = activation::parse_notification_tag(&response.tag) {
                ui::route_activation(cx, tag.target, tag.sender);
            }
        });
        cx.spawn(async move |cx| {
            while let Ok(urls) = open_urls_receiver.recv().await {
                for url in urls {
                    if let Some(link) = activation::parse_agent_deep_link(&url) {
                        let target = ActivationTarget::Agent {
                            server_id: link.server_id,
                            agent_id: link.agent_id,
                        };
                        let _ = cx.update(|cx| ui::route_activation(cx, target, None));
                    }
                }
            }
        })
        .detach();
        cx.on_window_closed(|cx, window_id| {
            cx.global_mut::<ui::WindowCoordinator>()
                .windows
                .retain(|handle| handle.window_id() != window_id);
            if !cfg!(target_os = "macos") && cx.windows().is_empty() {
                // macOS keeps the app alive for a dock reopen, like the
                // desktop app.
                cx.quit();
            }
        })
        .detach();
        let _ = ui::open_paseo_window(cx, pending_link);
        cx.activate(true);
    });
}

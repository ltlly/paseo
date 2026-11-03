pub mod activation;
pub mod agent_controls;
pub mod appearance;
pub mod attachment_geometry;
pub mod attachments;
pub mod browser_model;
pub mod client;
pub mod command_center_match;
pub mod command_center_model;
pub mod composer;
pub mod editing;
pub mod explorer_layout;
pub mod file_icons;
pub mod file_mount_cache;
pub mod font_selection;
pub mod history;
pub mod hosts;
pub mod inline_layout;
pub mod keyboard;
pub mod locales;
mod locales_data;
pub mod markdown;
pub mod menu_position;
pub mod message_document;
pub mod model;
pub mod mouse;
pub mod panel_host;
pub mod relay;
pub mod settings;
pub mod split_layout;
mod subscriptions;
pub mod terminal;
pub mod terminal_copy;
pub mod terminal_handler;
mod theme;
pub mod wire;

#[path = "terminal_view/decorations.rs"]
pub mod terminal_decorations;
#[path = "terminal_view/find.rs"]
pub mod terminal_find;
#[path = "terminal_view/geometry.rs"]
pub mod terminal_geometry;
#[path = "terminal_view/links.rs"]
pub mod terminal_links;
pub mod terminal_retention;

#[path = "terminal_view/scrollbar.rs"]
pub mod terminal_scrollbar;
pub mod workspace_layout_storage;
pub mod workspace_lifecycle;

pub mod workspace_tab_layout;

pub mod terminal_lifecycle;

pub mod terminal_profile_settings;

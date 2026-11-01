pub mod agent_controls;
pub mod attachment_geometry;
pub mod attachments;
pub mod client;
pub mod composer;
pub mod editing;
pub mod font_selection;
pub mod history;
pub mod inline_layout;
pub mod keyboard;
pub mod markdown;
pub mod message_document;
pub mod model;
pub mod mouse;
mod subscriptions;
pub mod terminal;
mod theme;
pub mod wire;

#[path = "terminal_view/geometry.rs"]
pub mod terminal_geometry;
pub mod terminal_retention;

pub mod workspace_lifecycle;

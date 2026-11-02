//! Explorer shell intent is stored with its workspace; viewport constraints
//! affect presentation without overwriting the saved width or navigation intent.
use serde::{Deserialize, Serialize};

#[path = "explorer_geometry.rs"]
mod geometry;
pub use geometry::*;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ExplorerState {
    pub visible: bool,
    pub width: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pane: Option<crate::split_layout::SplitPane>,
}
impl Default for ExplorerState {
    fn default() -> Self {
        Self {
            visible: false,
            width: DEFAULT_WIDTH,
            pane: None,
        }
    }
}
impl ExplorerState {
    pub fn ensure_pane(&mut self) -> &mut crate::split_layout::SplitPane {
        self.pane
            .get_or_insert_with(|| crate::split_layout::SplitPane {
                id: EXPLORER_PANE_ID.into(),
                tabs: vec![FILES_TAB_ID.into(), CHANGES_TAB_ID.into()],
                focused_tab: Some(FILES_TAB_ID.into()),
            })
    }
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
    pub fn normalized(mut self) -> Self {
        self.width = if self.width.is_finite() {
            self.width.max(MIN_WIDTH)
        } else {
            DEFAULT_WIDTH
        };
        self
    }
}
pub const EXPLORER_PANE_ID: &str = "explorer";
pub const FILES_TAB_ID: &str = "files";
pub const CHANGES_TAB_ID: &str = "changes_tree";

pub fn resolve_width(requested: f64, container: f64) -> f64 {
    let maximum = if container > 0. {
        (container - MIN_BODY_WIDTH).max(MIN_WIDTH)
    } else {
        requested
    };
    requested.min(maximum).max(MIN_WIDTH)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    pub navigation_visible: bool,
    pub overlay: bool,
    pub container_width: f64,
    pub width: f64,
}
pub fn geometry(
    viewport: f64,
    navigation_requested: bool,
    navigation_width: f64,
    explorer_visible: bool,
    requested_width: f64,
) -> Geometry {
    let navigation_visible = navigation_requested
        && (!explorer_visible || viewport >= navigation_width + MIN_WIDTH + MIN_BODY_WIDTH);
    let container_width = (viewport
        - if navigation_visible {
            navigation_width
        } else {
            0.
        })
    .max(0.);
    Geometry {
        navigation_visible,
        overlay: container_width < MIN_WIDTH + MIN_BODY_WIDTH,
        container_width,
        width: resolve_width(requested_width, container_width),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn widths_match_the_authored_explorer_resolver() {
        let oracle: serde_json::Value =
            serde_json::from_str(include_str!("testdata/explorer-geometry.json")).unwrap();
        for case in oracle["cases"].as_array().unwrap() {
            let width = case[0].as_f64().unwrap_or(DEFAULT_WIDTH);
            assert_eq!(
                resolve_width(width, case[1].as_f64().unwrap()),
                case[2].as_f64().unwrap(),
                "{case}"
            );
        }
    }
    #[test]
    fn narrow_docks_yield_navigation_without_changing_intent() {
        let state = ExplorerState {
            visible: true,
            width: 500.,
            ..Default::default()
        };
        let narrow = geometry(800., true, 320., state.visible, state.width);
        assert!(!narrow.navigation_visible);
        assert_eq!(narrow.width, 440.);
        assert!(!narrow.overlay);
        let wide = geometry(1280., true, 320., state.visible, state.width);
        assert!(wide.navigation_visible);
        assert_eq!(wide.width, 500.);
        assert!(geometry(500., true, 320., true, 320.).overlay);
        assert_eq!(state.width, 500.);
    }
}

//! Panel capabilities belong to the authored manifest, independently of the
//! main workspace tree or the Explorer shell that happens to render a tab.
#[path = "panel_manifest.rs"]
mod data;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Host {
    Main,
    Explorer,
}
impl Host {
    pub fn for_pane(id: &str) -> Self {
        if id == crate::explorer_layout::EXPLORER_PANE_ID {
            Self::Explorer
        } else {
            Self::Main
        }
    }
}
pub struct Manifest {
    pub kind: &'static str,
    pub main: bool,
    pub explorer: bool,
    pub show_close: bool,
    pub singleton: bool,
}
pub fn manifest(kind: &str) -> Option<&'static Manifest> {
    data::MANIFESTS.iter().find(|panel| panel.kind == kind)
}
pub fn supports(kind: &str, host: Host) -> bool {
    manifest(kind).is_some_and(|panel| match host {
        Host::Main => panel.main,
        Host::Explorer => panel.explorer,
    })
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchSurface {
    Menu,
    Panel,
}

pub fn can_launch(kind: &str, host: Host, existing: &[&str]) -> bool {
    can_launch_on_surface(kind, host, existing, LaunchSurface::Menu)
}

pub fn can_launch_on_surface(
    kind: &str,
    host: Host,
    existing: &[&str],
    surface: LaunchSurface,
) -> bool {
    supports(kind, host)
        && match surface {
            LaunchSurface::Panel => manifest(kind).is_some(),
            LaunchSurface::Menu => {
                manifest(kind).is_some_and(|panel| !panel.singleton || !existing.contains(&kind))
            }
        }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn launcher_and_move_admission_match_the_authored_panel_manifest() {
        let cases: serde_json::Value =
            serde_json::from_str(include_str!("testdata/panel-host.json")).unwrap();
        for case in cases.as_array().unwrap() {
            let kind = case["kind"].as_str().unwrap();
            let host = if case["host"] == "explorer" {
                Host::Explorer
            } else {
                Host::Main
            };
            let existing: Vec<_> = case["existing"]
                .as_array()
                .unwrap()
                .iter()
                .map(|kind| kind.as_str().unwrap())
                .collect();
            assert_eq!(
                supports(kind, host),
                case["supported"].as_bool().unwrap(),
                "{case}"
            );
            let surface = if case["surface"] == "panel" {
                LaunchSurface::Panel
            } else {
                LaunchSurface::Menu
            };
            assert_eq!(
                can_launch_on_surface(kind, host, &existing, surface),
                case["canLaunch"].as_bool().unwrap(),
                "{case}"
            );
        }
        assert!(!can_launch("unknown", Host::Explorer, &[]));
    }
}

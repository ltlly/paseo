//! Disk persistence for workspace split layouts.
//!
//! The original app persists layouts per host + workspace through its validated
//! zustand storage (`packages/app/src/stores/workspace-layout-storage.ts`). The
//! native client keeps the same identity: a layout belongs to one daemon host
//! and one workspace, so reconnecting to a different host cannot reuse a
//! same-named workspace's panes.
//!
//! Restored trees are untrusted input. They are bounded, checked for unique
//! pane/group ids, clamped to the split-size minimum, and their tab ownership is
//! deduplicated. A malformed store is moved aside instead of being silently
//! overwritten on the next save.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::split_layout::{
    MAX_PANES, MAX_TABS_PER_PANE, MAX_TREE_DEPTH, SplitGroup, SplitLayout, SplitNode, SplitPane,
    clamp_normalized_sizes,
};

/// A workspace layout file is a small JSON document; anything larger is not a
/// layout the app wrote and is refused before parsing.
pub const MAX_STORE_BYTES: u64 = 4 * 1024 * 1024;
pub const MAX_LAYOUTS: usize = 512;
/// `MAX_TREE_DEPTH` already limits leaves, but a corrupted tree can carry the
/// same depth through many groups. These caps keep restoration linear.
pub const MAX_NODES: usize = 128;
pub const MAX_ID_LEN: usize = 128;
pub const MAX_HOST_ID_LEN: usize = 256;
pub const MAX_TAB_ID_LEN: usize = 512;

const STORE_VERSION: u32 = 1;

/// Identity of a persisted layout: one daemon host, one workspace.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct LayoutKey {
    pub host_id: String,
    pub workspace_id: String,
}

impl LayoutKey {
    fn new(host_id: &str, workspace_id: &str) -> Option<Self> {
        let valid = |value: &str, max: usize| {
            !value.is_empty() && value.len() <= max && value.trim() == value
        };
        (valid(host_id, MAX_HOST_ID_LEN) && valid(workspace_id, MAX_ID_LEN)).then(|| Self {
            host_id: host_id.to_owned(),
            workspace_id: workspace_id.to_owned(),
        })
    }
}

#[derive(Deserialize, Serialize)]
struct PersistedFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    layouts: Vec<PersistedLayout>,
}

#[derive(Deserialize, Serialize)]
struct PersistedLayout {
    #[serde(rename = "hostId")]
    host_id: String,
    #[serde(rename = "workspaceId")]
    workspace_id: String,
    layout: SplitLayout,
}

pub struct SplitLayoutStore {
    path: PathBuf,
    layouts: BTreeMap<LayoutKey, SplitLayout>,
}

impl SplitLayoutStore {
    /// An empty store at `path`, used when the saved file cannot be read so
    /// the session starts with defaults instead of failing to open a window.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            layouts: BTreeMap::new(),
        }
    }

    /// Reads the store at `path`. A missing file is an empty store. A corrupt
    /// store is renamed to `<path>.<reason>-<id>.corrupt` so the next save
    /// starts clean without destroying what was there; when it cannot be moved
    /// aside this returns an error instead of leaving a file the next save would
    /// silently overwrite.
    pub fn load(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let mut store = Self {
            path,
            layouts: BTreeMap::new(),
        };
        let metadata = match fs::metadata(&store.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(store),
            Err(error) => {
                return Err(error).with_context(|| format!("stat {}", store.path.display()));
            }
        };
        ensure!(
            metadata.is_file(),
            "Workspace layout store is not a file: {}",
            store.path.display()
        );
        if metadata.len() > MAX_STORE_BYTES {
            store.move_corrupt_aside("oversized")?;
            return Ok(store);
        }
        let bytes = match fs::read(&store.path) {
            Ok(bytes) => bytes,
            Err(error) => {
                return Err(error).with_context(|| format!("read {}", store.path.display()));
            }
        };
        let persisted: PersistedFile = match serde_json::from_slice(&bytes) {
            Ok(persisted) => persisted,
            Err(_) => {
                store.move_corrupt_aside("invalid")?;
                return Ok(store);
            }
        };
        if persisted.version > STORE_VERSION {
            store.move_corrupt_aside("newer-version")?;
            return Ok(store);
        }
        for entry in persisted.layouts.into_iter().take(MAX_LAYOUTS) {
            let Some(key) = LayoutKey::new(&entry.host_id, &entry.workspace_id) else {
                continue;
            };
            if let Some(layout) = validate_layout(entry.layout) {
                store.layouts.entry(key).or_insert(layout);
            }
        }
        Ok(store)
    }

    /// Every layout persisted for one host, keyed by workspace id.
    pub fn layouts_for(&self, host_id: &str) -> BTreeMap<String, SplitLayout> {
        self.layouts
            .iter()
            .filter(|(key, _)| key.host_id == host_id)
            .map(|(key, layout)| (key.workspace_id.clone(), layout.clone()))
            .collect()
    }

    pub fn layout(&self, host_id: &str, workspace_id: &str) -> Option<&SplitLayout> {
        LayoutKey::new(host_id, workspace_id).and_then(|key| self.layouts.get(&key))
    }

    /// Validates and stores a layout in memory. Returns whether it changed.
    /// This never writes disk; `save` is the entry point that both persists and
    /// commits memory.
    pub fn insert(
        &mut self,
        host_id: &str,
        workspace_id: &str,
        layout: SplitLayout,
    ) -> Result<bool> {
        let key = layout_key(host_id, workspace_id)?;
        let layout = accept_layout(layout)?;
        if self.layouts.get(&key) == Some(&layout) {
            return Ok(false);
        }
        self.layouts.insert(key, layout);
        Ok(true)
    }

    /// Writes one layout after it changes. The whole file is rewritten, so the
    /// current file is re-read under a cross-process lock and this key is merged
    /// into it: another window's host or workspace layouts survive even when
    /// both windows save the same file. Memory follows the file only after the
    /// write succeeds, so a failed write leaves the previous document intact and
    /// the next save of the same layout retries.
    pub fn save(&mut self, host_id: &str, workspace_id: &str, layout: SplitLayout) -> Result<bool> {
        let key = layout_key(host_id, workspace_id)?;
        let layout = accept_layout(layout)?;
        let _lock = self.acquire_lock()?;
        let mut merged = self.read_disk()?;
        if merged.get(&key) == Some(&layout) {
            self.layouts = merged;
            return Ok(false);
        }
        let is_new_key = !merged.contains_key(&key);
        ensure!(
            !is_new_key || merged.len() < MAX_LAYOUTS,
            "Workspace layout store is full ({} workspaces)",
            MAX_LAYOUTS
        );
        merged.insert(key, layout);
        self.write_layouts(&merged)?;
        self.layouts = merged;
        Ok(true)
    }

    /// Rewrites the whole store from memory. Callers that share the file with
    /// another window use `save`, which merges first.
    pub fn flush(&self) -> Result<()> {
        let _lock = self.acquire_lock()?;
        self.write_layouts(&self.layouts)
    }

    fn write_layouts(&self, layouts: &BTreeMap<LayoutKey, SplitLayout>) -> Result<()> {
        ensure!(
            layouts.len() <= MAX_LAYOUTS,
            "Workspace layout store exceeds {} workspaces",
            MAX_LAYOUTS
        );
        let entries: Vec<PersistedLayout> = layouts
            .iter()
            .map(|(key, layout)| PersistedLayout {
                host_id: key.host_id.clone(),
                workspace_id: key.workspace_id.clone(),
                layout: layout.clone(),
            })
            .collect();
        let bytes = serde_json::to_vec(&PersistedFile {
            version: STORE_VERSION,
            layouts: entries,
        })?;
        ensure!(
            bytes.len() as u64 <= MAX_STORE_BYTES,
            "Workspace layout store exceeds {} bytes",
            MAX_STORE_BYTES
        );
        write_atomic(&self.path, &bytes)
    }

    /// The layouts another window may have written since this one loaded. The
    /// caller holds the lock, so the read cannot see a half-applied save.
    fn read_disk(&self) -> Result<BTreeMap<LayoutKey, SplitLayout>> {
        let mut layouts = BTreeMap::new();
        let metadata = match fs::metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(layouts),
            Err(error) => {
                return Err(error).with_context(|| format!("stat {}", self.path.display()));
            }
        };
        ensure!(
            metadata.is_file(),
            "Workspace layout store is not a file: {}",
            self.path.display()
        );
        ensure!(
            metadata.len() <= MAX_STORE_BYTES,
            "Workspace layout store at {} is larger than {} bytes; refusing to overwrite it",
            self.path.display(),
            MAX_STORE_BYTES
        );
        let bytes =
            fs::read(&self.path).with_context(|| format!("read {}", self.path.display()))?;
        let persisted: PersistedFile = serde_json::from_slice(&bytes).with_context(|| {
            format!(
                "parse {}; refusing to overwrite a store this build cannot read",
                self.path.display()
            )
        })?;
        ensure!(
            persisted.version <= STORE_VERSION,
            "Workspace layout store at {} was written by a newer version",
            self.path.display()
        );
        for entry in persisted.layouts {
            let Some(key) = LayoutKey::new(&entry.host_id, &entry.workspace_id) else {
                continue;
            };
            if let Some(layout) = validate_layout(entry.layout) {
                layouts.entry(key).or_insert(layout);
            }
        }
        ensure!(
            layouts.len() <= MAX_LAYOUTS,
            "Workspace layout store at {} holds more than {} workspaces",
            self.path.display(),
            MAX_LAYOUTS
        );
        Ok(layouts)
    }

    fn acquire_lock(&self) -> Result<File> {
        let lock_path = self.path.with_extension("lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .with_context(|| format!("open layout lock {}", lock_path.display()))?;
        file.lock()
            .with_context(|| format!("lock {}", lock_path.display()))?;
        Ok(file)
    }

    fn move_corrupt_aside(&mut self, reason: &str) -> Result<()> {
        // A second corrupt load must not replace the first backup.
        let destination = self
            .path
            .with_extension(format!("{reason}-{}.corrupt", Uuid::new_v4()));
        fs::rename(&self.path, &destination).with_context(|| {
            format!(
                "move unreadable workspace layout store {} to {}",
                self.path.display(),
                destination.display()
            )
        })
    }
}

/// Validates one persisted tree. Returns the sanitized layout or `None` when
/// the tree cannot be trusted (missing ids, duplicate ids, too deep, too
/// large). Tab ownership is repaired rather than rejected: a duplicate tab
/// belongs to the first pane that claims it, focus falls back to the last tab,
/// and sizes are rescaled with the same minimum the live model uses.
pub fn validate_layout(layout: SplitLayout) -> Option<SplitLayout> {
    let mut ids = BTreeSet::new();
    let mut tabs = BTreeSet::new();
    let mut budget = RestoreBudget { nodes: 0, panes: 0 };
    let root = validate_node(layout.root, 1, &mut ids, &mut tabs, &mut budget)?;
    if ids.contains(crate::explorer_layout::EXPLORER_PANE_ID) {
        return None;
    }
    if budget.panes == 0 {
        return None;
    }
    let mut explorer = layout.explorer.normalized();
    if let Some(pane) = explorer.pane.take() {
        if pane.id != crate::explorer_layout::EXPLORER_PANE_ID {
            return None;
        }
        explorer.pane = Some(validate_pane(pane, &mut ids, &mut tabs)?);
    }
    let mut restored = SplitLayout {
        root,
        focused_pane_id: layout.focused_pane_id,
        active_pane_id: None,
        explorer,
    };
    let panes = restored.panes();
    let focused = restored
        .focused_pane_id
        .clone()
        .filter(|id| panes.iter().any(|pane| &pane.id == id))
        .or_else(|| panes.first().map(|pane| pane.id.clone()));
    restored.focused_pane_id = focused;
    Some(restored)
}

struct RestoreBudget {
    nodes: usize,
    panes: usize,
}

fn layout_key(host_id: &str, workspace_id: &str) -> Result<LayoutKey> {
    LayoutKey::new(host_id, workspace_id).with_context(|| {
        format!("invalid workspace layout identity ({host_id:?}, {workspace_id:?})")
    })
}

/// The layout a save may persist: it validates and survives validation
/// unchanged. A tree that would be rewritten on disk (duplicate ids, tabs
/// deduplicated or cut at `MAX_TABS_PER_PANE`, a dangling focused pane) is
/// refused while the caller still holds the live state it can correct.
fn accept_layout(layout: SplitLayout) -> Result<SplitLayout> {
    let restored = validate_layout(layout.clone()).with_context(|| {
        format!(
            "split layout cannot be stored: duplicate or missing pane/group ids, deeper than {MAX_TREE_DEPTH} levels, or more than {MAX_PANES} panes"
        )
    })?;
    if restored.focused_pane_id != layout.focused_pane_id
        || restored.all_panes() != layout.all_panes()
    {
        bail!(
            "split layout cannot be stored without loss: duplicate tabs, more than {} tabs in one pane, or a missing focused pane",
            MAX_TABS_PER_PANE
        );
    }
    Ok(restored)
}

fn validate_node(
    node: SplitNode,
    depth: usize,
    ids: &mut BTreeSet<String>,
    tabs: &mut BTreeSet<String>,
    budget: &mut RestoreBudget,
) -> Option<SplitNode> {
    if depth > MAX_TREE_DEPTH {
        return None;
    }
    budget.nodes += 1;
    if budget.nodes > MAX_NODES {
        return None;
    }
    match node {
        SplitNode::Pane { pane } => {
            budget.panes += 1;
            if budget.panes > MAX_PANES {
                return None;
            }
            let pane = validate_pane(pane, ids, tabs)?;
            Some(SplitNode::Pane { pane })
        }
        SplitNode::Group { group } => {
            if !valid_id(&group.id, ids) {
                return None;
            }
            if group.children.len() < 2 {
                return None;
            }
            let mut children = Vec::with_capacity(group.children.len());
            for child in group.children {
                children.push(validate_node(child, depth + 1, ids, tabs, budget)?);
            }
            let sizes = if group.sizes.len() == children.len() {
                // A non-finite or non-positive size is replaced with an even
                // share rather than dropping the pane the user positioned.
                group
                    .sizes
                    .into_iter()
                    .map(|size| {
                        if size.is_finite() && size > 0.0 {
                            size
                        } else {
                            1.0
                        }
                    })
                    .collect()
            } else {
                vec![1.0; children.len()]
            };
            Some(SplitNode::Group {
                group: SplitGroup {
                    id: group.id,
                    direction: group.direction,
                    children,
                    sizes: clamp_normalized_sizes(&sizes),
                },
            })
        }
    }
}

fn validate_pane(
    pane: SplitPane,
    ids: &mut BTreeSet<String>,
    tabs: &mut BTreeSet<String>,
) -> Option<SplitPane> {
    if !valid_id(&pane.id, ids) {
        return None;
    }
    let mut owned = Vec::new();
    for tab in pane.tabs {
        let tab = tab.trim();
        if tab.is_empty() || tab.len() > MAX_TAB_ID_LEN || owned.len() >= MAX_TABS_PER_PANE {
            continue;
        }
        if !tabs.insert(tab.to_owned()) {
            continue;
        }
        owned.push(tab.to_owned());
    }
    let focused_tab = pane
        .focused_tab
        .filter(|tab| owned.iter().any(|candidate| candidate == tab))
        .or_else(|| owned.last().cloned());
    Some(SplitPane {
        id: pane.id,
        tabs: owned,
        focused_tab,
    })
}

fn valid_id(id: &str, seen: &mut BTreeSet<String>) -> bool {
    if id.is_empty() || id.len() > MAX_ID_LEN || id.trim() != id {
        return false;
    }
    seen.insert(id.to_owned())
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let directory = path
        .parent()
        .context("Workspace layout store has no parent directory")?;
    fs::create_dir_all(directory)
        .with_context(|| format!("create layout directory {}", directory.display()))?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("workspace-layouts.json");
    let temporary = directory.join(format!(".{name}.{}.tmp", Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .with_context(|| format!("create {}", temporary.display()))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path).with_context(|| format!("replace {}", path.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
        return result;
    }
    // The rename is durable once the directory entry is synced. Not every
    // platform allows opening a directory; the data file is already synced.
    #[cfg(unix)]
    if let Ok(directory) = File::open(directory) {
        let _ = directory.sync_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests {

    #[test]
    fn explorer_restore_deduplicates_and_persists_without_keyboard_focus() {
        use crate::explorer_layout::EXPLORER_PANE_ID as DOCK;
        let mut layout = crate::split_layout::SplitLayout::single("main");
        layout.attach_tab("original");
        layout.explorer.ensure_pane();
        layout.focus_pane(DOCK);
        layout.attach_tab("side-terminal");
        let serialized = serde_json::to_vec(&layout).unwrap();
        let restored =
            super::validate_layout(serde_json::from_slice(&serialized).unwrap()).unwrap();
        assert_eq!(restored.focused_pane_id.as_deref(), Some("main"));
        assert_eq!(restored.active_pane().unwrap().id, "main");
        assert_eq!(restored.pane(DOCK).unwrap(), layout.pane(DOCK).unwrap());
        layout
            .explorer
            .pane
            .as_mut()
            .unwrap()
            .tabs
            .push("original".into());
        assert!(super::accept_layout(layout.clone()).is_err());
        let repaired = super::validate_layout(layout).unwrap();
        assert!(
            !repaired
                .pane(DOCK)
                .unwrap()
                .tabs
                .contains(&"original".into())
        );
        let mut invalid = restored;
        invalid.explorer.pane.as_mut().unwrap().id = "main".into();
        assert!(super::validate_layout(invalid).is_none());
    }
    use super::*;
    use crate::split_layout::{
        MAIN_PANE_ID, MIN_SPLIT_SIZE, SplitDirection, SplitIds, SplitPosition,
    };

    fn temporary_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("paseo-layout-{name}-{}.json", Uuid::new_v4()))
    }

    fn corrupt_backups(path: &Path) -> Vec<PathBuf> {
        let prefix = format!(
            "{}.",
            path.file_stem().unwrap_or_default().to_string_lossy()
        );
        fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|candidate| {
                let name = candidate.file_name().unwrap_or_default().to_string_lossy();
                name.starts_with(&prefix) && name.ends_with(".corrupt")
            })
            .collect()
    }

    fn split_layout(panes: &[(&str, &[&str])]) -> SplitLayout {
        let mut layout = SplitLayout::single("main");
        let mut ids = SplitIds::default();
        for index in 1..panes.len() {
            let target = panes[index - 1].0;
            layout
                .split_empty(target, SplitPosition::Right, &mut ids)
                .expect("split within depth");
        }
        for (pane_id, tabs) in panes {
            layout.focus_pane(pane_id);
            for tab in *tabs {
                layout.attach_tab(tab);
            }
        }
        layout
    }

    #[test]
    fn explorer_intent_round_trips_per_host_and_workspace() {
        let path = temporary_path("explorer-intent");
        let mut first = SplitLayoutStore::load(&path).unwrap();
        let mut second = SplitLayoutStore::load(&path).unwrap();
        let mut a = SplitLayout::single("main");
        a.explorer.visible = true;
        a.explorer.width = 480.5;
        first.save("host-a", "workspace-a", a.clone()).unwrap();
        let mut b = SplitLayout::single("main");
        b.explorer.width = 360.;
        second.save("host-a", "workspace-b", b.clone()).unwrap();
        second.save("host-b", "workspace-a", b.clone()).unwrap();
        let restored = SplitLayoutStore::load(&path).unwrap();
        assert_eq!(restored.layout("host-a", "workspace-a"), Some(&a));
        assert_eq!(restored.layout("host-a", "workspace-b"), Some(&b));
        assert_eq!(restored.layout("host-b", "workspace-a"), Some(&b));
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn old_layouts_default_closed_and_invalid_widths_do_not_replace_the_tree() {
        let old = serde_json::json!({"root":{"kind":"pane","pane":{"id":"main","tabs":[]}},"focusedPaneId":"main"});
        let mut layout: SplitLayout = serde_json::from_value(old).unwrap();
        assert!(!layout.explorer.visible);
        assert_eq!(layout.explorer.width, 320.);
        layout.explorer.width = f64::NAN;
        let restored = validate_layout(layout).unwrap();
        assert_eq!(restored.explorer.width, 320.);
        assert_eq!(restored.focused_pane_id.as_deref(), Some("main"));
    }

    #[test]
    fn layouts_are_scoped_by_host_and_workspace_and_survive_reload() {
        let path = temporary_path("scope");
        let mut store = SplitLayoutStore::load(&path).unwrap();
        let host_a = split_layout(&[("main", &["term-a"]), ("pane-1", &["term-b"])]);
        let host_b = split_layout(&[("main", &["term-c"])]);
        store.save("host-a", "workspace-1", host_a.clone()).unwrap();
        store
            .save("host-a", "workspace-2", SplitLayout::single("main"))
            .unwrap();
        store.save("host-b", "workspace-1", host_b.clone()).unwrap();

        let reloaded = SplitLayoutStore::load(&path).unwrap();
        assert_eq!(reloaded.layout("host-a", "workspace-1"), Some(&host_a));
        assert_eq!(reloaded.layout("host-b", "workspace-1"), Some(&host_b));
        assert_eq!(
            reloaded.layout("host-a", "workspace-2"),
            Some(&SplitLayout::single("main"))
        );
        assert_eq!(reloaded.layout("host-b", "workspace-2"), None);
        assert_eq!(reloaded.layouts_for("host-a").len(), 2);
        assert_eq!(reloaded.layouts_for("host-b").len(), 1);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn restore_repairs_tab_ownership_and_clamps_sizes() {
        let mut layout = split_layout(&[("main", &["term-a"]), ("pane-1", &["term-b"])]);
        if let SplitNode::Group { group } = &mut layout.root {
            group.sizes = vec![0.01, 0.99];
            let SplitNode::Pane { pane } = &mut group.children[1] else {
                panic!("expected pane");
            };
            // The same tab cannot be owned by two panes.
            pane.tabs.push("term-a".into());
            pane.focused_tab = Some("term-missing".into());
            pane.tabs.push("".into());
        }
        let restored = validate_layout(layout).expect("restorable");
        let SplitNode::Group { group } = &restored.root else {
            panic!("expected group");
        };
        assert!((group.sizes[0] - MIN_SPLIT_SIZE).abs() < 1e-9);
        assert!((group.sizes[1] - (1.0 - MIN_SPLIT_SIZE)).abs() < 1e-9);
        assert_eq!(
            restored.find_pane_containing_tab("term-a").unwrap().id,
            "main"
        );
        let pane = restored.pane("pane-1").unwrap();
        assert_eq!(pane.tabs, vec!["term-b"]);
        assert_eq!(pane.focused_tab.as_deref(), Some("term-b"));
    }

    #[test]
    fn invalid_trees_are_refused_instead_of_half_restored() {
        let mut duplicate = split_layout(&[("main", &[]), ("pane-1", &[])]);
        if let SplitNode::Group { group } = &mut duplicate.root {
            if let SplitNode::Pane { pane } = &mut group.children[1] {
                pane.id = "main".into();
            }
        }
        assert_eq!(validate_layout(duplicate), None);

        // MAX_TREE_DEPTH is 5; a sixth nested group cannot be produced by the
        // live model, so it can only arrive from a hand-edited store.
        let mut node = SplitNode::Pane {
            pane: SplitPane {
                id: "main".into(),
                ..Default::default()
            },
        };
        for depth in 1..=MAX_TREE_DEPTH + 1 {
            node = SplitNode::Group {
                group: SplitGroup {
                    id: format!("group-{depth}"),
                    direction: SplitDirection::Vertical,
                    children: vec![
                        node,
                        SplitNode::Pane {
                            pane: SplitPane {
                                id: format!("pane-{depth}"),
                                ..Default::default()
                            },
                        },
                    ],
                    sizes: vec![0.5, 0.5],
                },
            };
        }
        assert_eq!(
            validate_layout(SplitLayout {
                root: node,
                focused_pane_id: Some("main".into()),
                active_pane_id: None,
                explorer: Default::default(),
            }),
            None
        );
    }

    #[test]
    fn corrupt_and_oversized_stores_are_moved_aside() {
        let path = temporary_path("corrupt");
        fs::write(&path, b"{not json").unwrap();
        let store = SplitLayoutStore::load(&path).unwrap();
        assert!(store.layouts.is_empty());
        assert!(!path.exists());
        assert_eq!(corrupt_backups(&path).len(), 1);
        // A second unreadable document gets its own backup instead of replacing
        // the first one, which still holds what the user had.
        fs::write(&path, b"{still not json").unwrap();
        let store = SplitLayoutStore::load(&path).unwrap();
        assert!(store.layouts.is_empty());
        let backups = corrupt_backups(&path);
        assert_eq!(backups.len(), 2);
        for backup in backups {
            let _ = fs::remove_file(backup);
        }

        let oversized = temporary_path("oversized");
        let file = File::create(&oversized).unwrap();
        file.set_len(MAX_STORE_BYTES + 1).unwrap();
        drop(file);
        let store = SplitLayoutStore::load(&oversized).unwrap();
        assert!(store.layouts.is_empty());
        assert!(!oversized.exists());
        for backup in corrupt_backups(&oversized) {
            let _ = fs::remove_file(backup);
        }
    }

    #[test]
    fn restore_refuses_trees_over_the_pane_bound() {
        let children: Vec<SplitNode> = (0..=MAX_PANES)
            .map(|index| SplitNode::Pane {
                pane: SplitPane {
                    id: format!("pane-{index}"),
                    ..Default::default()
                },
            })
            .collect();
        let layout = SplitLayout {
            root: SplitNode::Group {
                group: SplitGroup {
                    id: "group-1".into(),
                    direction: SplitDirection::Horizontal,
                    children,
                    sizes: vec![1.0; MAX_PANES + 1],
                },
            },
            focused_pane_id: Some("pane-0".into()),
            active_pane_id: None,
            explorer: Default::default(),
        };
        assert_eq!(validate_layout(layout), None);
    }

    #[test]
    fn saving_replaces_the_previous_document_atomically() {
        let path = temporary_path("replace");
        let mut store = SplitLayoutStore::load(&path).unwrap();
        store
            .save("host", "workspace", SplitLayout::single("main"))
            .unwrap();
        let first = fs::read_to_string(&path).unwrap();
        assert!(first.contains("main"));
        let layout = split_layout(&[("main", &["term-a"]), ("pane-1", &["term-b"])]);
        assert!(store.save("host", "workspace", layout.clone()).unwrap());
        let current = SplitLayoutStore::load(&path).unwrap();
        assert_eq!(current.layout("host", "workspace"), Some(&layout));
        // An unchanged layout does not rewrite the file.
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        assert!(!store.save("host", "workspace", layout).unwrap());
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), modified);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn save_refuses_invalid_identity_instead_of_dropping_the_layout() {
        let path = temporary_path("identity");
        let mut store = SplitLayoutStore::load(&path).unwrap();
        store
            .save("host-a", "workspace", SplitLayout::single("main"))
            .unwrap();
        assert!(
            store
                .insert("", "workspace", SplitLayout::single("main"))
                .is_err()
        );
        assert!(
            store
                .save("", "workspace", SplitLayout::single("main"))
                .is_err()
        );
        assert!(
            store
                .save("host-b", "workspace", SplitLayout::single("main"))
                .unwrap()
        );
        let reloaded = SplitLayoutStore::load(&path).unwrap();
        assert!(reloaded.layout("host-a", "workspace").is_some());
        assert!(reloaded.layout("host-b", "workspace").is_some());
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn one_window_keeps_the_layouts_another_window_wrote() {
        let path = temporary_path("multiwindow");
        // Both windows loaded the empty file before either saved.
        let mut first = SplitLayoutStore::load(&path).unwrap();
        let mut second = SplitLayoutStore::load(&path).unwrap();
        let host_a = split_layout(&[("main", &["term-a"]), ("pane-1", &["term-b"])]);
        first.save("host-a", "workspace-a", host_a.clone()).unwrap();
        let host_b = split_layout(&[("main", &["term-c"])]);
        second
            .save("host-b", "workspace-b", host_b.clone())
            .unwrap();

        let reloaded = SplitLayoutStore::load(&path).unwrap();
        assert_eq!(reloaded.layout("host-a", "workspace-a"), Some(&host_a));
        assert_eq!(reloaded.layout("host-b", "workspace-b"), Some(&host_b));
        // The saving window also adopts what the other window wrote, so its
        // next save starts from the file instead of a stale snapshot.
        assert_eq!(second.layout("host-a", "workspace-a"), Some(&host_a));
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(path.with_extension("lock"));
    }

    #[test]
    fn a_failed_write_keeps_the_previous_document_and_the_next_save_retries() {
        let path = temporary_path("retry");
        let mut store = SplitLayoutStore::load(&path).unwrap();
        // A directory where the store belongs makes the atomic replace fail.
        fs::create_dir_all(&path).unwrap();
        let layout = split_layout(&[("main", &["term-a"]), ("pane-1", &["term-b"])]);
        assert!(store.save("host", "workspace", layout.clone()).is_err());
        fs::remove_dir(&path).unwrap();
        // The failed save did not count as saved, so the same layout is written.
        assert!(store.save("host", "workspace", layout.clone()).unwrap());
        let reloaded = SplitLayoutStore::load(&path).unwrap();
        assert_eq!(reloaded.layout("host", "workspace"), Some(&layout));
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(path.with_extension("lock"));
    }

    #[test]
    fn save_refuses_a_layout_it_cannot_store_without_loss() {
        let path = temporary_path("lossy");
        let mut store = SplitLayoutStore::load(&path).unwrap();
        let tabs: Vec<String> = (0..MAX_TABS_PER_PANE)
            .map(|index| format!("term-{index}"))
            .collect();
        let oversized = SplitLayout {
            root: SplitNode::Pane {
                pane: SplitPane {
                    id: MAIN_PANE_ID.into(),
                    tabs: tabs.clone(),
                    focused_tab: tabs.last().cloned(),
                },
            },
            focused_pane_id: Some(MAIN_PANE_ID.into()),
            active_pane_id: None,
            explorer: Default::default(),
        };
        // The live model refuses the 257th tab instead of accepting it and
        // letting the save drop it.
        let mut model = oversized.clone();
        assert_eq!(model.attach_tab("term-extra"), None);
        assert_eq!(
            model.pane(MAIN_PANE_ID).unwrap().tabs.len(),
            MAX_TABS_PER_PANE
        );

        let mut lossy = oversized.clone();
        let SplitNode::Pane { pane } = &mut lossy.root else {
            unreachable!()
        };
        pane.tabs.push("term-extra".into());
        let error = store.save("host", "workspace", lossy).unwrap_err();
        assert!(format!("{error:#}").contains("without loss"));
        assert!(!path.exists(), "a refused layout must not be written");
        assert!(store.save("host", "workspace", oversized).unwrap());
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn save_refuses_a_new_key_at_capacity_before_touching_the_file() {
        let path = temporary_path("capacity");
        let entries: Vec<PersistedLayout> = (0..MAX_LAYOUTS)
            .map(|index| PersistedLayout {
                host_id: "host".into(),
                workspace_id: format!("workspace-{index}"),
                layout: SplitLayout::single(MAIN_PANE_ID),
            })
            .collect();
        fs::write(
            &path,
            serde_json::to_vec(&PersistedFile {
                version: STORE_VERSION,
                layouts: entries,
            })
            .unwrap(),
        )
        .unwrap();
        let mut store = SplitLayoutStore::load(&path).unwrap();
        let before = fs::read(&path).unwrap();
        assert!(
            store
                .save("host", "workspace-new", SplitLayout::single(MAIN_PANE_ID))
                .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), before);
        // Replacing a key that already fits still works at the ceiling.
        let replacement = split_layout(&[("main", &["term-a"]), ("pane-1", &["term-b"])]);
        assert!(
            store
                .save("host", "workspace-1", replacement.clone())
                .unwrap()
        );
        let reloaded = SplitLayoutStore::load(&path).unwrap();
        assert_eq!(reloaded.layout("host", "workspace-1"), Some(&replacement));
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(path.with_extension("lock"));
    }
}

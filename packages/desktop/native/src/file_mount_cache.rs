use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::split_layout::SplitLayout;

pub const DEFAULT_FILE_MOUNT_CAP: usize = 3;
pub const FILE_TAB_PREFIX: &str = "file:";

/// Canonical workspace-scoped identity for an open file tab.
///
/// Keying by `(cwd, path)` ensures the same relative path in two workspaces
/// stays two distinct buffers, and a persisted tab naming another workspace's
/// `cwd` is rejected during restore.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FileTabKey {
    pub cwd: String,
    pub path: String,
}

impl FileTabKey {
    pub fn new(cwd: &str, path: &str) -> Self {
        Self {
            cwd: cwd.to_owned(),
            path: path.to_owned(),
        }
    }

    /// Parses and validates a persisted pane tab ID against `workspace_cwd`.
    /// Rejects empty or malformed payloads, non-canonical JSON encodings, and
    /// tabs belonging to a different workspace directory.
    pub fn from_tab_id(tab_id: &str, workspace_cwd: &str) -> Option<Self> {
        let payload = tab_id.strip_prefix(FILE_TAB_PREFIX)?;
        let (cwd, path): (String, String) = serde_json::from_str(payload).ok()?;
        if cwd.is_empty() || path.is_empty() || cwd != workspace_cwd {
            return None;
        }
        let key = Self { cwd, path };
        (key.tab_id() == tab_id).then_some(key)
    }

    pub fn title(&self) -> String {
        self.path
            .rsplit('/')
            .next()
            .unwrap_or(&self.path)
            .to_owned()
    }

    pub fn tab_id(&self) -> String {
        format!(
            "{FILE_TAB_PREFIX}{}",
            serde_json::to_string(&(&self.cwd, &self.path)).expect("serialize file tab key")
        )
    }
}

/// Returns every persisted file tab in `layout` that belongs to `workspace_cwd`.
pub fn restorable_file_tabs(layout: &SplitLayout, workspace_cwd: &str) -> Vec<FileTabKey> {
    layout
        .all_panes()
        .into_iter()
        .flat_map(|pane| pane.tabs)
        .filter_map(|tab| FileTabKey::from_tab_id(&tab, workspace_cwd))
        .collect()
}

/// Returns only the file tabs in `layout` that are within each pane's initial
/// mounted LRU window (plus any `retained_tabs`), avoiding eager loading and
/// host-watcher creation for cold background file tabs on restore.
pub fn mounted_restorable_file_tabs(
    cache: &mut FileMountCache,
    workspace_id: &str,
    layout: &SplitLayout,
    workspace_cwd: &str,
    retained_tabs: &BTreeSet<String>,
) -> Vec<FileTabKey> {
    let mounted = cache.sync_workspace(workspace_id, layout, retained_tabs);
    layout
        .all_panes()
        .into_iter()
        .flat_map(|pane| pane.tabs)
        .filter(|tab| mounted.contains(tab.as_str()))
        .filter_map(|tab| FileTabKey::from_tab_id(&tab, workspace_cwd))
        .collect()
}

/// Returns the file tab keys in `layout` whose pane is currently rendered on
/// screen (`layout.rendered_all_panes()`) and whose tab is the pane's focused
/// tab. Matches Web `FilePane`'s `observing = isActive && isAppVisible`: only
/// the active tab in a visible pane (when the window is visible and not covered
/// by a full-window pane) should actively observe the file on the host.
pub fn visible_file_tabs(
    layout: &SplitLayout,
    workspace_cwd: &str,
    window_visible: bool,
) -> BTreeSet<FileTabKey> {
    if !window_visible {
        return BTreeSet::new();
    }
    layout
        .rendered_all_panes()
        .into_iter()
        .filter_map(|pane| pane.focused_tab)
        .filter_map(|tab| FileTabKey::from_tab_id(&tab, workspace_cwd))
        .collect()
}

/// Per-pane LRU tracker for mounted tabs, porting `deriveMountedTabLru` from
/// `packages/app/src/screens/workspace/use-mounted-tab-set.ts`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PaneMountLru {
    cap: usize,
    lru: Vec<String>,
}

impl PaneMountLru {
    pub fn new(cap: usize) -> Self {
        Self {
            cap: cap.max(1),
            lru: Vec::new(),
        }
    }

    pub fn cap(&self) -> usize {
        self.cap.max(1)
    }

    pub fn lru(&self) -> &[String] {
        &self.lru
    }

    /// Recomputes and commits the mounted LRU for this pane from the current
    /// active tab, available tabs in the pane, and modified (`retained`) tabs.
    ///
    /// Order matches Web `deriveMountedTabLru`:
    /// 1. `active_tab` if present in `available_tabs`
    /// 2. every `retained` tab present in `available_tabs` (excluding `active_tab`)
    /// 3. previous LRU entries still in `available_tabs` (excluding already included tabs)
    ///    until `next.len() >= cap`.
    ///
    /// When `active_tab` plus `retained` tabs exceed `cap`, all of them remain
    /// mounted beyond the cap; once a retained tab becomes clean, the next sync
    /// trims back to `cap`.
    pub fn sync(
        &mut self,
        active_tab: Option<&str>,
        available_tabs: &[String],
        retained_tabs: &BTreeSet<String>,
    ) -> BTreeSet<String> {
        let max_size = self.cap.max(1);
        let available: BTreeSet<&str> = available_tabs.iter().map(String::as_str).collect();
        let mut next: Vec<String> = Vec::new();

        if let Some(active) = active_tab
            && available.contains(active)
        {
            next.push(active.to_owned());
        }

        // Preserve pane tab order for retained tabs that belong to this pane,
        // matching Web's `useModifiedPanelTabIds` which filters `input.tabIds`
        // in pane tab order.
        for tab_id in available_tabs {
            if retained_tabs.contains(tab_id.as_str())
                && Some(tab_id.as_str()) != active_tab
                && !next.iter().any(|existing| existing == tab_id)
            {
                next.push(tab_id.clone());
            }
        }

        for tab_id in &self.lru {
            if next.len() >= max_size {
                break;
            }
            if Some(tab_id.as_str()) != active_tab
                && available.contains(tab_id.as_str())
                && !next.iter().any(|existing| existing == tab_id)
            {
                next.push(tab_id.clone());
            }
        }

        self.lru = next.clone();
        next.into_iter().collect()
    }
}

/// Coordinates per-pane mounted tab LRUs across workspaces and panes, returning
/// which tabs remain mounted and which previously mounted tabs were evicted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileMountCache {
    cap: usize,
    /// Keyed by `(workspace_id, pane_id)`.
    panes: BTreeMap<(String, String), PaneMountLru>,
}

impl Default for FileMountCache {
    fn default() -> Self {
        Self::new(DEFAULT_FILE_MOUNT_CAP)
    }
}

/// Outcome of synchronizing mounted tabs against the current split layouts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MountSyncOutcome {
    /// All tab IDs currently mounted across all synced panes.
    pub mounted_tabs: BTreeSet<String>,
    /// Previously mounted tab IDs that are no longer mounted in any pane and
    /// are not protected by `retained_tabs` (safe to evict).
    pub evicted_tabs: Vec<String>,
}

impl FileMountCache {
    pub fn new(cap: usize) -> Self {
        Self {
            cap: cap.max(1),
            panes: BTreeMap::new(),
        }
    }

    pub fn cap(&self) -> usize {
        self.cap.max(1)
    }

    pub fn clear(&mut self) {
        self.panes.clear();
    }

    pub fn pane_lru(&self, workspace_id: &str, pane_id: &str) -> Option<&[String]> {
        self.panes
            .get(&(workspace_id.to_owned(), pane_id.to_owned()))
            .map(PaneMountLru::lru)
    }

    /// Removes all pane LRU state for `workspace_id` (e.g., when a workspace is
    /// archived or purged).
    pub fn remove_workspace(&mut self, workspace_id: &str) {
        self.panes.retain(|(ws, _), _| ws != workspace_id);
    }

    /// Synchronizes a single workspace's panes against `mounted_before` (for
    /// this workspace) and `retained_tabs`, returning the mounted tab IDs and
    /// any previously mounted tabs in this workspace that are now safe to evict.
    pub fn sync_workspace_with_evictions(
        &mut self,
        workspace_id: &str,
        layout: &SplitLayout,
        mounted_before: &BTreeSet<String>,
        retained_tabs: &BTreeSet<String>,
    ) -> MountSyncOutcome {
        let mounted_tabs = self.sync_workspace(workspace_id, layout, retained_tabs);
        let evicted_tabs = mounted_before
            .iter()
            .filter(|tab_id| {
                !mounted_tabs.contains(tab_id.as_str()) && !retained_tabs.contains(tab_id.as_str())
            })
            .cloned()
            .collect();
        MountSyncOutcome {
            mounted_tabs,
            evicted_tabs,
        }
    }

    /// Synchronizes a single workspace's panes and returns the mounted tab IDs
    /// for that workspace.
    pub fn sync_workspace(
        &mut self,
        workspace_id: &str,
        layout: &SplitLayout,
        retained_tabs: &BTreeSet<String>,
    ) -> BTreeSet<String> {
        let all_panes = layout.all_panes();
        let live_pane_ids: BTreeSet<&str> = all_panes.iter().map(|pane| pane.id.as_str()).collect();
        self.panes.retain(|(ws, pane_id), _| {
            ws != workspace_id || live_pane_ids.contains(pane_id.as_str())
        });

        let mut mounted = BTreeSet::new();
        for pane in &all_panes {
            let entry = self
                .panes
                .entry((workspace_id.to_owned(), pane.id.clone()))
                .or_insert_with(|| PaneMountLru::new(self.cap));
            let pane_mounted = entry.sync(pane.focused_tab.as_deref(), &pane.tabs, retained_tabs);
            mounted.extend(pane_mounted);
        }
        mounted
    }

    /// Synchronizes all workspaces' split layouts against the currently mounted
    /// file tabs (`mounted_before`) and modified file tabs (`retained_tabs`).
    ///
    /// - A tab moved between panes stays in `mounted_tabs` as long as the
    ///   target pane mounts it (which is guaranteed when moving focuses it or
    ///   when it is dirty), so it is never placed in `evicted_tabs`.
    /// - A dirty tab in `retained_tabs` is never placed in `evicted_tabs`.
    pub fn sync_layouts<'a>(
        &mut self,
        layouts: impl IntoIterator<Item = (&'a str, &'a SplitLayout)>,
        mounted_before: &BTreeSet<String>,
        retained_tabs: &BTreeSet<String>,
    ) -> MountSyncOutcome {
        let mut live_workspaces = BTreeSet::new();
        let mut mounted_tabs = BTreeSet::new();

        for (workspace_id, layout) in layouts {
            live_workspaces.insert(workspace_id.to_owned());
            mounted_tabs.extend(self.sync_workspace(workspace_id, layout, retained_tabs));
        }

        self.panes
            .retain(|(ws, _), _| live_workspaces.contains(ws.as_str()));

        let evicted_tabs = mounted_before
            .iter()
            .filter(|tab_id| {
                !mounted_tabs.contains(tab_id.as_str()) && !retained_tabs.contains(tab_id.as_str())
            })
            .cloned()
            .collect();

        MountSyncOutcome {
            mounted_tabs,
            evicted_tabs,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::explorer_layout::EXPLORER_PANE_ID;
    use crate::split_layout::{MAIN_PANE_ID, SplitIds, SplitLayout, SplitPosition};

    #[test]
    fn includes_newly_active_tab_and_preserves_cap_with_lru_eviction() {
        let mut pane = PaneMountLru::new(3);
        let all = vec![
            "a".to_owned(),
            "b".to_owned(),
            "c".to_owned(),
            "d".to_owned(),
        ];
        let clean = BTreeSet::new();

        pane.sync(Some("a"), &all, &clean);
        assert_eq!(pane.lru(), &["a"]);

        pane.sync(Some("b"), &all, &clean);
        assert_eq!(pane.lru(), &["b", "a"]);

        pane.sync(Some("c"), &all, &clean);
        assert_eq!(pane.lru(), &["c", "b", "a"]);

        // Visiting d evicts the least recently used tab ("a").
        pane.sync(Some("d"), &all, &clean);
        assert_eq!(pane.lru(), &["d", "c", "b"]);

        // Re-visiting "b" promotes it to the front without evicting "d" or "c".
        pane.sync(Some("b"), &all, &clean);
        assert_eq!(pane.lru(), &["b", "d", "c"]);

        // Visiting "a" now evicts "c" (the least recent of b, d, c).
        pane.sync(Some("a"), &all, &clean);
        assert_eq!(pane.lru(), &["a", "b", "d"]);
    }

    #[test]
    fn retains_dirty_tabs_beyond_cap_and_evicts_normally_after_save() {
        // Mirrors packages/app/src/panels/retained-chat-content.browser.test.tsx:
        // "retains modified panels beyond the LRU cap and evicts them normally after save"
        let mut pane = PaneMountLru::new(3);
        let all = vec![
            "a".to_owned(),
            "b".to_owned(),
            "c".to_owned(),
            "d".to_owned(),
            "e".to_owned(),
        ];
        let clean = BTreeSet::new();

        pane.sync(Some("a"), &all, &clean);
        pane.sync(Some("b"), &all, &clean);
        pane.sync(Some("c"), &all, &clean);
        pane.sync(Some("d"), &all, &clean);
        assert_eq!(pane.lru(), &["d", "c", "b"]);

        let dirty: BTreeSet<String> = ["a", "b", "c", "d"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        pane.sync(Some("e"), &all, &dirty);
        assert_eq!(pane.lru(), &["e", "a", "b", "c", "d"]);

        // Once saved (no longer dirty), the set trims back to cap=3 using the
        // committed LRU order ["e", "a", "b", "c", "d"] -> ["e", "a", "b"].
        pane.sync(Some("e"), &all, &clean);
        assert_eq!(pane.lru(), &["e", "a", "b"]);
    }

    #[test]
    fn cap_is_per_pane_and_independent_across_split_and_explorer_panes() {
        let mut cache = FileMountCache::new(3);
        let mut layout = SplitLayout::single(MAIN_PANE_ID);
        let mut ids = SplitIds::for_layout(&layout.root);
        let right_pane = layout
            .split_empty(MAIN_PANE_ID, SplitPosition::Right, &mut ids)
            .unwrap();
        layout.explorer.ensure_pane();
        layout.explorer.visible = true;

        // Open 4 tabs in main pane: m1, m2, m3, m4
        layout.focus_pane(MAIN_PANE_ID);
        for tab in ["m1", "m2", "m3", "m4"] {
            layout.attach_tab(tab).unwrap();
            cache.sync_workspace("ws", &layout, &BTreeSet::new());
        }
        // Open 4 tabs in right pane: r1, r2, r3, r4
        layout.focus_pane(&right_pane);
        for tab in ["r1", "r2", "r3", "r4"] {
            layout.attach_tab(tab).unwrap();
            cache.sync_workspace("ws", &layout, &BTreeSet::new());
        }
        // Open 2 file tabs in explorer pane (alongside files & changes): e1, e2
        layout.focus_pane(EXPLORER_PANE_ID);
        for tab in ["e1", "e2"] {
            layout.attach_tab(tab).unwrap();
            cache.sync_workspace("ws", &layout, &BTreeSet::new());
        }

        // Main pane keeps its own top 3 (m4, m3, m2); right pane keeps (r4, r3, r2).
        assert_eq!(
            cache.pane_lru("ws", MAIN_PANE_ID).unwrap(),
            &["m4", "m3", "m2"]
        );
        assert_eq!(
            cache.pane_lru("ws", &right_pane).unwrap(),
            &["r4", "r3", "r2"]
        );
        // Explorer pane has 4 total tabs (files, changes, e1, e2), so its top 3
        // mounted tabs are the 3 most recently active in explorer.
        assert_eq!(cache.pane_lru("ws", EXPLORER_PANE_ID).unwrap().len(), 3);
    }

    #[test]
    fn cross_pane_move_preserves_mounted_identity_and_dirty_state_without_eviction() {
        let mut cache = FileMountCache::new(3);
        let mut layout = SplitLayout::single(MAIN_PANE_ID);
        let mut ids = SplitIds::for_layout(&layout.root);
        let right_pane = layout
            .split_empty(MAIN_PANE_ID, SplitPosition::Right, &mut ids)
            .unwrap();

        let mut mounted = BTreeSet::new();
        let mut dirty = BTreeSet::new();
        dirty.insert("file:dirty".to_owned());

        layout.focus_pane(MAIN_PANE_ID);
        for tab in ["file:clean", "file:dirty", "file:third"] {
            layout.attach_tab(tab).unwrap();
            mounted.insert(tab.to_owned());
            let outcome = cache.sync_layouts([("ws", &layout)], &mounted, &dirty);
            assert!(outcome.evicted_tabs.is_empty());
            mounted = outcome.mounted_tabs;
        }

        // Move both clean and dirty tabs to the right pane. Neither may be evicted
        // during the move, so their FileEditor entity identity and dirty state survive.
        assert!(layout.move_tab_to_pane("file:dirty", &right_pane));
        let outcome = cache.sync_layouts([("ws", &layout)], &mounted, &dirty);
        assert!(outcome.evicted_tabs.is_empty());
        assert!(outcome.mounted_tabs.contains("file:dirty"));
        mounted = outcome.mounted_tabs;

        assert!(layout.move_tab_to_pane("file:clean", &right_pane));
        let outcome = cache.sync_layouts([("ws", &layout)], &mounted, &dirty);
        assert!(outcome.evicted_tabs.is_empty());
        assert!(outcome.mounted_tabs.contains("file:clean"));
        assert!(outcome.mounted_tabs.contains("file:dirty"));
    }

    #[test]
    fn closing_a_pane_transfers_tabs_to_remaining_pane_and_evicts_excess_clean_tabs() {
        let mut cache = FileMountCache::new(3);
        let mut layout = SplitLayout::single(MAIN_PANE_ID);
        let mut ids = SplitIds::for_layout(&layout.root);
        let right_pane = layout
            .split_empty(MAIN_PANE_ID, SplitPosition::Right, &mut ids)
            .unwrap();

        let mut mounted = BTreeSet::new();
        let mut dirty = BTreeSet::new();
        dirty.insert("file:dirty".to_owned());

        layout.focus_pane(MAIN_PANE_ID);
        for tab in ["file:dirty", "file:m1", "file:m2"] {
            layout.attach_tab(tab).unwrap();
            mounted.insert(tab.to_owned());
            mounted = cache
                .sync_layouts([("ws", &layout)], &mounted, &dirty)
                .mounted_tabs;
        }

        layout.focus_pane(&right_pane);
        for tab in ["file:r1", "file:r2"] {
            layout.attach_tab(tab).unwrap();
            mounted.insert(tab.to_owned());
            mounted = cache
                .sync_layouts([("ws", &layout)], &mounted, &dirty)
                .mounted_tabs;
        }
        assert_eq!(mounted.len(), 5);

        // Simulate `close_focused_pane`: close right_pane and re-attach its tabs to main.
        let transferred = layout.pane(&right_pane).unwrap().tabs.clone();
        assert!(layout.close_pane(&right_pane));
        for tab in transferred {
            layout.attach_tab(&tab).unwrap();
        }

        let outcome = cache.sync_layouts([("ws", &layout)], &mounted, &dirty);
        // Active tab is "file:r2", dirty tab is "file:dirty", plus 1 previous LRU tab ("file:m2") -> cap=3.
        // Clean tabs "file:m1" and "file:r1" are evicted; "file:dirty" is never evicted.
        assert!(outcome.mounted_tabs.contains("file:r2"));
        assert!(outcome.mounted_tabs.contains("file:dirty"));
        assert_eq!(outcome.mounted_tabs.len(), 3);
        assert!(!outcome.evicted_tabs.contains(&"file:dirty".to_owned()));
        assert_eq!(
            outcome.evicted_tabs,
            vec!["file:m1".to_owned(), "file:r1".to_owned()]
        );
    }

    #[test]
    fn mixed_non_file_and_file_tabs_share_the_pane_lru_budget_and_evict_stale_clean_files() {
        // In Web WorkspacePanelHost, allTabIds includes every tab in the pane
        // (terminals, agents, files, browsers). Switching through 3 terminal/agent
        // tabs in the same pane pushes a clean file tab out of the cap=3 window,
        // while a dirty file tab remains mounted.
        let mut cache = FileMountCache::new(3);
        let mut layout = SplitLayout::single(MAIN_PANE_ID);
        let mut mounted = BTreeSet::new();
        let mut dirty = BTreeSet::new();
        dirty.insert("file:dirty".to_owned());

        for tab in ["file:clean", "file:dirty", "term-1", "term-2", "agent-1"] {
            layout.attach_tab(tab).unwrap();
            if tab.starts_with("file:") {
                mounted.insert(tab.to_owned());
            }
            let outcome = cache.sync_layouts([("ws", &layout)], &mounted, &dirty);
            for evicted in &outcome.evicted_tabs {
                mounted.remove(evicted);
            }
        }

        // The pane's LRU is ["agent-1", "file:dirty", "term-2"] (cap=3).
        // "file:clean" fell out of the top 3 and was evicted; "file:dirty" stayed.
        assert_eq!(
            cache.pane_lru("ws", MAIN_PANE_ID).unwrap(),
            &["agent-1", "file:dirty", "term-2"]
        );
        assert!(!mounted.contains("file:clean"));
        assert!(mounted.contains("file:dirty"));
    }

    #[test]
    fn evicted_clean_tab_remounts_fresh_when_selected_again() {
        let mut cache = FileMountCache::new(3);
        let mut layout = SplitLayout::single(MAIN_PANE_ID);
        let mut mounted = BTreeSet::new();
        let clean = BTreeSet::new();

        for tab in ["file:a", "file:b", "file:c", "file:d"] {
            layout.attach_tab(tab).unwrap();
            mounted.insert(tab.to_owned());
            let outcome = cache.sync_layouts([("ws", &layout)], &mounted, &clean);
            for evicted in &outcome.evicted_tabs {
                mounted.remove(evicted);
            }
        }
        assert!(!mounted.contains("file:a"));
        assert_eq!(
            cache.pane_lru("ws", MAIN_PANE_ID).unwrap(),
            &["file:d", "file:c", "file:b"]
        );

        // Selecting "file:a" again promotes it to the front and evicts "file:b".
        layout.set_focused_tab(MAIN_PANE_ID, "file:a").unwrap();
        mounted.insert("file:a".to_owned());
        let outcome = cache.sync_layouts([("ws", &layout)], &mounted, &clean);
        assert_eq!(outcome.evicted_tabs, vec!["file:b".to_owned()]);
        assert_eq!(
            cache.pane_lru("ws", MAIN_PANE_ID).unwrap(),
            &["file:a", "file:d", "file:c"]
        );
    }

    #[test]
    fn file_tab_key_validates_canonical_identity_and_workspace_scope() {
        let key = FileTabKey::new("/repos/a", "src/main.rs");
        let tab_id = key.tab_id();
        assert_eq!(key.title(), "main.rs");
        assert_eq!(
            FileTabKey::from_tab_id(&tab_id, "/repos/a"),
            Some(key.clone())
        );
        // Foreign workspace cwd is rejected.
        assert_eq!(FileTabKey::from_tab_id(&tab_id, "/repos/b"), None);

        // Malformed or non-canonical tab ids are rejected.
        for malformed in [
            "",
            "file:",
            "file:[]",
            "file:[\"/repos/a\"]",
            "file:[\"/repos/a\",\"\"]",
            "file:[\"\",\"src/main.rs\"]",
            "file:[ \"/repos/a\", \"src/main.rs\" ]",
            "term-1",
        ] {
            assert_eq!(
                FileTabKey::from_tab_id(malformed, "/repos/a"),
                None,
                "accepted {malformed:?}"
            );
        }
    }

    #[test]
    fn cold_restore_mounts_only_focused_and_retained_file_tabs_instead_of_every_persisted_tab() {
        let cwd = "/repos/a";
        let f1 = FileTabKey::new(cwd, "a.rs");
        let f2 = FileTabKey::new(cwd, "b.rs");
        let f3 = FileTabKey::new(cwd, "c.rs");
        let f4 = FileTabKey::new(cwd, "d.rs");
        let foreign = FileTabKey::new("/repos/other", "secret.rs");

        let mut layout = SplitLayout::single(MAIN_PANE_ID);
        for key in [&f1, &f2, &f3, &f4, &foreign] {
            layout.attach_tab(&key.tab_id()).unwrap();
        }
        // Focus f3 in the main pane.
        layout.set_focused_tab(MAIN_PANE_ID, &f3.tab_id()).unwrap();

        // All restorable tabs in this workspace (excluding foreign):
        assert_eq!(
            restorable_file_tabs(&layout, cwd),
            vec![f1.clone(), f2.clone(), f3.clone(), f4.clone()]
        );

        // On cold restore with no prior LRU history and f1 retained (dirty),
        // only the active tab (f3) and retained tab (f1) are mounted initially.
        let mut cache = FileMountCache::new(3);
        let mut retained = BTreeSet::new();
        retained.insert(f1.tab_id());
        let initial_mounted =
            mounted_restorable_file_tabs(&mut cache, "ws", &layout, cwd, &retained);
        assert_eq!(initial_mounted, vec![f1, f3]);
    }

    #[test]
    fn visible_file_tabs_respects_rendered_panes_maximized_state_and_window_visibility() {
        let cwd = "/repos/a";
        let main_bg = FileTabKey::new(cwd, "bg.rs");
        let main_active = FileTabKey::new(cwd, "main.rs");
        let right_active = FileTabKey::new(cwd, "right.rs");
        let explorer_active = FileTabKey::new(cwd, "explorer.rs");

        let mut layout = SplitLayout::single(MAIN_PANE_ID);
        let mut ids = SplitIds::for_layout(&layout.root);
        let right_pane = layout
            .split_empty(MAIN_PANE_ID, SplitPosition::Right, &mut ids)
            .unwrap();
        layout.explorer.ensure_pane();
        layout.explorer.visible = true;

        layout.focus_pane(MAIN_PANE_ID);
        layout.attach_tab(&main_bg.tab_id()).unwrap();
        layout.attach_tab(&main_active.tab_id()).unwrap();

        layout.focus_pane(&right_pane);
        layout.attach_tab(&right_active.tab_id()).unwrap();

        layout.focus_pane(EXPLORER_PANE_ID);
        layout.attach_tab(&explorer_active.tab_id()).unwrap();

        // All 3 panes are rendered; only their focused tabs are visible.
        let visible = visible_file_tabs(&layout, cwd, true);
        assert_eq!(
            visible,
            BTreeSet::from([
                main_active.clone(),
                right_active.clone(),
                explorer_active.clone(),
            ])
        );

        // Hiding Explorer removes its focused file tab from visible observation.
        layout.explorer.visible = false;
        let visible = visible_file_tabs(&layout, cwd, true);
        assert_eq!(
            visible,
            BTreeSet::from([main_active.clone(), right_active.clone()])
        );

        // Maximizing the main pane hides the right split pane from presentation.
        assert!(layout.toggle_pane_maximized(MAIN_PANE_ID));
        let visible = visible_file_tabs(&layout, cwd, true);
        assert_eq!(visible, BTreeSet::from([main_active]));

        // When the window is hidden or occluded by a full-window pane, no file tab is visible.
        assert!(visible_file_tabs(&layout, cwd, false).is_empty());
    }

    #[test]
    fn sync_workspace_with_evictions_and_remove_workspace_isolate_workspace_state() {
        let mut cache = FileMountCache::new(3);
        let mut layout_a = SplitLayout::single(MAIN_PANE_ID);
        let mut layout_b = SplitLayout::single(MAIN_PANE_ID);

        let mut mounted_a = BTreeSet::new();
        for tab in ["a1", "a2", "a3", "a4"] {
            layout_a.attach_tab(tab).unwrap();
            mounted_a.insert(tab.to_owned());
            let outcome = cache.sync_workspace_with_evictions(
                "ws-a",
                &layout_a,
                &mounted_a,
                &BTreeSet::new(),
            );
            for evicted in &outcome.evicted_tabs {
                mounted_a.remove(evicted);
            }
        }
        assert_eq!(
            mounted_a,
            BTreeSet::from(["a2".to_owned(), "a3".to_owned(), "a4".to_owned()])
        );

        layout_b.attach_tab("b1").unwrap();
        cache.sync_workspace("ws-b", &layout_b, &BTreeSet::new());
        assert!(cache.pane_lru("ws-b", MAIN_PANE_ID).is_some());

        // Removing ws-a clears only ws-a's pane LRU state.
        cache.remove_workspace("ws-a");
        assert!(cache.pane_lru("ws-a", MAIN_PANE_ID).is_none());
        assert_eq!(cache.pane_lru("ws-b", MAIN_PANE_ID).unwrap(), &["b1"]);
    }

    #[test]
    fn switching_workspaces_keeps_other_retained_workspaces_clean_buffers_within_per_pane_cap() {
        let mut cache = FileMountCache::new(3);
        let mut layout_a = SplitLayout::single(MAIN_PANE_ID);
        let mut layout_b = SplitLayout::single(MAIN_PANE_ID);
        let mut mounted = BTreeSet::new();
        let mut dirty = BTreeSet::new();

        for tab in ["ws-a:1", "ws-a:2", "ws-a:3", "ws-a:4"] {
            layout_a.attach_tab(tab).unwrap();
            mounted.insert(tab.to_owned());
            let outcome = cache.sync_layouts([("ws-a", &layout_a)], &mounted, &dirty);
            for evicted in &outcome.evicted_tabs {
                mounted.remove(evicted);
            }
        }
        assert_eq!(
            mounted,
            BTreeSet::from([
                "ws-a:2".to_owned(),
                "ws-a:3".to_owned(),
                "ws-a:4".to_owned()
            ])
        );

        // Switching to ws-b and opening 4 clean tabs + 1 dirty tab in ws-b must
        // keep ws-a's 3 clean LRU tabs mounted while capping ws-b's clean tabs
        // at 3 alongside its dirty tab.
        dirty.insert("ws-b:dirty".to_owned());
        for tab in ["ws-b:dirty", "ws-b:1", "ws-b:2", "ws-b:3", "ws-b:4"] {
            layout_b.attach_tab(tab).unwrap();
            mounted.insert(tab.to_owned());
            let outcome =
                cache.sync_layouts([("ws-a", &layout_a), ("ws-b", &layout_b)], &mounted, &dirty);
            for evicted in &outcome.evicted_tabs {
                mounted.remove(evicted);
            }
        }

        assert_eq!(
            cache.pane_lru("ws-a", MAIN_PANE_ID).unwrap(),
            &["ws-a:4", "ws-a:3", "ws-a:2"]
        );
        assert_eq!(
            cache.pane_lru("ws-b", MAIN_PANE_ID).unwrap(),
            &["ws-b:4", "ws-b:dirty", "ws-b:3"]
        );
        assert_eq!(
            mounted,
            BTreeSet::from([
                "ws-a:2".to_owned(),
                "ws-a:3".to_owned(),
                "ws-a:4".to_owned(),
                "ws-b:dirty".to_owned(),
                "ws-b:3".to_owned(),
                "ws-b:4".to_owned(),
            ])
        );
    }
}

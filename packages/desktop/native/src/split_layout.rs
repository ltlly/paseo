//! Workspace split pane tree.
//!
//! Model port of the original app's workspace split layout
//! (`packages/app/src/stores/workspace-layout-actions.ts`, sizes and pane
//! semantics) and its spatial focus traversal
//! (`packages/app/src/utils/split-navigation.ts`). The native client keeps one
//! layout per workspace for the session; terminals stay open when a pane
//! closes, matching the daemon-owned terminal lifecycle.

use serde::{Deserialize, Serialize};

/// Matches `MIN_SPLIT_SIZE` in `packages/app/src/stores/workspace-layout-constants.ts`.
pub const MIN_SPLIT_SIZE: f64 = 0.1;
/// Matches `MAX_TREE_DEPTH` in `packages/app/src/stores/workspace-layout-store.ts`.
pub const MAX_TREE_DEPTH: usize = 5;
/// Upper bound on panes in one workspace. The renderer keeps one terminal view
/// per pane, so the live model and restore validation share this ceiling.
pub const MAX_PANES: usize = 64;
/// Upper bound on tabs one pane owns. The live model refuses the operation and
/// restore validation drops the surplus, so both sides share one ceiling and a
/// save never truncates a layout the window accepted.
pub const MAX_TABS_PER_PANE: usize = 256;
pub const MAIN_PANE_ID: &str = "main";

const FLOAT_TOLERANCE: f64 = 0.000_001;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SplitDirection {
    Horizontal,
    Vertical,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitPosition {
    Left,
    Right,
    Top,
    Bottom,
}

impl SplitPosition {
    fn direction(self) -> SplitDirection {
        match self {
            SplitPosition::Left | SplitPosition::Right => SplitDirection::Horizontal,
            SplitPosition::Top | SplitPosition::Bottom => SplitDirection::Vertical,
        }
    }

    fn after(self) -> bool {
        matches!(self, SplitPosition::Right | SplitPosition::Bottom)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusDirection {
    Left,
    Right,
    Up,
    Down,
}

/// A leaf pane. `tabs` are workspace tab ids the pane owns; in the native
/// client these are terminal ids. A pane with no tabs shows a placeholder.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SplitPane {
    pub id: String,
    #[serde(default)]
    pub tabs: Vec<String>,
    #[serde(default)]
    pub focused_tab: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SplitGroup {
    pub id: String,
    pub direction: SplitDirection,
    pub children: Vec<SplitNode>,
    pub sizes: Vec<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum SplitNode {
    Pane { pane: SplitPane },
    Group { group: SplitGroup },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SplitLayout {
    pub root: SplitNode,
    #[serde(default)]
    pub focused_pane_id: Option<String>,
}

/// Generates persisted pane/group ids the same way the original app's
/// `defaultWorkspaceLayoutIds` does: `pane-N` and `group-N` counters that never
/// collide with the ids already present in a restored layout.
///
/// A restored layout can carry `pane-18446744073709551615`, so the counter is
/// exhausted rather than wrapping: `create` returns `None` and the operation
/// that needed the id is refused.
#[derive(Debug)]
pub struct SplitIds {
    next: u64,
}

impl Default for SplitIds {
    fn default() -> Self {
        SplitIds { next: 1 }
    }
}

impl SplitIds {
    pub fn for_layout(node: &SplitNode) -> Self {
        let mut next = 1;
        scan_ids(node, &mut next);
        SplitIds { next }
    }

    pub fn create(&mut self, prefix: &str) -> Option<String> {
        let next = self.next.checked_add(1)?;
        let id = format!("{prefix}-{}", self.next);
        self.next = next;
        Some(id)
    }
}

fn scan_ids(node: &SplitNode, next: &mut u64) {
    match node {
        SplitNode::Pane { pane } => consider_id(&pane.id, next),
        SplitNode::Group { group } => {
            consider_id(&group.id, next);
            for child in &group.children {
                scan_ids(child, next);
            }
        }
    }
}

fn consider_id(id: &str, next: &mut u64) {
    if let Some((_, suffix)) = id.rsplit_once('-')
        && let Ok(value) = suffix.parse::<u64>()
    {
        // Saturating keeps a restored maximum id from overflowing here; the
        // exhausted counter then refuses the next split in `create`.
        *next = (*next).max(value.saturating_add(1));
    }
}

pub fn normalize_sizes(sizes: &[f64], count: usize) -> Vec<f64> {
    if count == 0 {
        return Vec::new();
    }
    let mut raw: Vec<f64> = sizes.iter().take(count).copied().collect();
    while raw.len() < count {
        raw.push(1.0);
    }
    let sanitized: Vec<f64> = raw
        .iter()
        .map(|value| {
            if value.is_finite() && *value > 0.0 {
                *value
            } else {
                1.0
            }
        })
        .collect();
    let total: f64 = sanitized.iter().sum();
    if total <= 0.0 {
        return vec![1.0 / count as f64; count];
    }
    sanitized.iter().map(|value| value / total).collect()
}

/// Ports `clampNormalizedSizes`: rescale, then lock any child below
/// `MIN_SPLIT_SIZE` and redistribute the remainder among the rest.
pub fn clamp_normalized_sizes(sizes: &[f64]) -> Vec<f64> {
    if sizes.is_empty() {
        return Vec::new();
    }
    let count = sizes.len();
    let normalized = normalize_sizes(sizes, count);
    if count == 1 {
        return vec![1.0];
    }
    if count as f64 * MIN_SPLIT_SIZE > 1.0 {
        return vec![1.0 / count as f64; count];
    }

    let mut next_sizes = vec![0.0; count];
    let mut unlocked: Vec<usize> = (0..count).collect();
    let mut remaining_total = 1.0;

    while !unlocked.is_empty() {
        let unlocked_weight: f64 = unlocked.iter().map(|&index| normalized[index]).sum();
        if unlocked_weight <= 0.0 {
            let even_share = remaining_total / unlocked.len() as f64;
            for &index in &unlocked {
                next_sizes[index] = even_share;
            }
            break;
        }

        let locked: Vec<usize> = unlocked
            .iter()
            .copied()
            .filter(|&index| normalized[index] / unlocked_weight * remaining_total < MIN_SPLIT_SIZE)
            .collect();

        if locked.is_empty() {
            for &index in &unlocked {
                next_sizes[index] = normalized[index] / unlocked_weight * remaining_total;
            }
            break;
        }

        for &index in &locked {
            next_sizes[index] = MIN_SPLIT_SIZE;
        }
        unlocked.retain(|index| !locked.contains(index));
        remaining_total -= MIN_SPLIT_SIZE * locked.len() as f64;
    }

    normalize_sizes(&next_sizes, count)
}

impl SplitLayout {
    pub fn single(pane_id: &str) -> Self {
        SplitLayout {
            root: SplitNode::Pane {
                pane: SplitPane {
                    id: pane_id.to_owned(),
                    ..Default::default()
                },
            },
            focused_pane_id: Some(pane_id.to_owned()),
        }
    }

    pub fn panes(&self) -> Vec<SplitPane> {
        let mut panes = Vec::new();
        collect_panes(&self.root, &mut panes);
        panes
    }

    pub fn pane(&self, pane_id: &str) -> Option<&SplitPane> {
        fn walk<'a>(node: &'a SplitNode, pane_id: &str) -> Option<&'a SplitPane> {
            match node {
                SplitNode::Pane { pane } => (pane.id == pane_id).then_some(pane),
                SplitNode::Group { group } => {
                    group.children.iter().find_map(|child| walk(child, pane_id))
                }
            }
        }
        walk(&self.root, pane_id)
    }

    pub fn focused_pane(&self) -> Option<&SplitPane> {
        let id = self.focused_pane_id.clone()?;
        self.pane(&id)
    }

    pub fn depth(&self) -> usize {
        tree_depth(&self.root)
    }

    pub fn find_pane_containing_tab(&self, tab_id: &str) -> Option<SplitPane> {
        self.panes()
            .into_iter()
            .find(|pane| pane.tabs.iter().any(|tab| tab == tab_id))
    }

    /// Ports `splitPaneEmptyInLayout`: inserts an empty placeholder pane beside
    /// `target_pane_id`, reusing the parent group when the direction matches,
    /// splitting half its size. Focuses the new pane. Returns its id, or None
    /// when the target is missing or the tree would exceed `MAX_TREE_DEPTH`.
    pub fn split_empty(
        &mut self,
        target_pane_id: &str,
        position: SplitPosition,
        ids: &mut SplitIds,
    ) -> Option<String> {
        let mut candidate = self.clone();
        let direction = position.direction();
        let after = position.after();

        let path = find_pane_path(&candidate.root, target_pane_id)?;
        // An empty path is the root pane itself, which has no parent group; any
        // deeper pane's parent is `node_at_path` of the path without its last
        // index, and the root group lives at the empty path.
        let (parent_path, last) = path.split_at(path.len().saturating_sub(1));
        let target_index = last.last().copied().unwrap_or(0);

        let parent = if path.is_empty() {
            None
        } else {
            Some(node_at_path(&candidate.root, parent_path))
        };
        // A reused parent group already contains the target pane, so the whole
        // group replaces the parent. Only a fresh wrapper group replaces the
        // leaf: replacing the leaf with its own ancestor would duplicate every
        // id and tag in that group.
        let (replace_path, new_pane_id, inserted) = match parent {
            Some(SplitNode::Group { group })
                if group.direction == direction && group.children.len() == group.sizes.len() =>
            {
                let new_pane_id = ids.create("pane")?;
                let mut group = group;
                let target_size = group.sizes.get(target_index).copied().unwrap_or(0.0);
                let insert_index = if after {
                    target_index + 1
                } else {
                    target_index
                };
                group.children.insert(
                    insert_index,
                    SplitNode::Pane {
                        pane: SplitPane {
                            id: new_pane_id.clone(),
                            ..Default::default()
                        },
                    },
                );
                group.sizes.insert(insert_index, target_size / 2.0);
                let adjusted = if after {
                    target_index
                } else {
                    target_index + 1
                };
                if let Some(size) = group.sizes.get_mut(adjusted) {
                    *size = target_size / 2.0;
                }
                group.sizes = normalize_sizes(&group.sizes, group.children.len());
                (
                    parent_path.to_vec(),
                    new_pane_id,
                    SplitNode::Group { group },
                )
            }
            _ => {
                let new_pane_id = ids.create("pane")?;
                let new_pane = SplitNode::Pane {
                    pane: SplitPane {
                        id: new_pane_id.clone(),
                        ..Default::default()
                    },
                };
                let target_node = node_at_path(&candidate.root, &path);
                let children = if after {
                    vec![target_node, new_pane]
                } else {
                    vec![new_pane, target_node]
                };
                (
                    path.clone(),
                    new_pane_id,
                    SplitNode::Group {
                        group: SplitGroup {
                            id: ids.create("group")?,
                            direction,
                            children,
                            sizes: vec![0.5, 0.5],
                        },
                    },
                )
            }
        };

        candidate.root = replace_node_at_path(&candidate.root, &replace_path, inserted);
        if candidate.depth() > MAX_TREE_DEPTH || candidate.panes().len() > MAX_PANES {
            return None;
        }
        candidate.focused_pane_id = Some(new_pane_id.clone());
        *self = candidate;
        Some(new_pane_id)
    }

    /// Ports `closePaneInLayout` for the native client: removes a pane and its
    /// tab ownership. Terminal teardown belongs to the caller — a closed pane
    /// never closes PTYs. Returns false for the last remaining pane.
    pub fn close_pane(&mut self, pane_id: &str) -> bool {
        if self.panes().len() <= 1 {
            return false;
        }
        let Some(path) = find_pane_path(&self.root, pane_id) else {
            return false;
        };
        let fallback = nearest_sibling_pane(&self.root, pane_id);
        self.root = remove_pane_by_path(&self.root, &path);
        if self.focused_pane_id.as_deref() == Some(pane_id) {
            self.focused_pane_id =
                fallback.or_else(|| self.panes().first().map(|pane| pane.id.clone()));
        }
        true
    }

    pub fn focus_pane(&mut self, pane_id: &str) -> bool {
        if self.pane(pane_id).is_none() {
            return false;
        }
        if self.focused_pane_id.as_deref() == Some(pane_id) {
            return false;
        }
        self.focused_pane_id = Some(pane_id.to_owned());
        true
    }

    /// Ports `resizeSplitInLayout` + `clampNormalizedSizes`.
    pub fn resize_group(&mut self, group_id: &str, sizes: Vec<f64>) -> bool {
        let mut candidate = self.clone();
        if !resize_group_in_node(&mut candidate.root, group_id, sizes) {
            return false;
        }
        *self = candidate;
        true
    }

    pub fn group_sizes(&self, group_id: &str) -> Option<Vec<f64>> {
        fn walk(node: &SplitNode, id: &str) -> Option<Vec<f64>> {
            match node {
                SplitNode::Pane { .. } => None,
                SplitNode::Group { group } => {
                    if group.id == id {
                        Some(group.sizes.clone())
                    } else {
                        group.children.iter().find_map(|child| walk(child, id))
                    }
                }
            }
        }
        walk(&self.root, group_id)
    }

    /// Shows a tab in the pane that already owns it; only a tab no pane owns
    /// lands in the focused pane. This is the app's `revealExistingTab`: a
    /// sidebar click or terminal restore never relocates a tab the user placed.
    /// Explicit relocation is `move_tab_to_pane`. Returns the owning pane id, or
    /// `None` when the focused pane already owns `MAX_TABS_PER_PANE` tabs; the
    /// save path cannot store more, so the model refuses instead of dropping a
    /// tab on the next write.
    pub fn attach_tab(&mut self, tab_id: &str) -> Option<String> {
        if let Some(current) = self.find_pane_containing_tab(tab_id) {
            self.focused_pane_id = Some(current.id.clone());
            self.set_focused_tab(&current.id, tab_id);
            return Some(current.id);
        }
        let target = self
            .focused_pane()
            .cloned()
            .or_else(|| self.panes().into_iter().next())?;
        if target.tabs.len() >= MAX_TABS_PER_PANE {
            return None;
        }
        let pane_id = target.id.clone();
        let mut pane = target;
        pane.tabs.push(tab_id.to_owned());
        pane.focused_tab = Some(tab_id.to_owned());
        self.replace_pane(pane);
        self.focused_pane_id = Some(pane_id.clone());
        Some(pane_id)
    }

    /// Adds a tab the layout does not own yet to the pane that already hosts
    /// tabs, without changing pane focus. Used when a workspace's terminals
    /// appear after the layout was restored: every terminal stays reachable
    /// as a tab, but restoring does not yank focus to the last one.
    pub fn adopt_tab(&mut self, tab_id: &str) -> bool {
        if self.find_pane_containing_tab(tab_id).is_some() {
            return false;
        }
        let target = self
            .panes()
            .into_iter()
            .find(|pane| !pane.tabs.is_empty())
            .map(|pane| pane.id)
            .or_else(|| self.focused_pane_id.clone())
            .or_else(|| self.panes().first().map(|pane| pane.id.clone()));
        let Some(pane_id) = target else {
            return false;
        };
        let Some(mut pane) = self.pane(&pane_id).cloned() else {
            return false;
        };
        if pane.tabs.len() >= MAX_TABS_PER_PANE {
            return false;
        }
        pane.tabs.push(tab_id.to_owned());
        if pane.focused_tab.is_none() {
            pane.focused_tab = Some(tab_id.to_owned());
        }
        self.replace_pane(pane);
        true
    }

    pub fn set_focused_tab(&mut self, pane_id: &str, tab_id: &str) -> Option<()> {
        let mut pane = self.pane(pane_id)?.clone();
        if !pane.tabs.iter().any(|tab| tab == tab_id) {
            return None;
        }
        pane.focused_tab = Some(tab_id.to_owned());
        self.replace_pane(pane);
        Some(())
    }

    /// True when `tab_id` already is the visible tab of the focused pane.
    pub fn tab_is_focused(&self, tab_id: &str) -> bool {
        self.focused_pane().is_some_and(|pane| {
            pane.focused_tab.as_deref() == Some(tab_id) && pane.tabs.iter().any(|tab| tab == tab_id)
        })
    }

    /// Ports `moveTabToPaneInLayout`: a tab lives in exactly one pane. The
    /// source pane loses it and closes when that was its last tab; the main
    /// pane stays as an empty split target, like a pane that lost its tabs in
    /// the native client's preserved default pane. The target pane becomes
    /// focused and shows the moved tab.
    pub fn move_tab_to_pane(&mut self, tab_id: &str, to_pane_id: &str) -> bool {
        let Some(source) = self.find_pane_containing_tab(tab_id) else {
            return false;
        };
        if source.id == to_pane_id {
            if source.focused_tab.as_deref() == Some(tab_id)
                && self.focused_pane_id.as_deref() == Some(to_pane_id)
            {
                return false;
            }
            self.focused_pane_id = Some(to_pane_id.to_owned());
            self.set_focused_tab(to_pane_id, tab_id);
            return true;
        }
        let Some(target) = self.pane(to_pane_id).cloned() else {
            return false;
        };
        if !target.tabs.iter().any(|tab| tab == tab_id) && target.tabs.len() >= MAX_TABS_PER_PANE {
            // Refused before the source pane loses the tab, so a failed move
            // leaves both panes exactly as they were.
            return false;
        }

        let mut source_pane = source;
        source_pane.tabs.retain(|tab| tab != tab_id);
        if source_pane.focused_tab.as_deref() == Some(tab_id) {
            source_pane.focused_tab = source_pane.tabs.last().cloned();
        }
        let source_id = source_pane.id.clone();
        let source_is_main = source_id == MAIN_PANE_ID;
        self.replace_pane(source_pane);
        if !source_is_main && self.panes().len() > 1 {
            let emptied = self
                .pane(&source_id)
                .is_some_and(|pane| pane.tabs.is_empty());
            if emptied {
                self.close_pane(&source_id);
            }
        }

        let mut target_pane = target;
        if !target_pane.tabs.iter().any(|tab| tab == tab_id) {
            target_pane.tabs.push(tab_id.to_owned());
        }
        target_pane.focused_tab = Some(tab_id.to_owned());
        self.replace_pane(target_pane);
        self.focused_pane_id = Some(to_pane_id.to_owned());
        true
    }

    /// Focuses the pane that owns `tab_id`, if any. Returns the pane id.
    pub fn focus_tab(&mut self, tab_id: &str) -> Option<String> {
        let pane = self.find_pane_containing_tab(tab_id)?;
        self.focus_pane(&pane.id);
        self.set_focused_tab(&pane.id, tab_id);
        Some(pane.id)
    }

    /// Forgets tabs that no longer exist. Panes that lose their last tab are
    /// removed unless they are the last pane or the main workspace pane, which
    /// survives as an empty split target like the app's preserved default pane.
    pub fn prune_tabs(&mut self, mut keep: impl FnMut(&str) -> bool) {
        let panes = self.panes();
        for pane in panes {
            let focused_alive = pane.focused_tab.as_deref().map_or(true, |tab| keep(tab));
            if pane.tabs.iter().all(|tab| keep(tab)) && focused_alive {
                continue;
            }
            let tabs: Vec<String> = pane.tabs.iter().filter(|tab| keep(tab)).cloned().collect();
            if tabs.is_empty() && pane.id != MAIN_PANE_ID && self.panes().len() > 1 {
                self.close_pane(&pane.id);
            } else {
                let focused_tab = tabs
                    .iter()
                    .find(|tab| Some(tab.as_str()) == pane.focused_tab.as_deref())
                    .or_else(|| tabs.last())
                    .cloned();
                self.replace_pane(SplitPane {
                    id: pane.id.clone(),
                    tabs,
                    focused_tab,
                });
            }
        }
    }

    /// Ports `findAdjacentPane` from `packages/app/src/utils/split-navigation.ts`.
    pub fn find_adjacent_pane(&self, pane_id: &str, direction: FocusDirection) -> Option<String> {
        let panes = collect_pane_bounds(&self.root, (0.0, 0.0, 1.0, 1.0));
        let focused = panes.iter().find(|pane| pane.id == pane_id)?;
        let mut candidates: Vec<PaneCandidate> = panes
            .iter()
            .filter(|pane| pane.id != pane_id)
            .filter_map(|pane| build_candidate(pane, focused, direction))
            .collect();
        candidates.sort_by(compare_candidates);
        candidates.first().map(|candidate| candidate.id.clone())
    }

    /// The size in pixels this group spans along its own axis, derived from the
    /// rendered split area. Separator drags convert pixel deltas with this.
    pub fn group_size_px(&self, group_id: &str, area: (f64, f64, f64, f64)) -> Option<f64> {
        match &self.root {
            SplitNode::Pane { .. } => None,
            SplitNode::Group { group } => group_size_px_in(group, group_id, area),
        }
    }

    fn replace_pane(&mut self, pane: SplitPane) {
        replace_pane_in_node(&mut self.root, pane);
    }
}

fn group_size_px_in(group: &SplitGroup, group_id: &str, area: (f64, f64, f64, f64)) -> Option<f64> {
    let (left, top, right, bottom) = area;
    let span = if group.direction == SplitDirection::Horizontal {
        right - left
    } else {
        bottom - top
    };
    if group.id == group_id {
        return Some(span);
    }
    let mut offset = 0.0;
    for (child, size) in group.children.iter().zip(group.sizes.iter()) {
        let child_area = if group.direction == SplitDirection::Horizontal {
            let child_left = left + (right - left) * offset;
            offset += size;
            (child_left, top, left + (right - left) * offset, bottom)
        } else {
            let child_top = top + (bottom - top) * offset;
            offset += size;
            (left, child_top, right, top + (bottom - top) * offset)
        };
        if let SplitNode::Group { group: child_group } = child
            && let Some(found) = group_size_px_in(child_group, group_id, child_area)
        {
            return Some(found);
        }
    }
    None
}

fn resize_group_in_node(node: &mut SplitNode, group_id: &str, sizes: Vec<f64>) -> bool {
    match node {
        SplitNode::Pane { .. } => false,
        SplitNode::Group { group } => {
            if group.id == group_id {
                if group.children.len() != sizes.len() {
                    return false;
                }
                group.sizes = clamp_normalized_sizes(&sizes);
                return true;
            }
            let mut resized = false;
            for child in &mut group.children {
                resized |= resize_group_in_node(child, group_id, sizes.clone());
            }
            resized
        }
    }
}

fn replace_pane_in_node(node: &mut SplitNode, pane: SplitPane) {
    match node {
        SplitNode::Pane { pane: current } => {
            if current.id == pane.id {
                *current = pane;
            }
        }
        SplitNode::Group { group } => {
            for child in &mut group.children {
                replace_pane_in_node(child, pane.clone());
            }
        }
    }
}

fn collect_panes(node: &SplitNode, panes: &mut Vec<SplitPane>) {
    match node {
        SplitNode::Pane { pane } => panes.push(pane.clone()),
        SplitNode::Group { group } => {
            for child in &group.children {
                collect_panes(child, panes);
            }
        }
    }
}

fn tree_depth(node: &SplitNode) -> usize {
    match node {
        SplitNode::Pane { .. } => 1,
        SplitNode::Group { group } => {
            1 + group
                .children
                .iter()
                .map(tree_depth)
                .max()
                .unwrap_or_default()
        }
    }
}

fn find_pane_path(node: &SplitNode, pane_id: &str) -> Option<Vec<usize>> {
    match node {
        SplitNode::Pane { pane } => (pane.id == pane_id).then_some(Vec::new()),
        SplitNode::Group { group } => {
            group
                .children
                .iter()
                .enumerate()
                .find_map(|(index, child)| {
                    find_pane_path(child, pane_id).map(|mut path| {
                        path.insert(0, index);
                        path
                    })
                })
        }
    }
}

fn node_at_path(root: &SplitNode, path: &[usize]) -> SplitNode {
    let mut current = root.clone();
    for index in path {
        match current {
            SplitNode::Pane { .. } => unreachable!("split tree path crosses a pane"),
            SplitNode::Group { ref group } => {
                current = group.children[*index].clone();
            }
        }
    }
    current
}

fn replace_node_at_path(root: &SplitNode, path: &[usize], replacement: SplitNode) -> SplitNode {
    if path.is_empty() {
        return replacement;
    }
    match root {
        SplitNode::Pane { .. } => unreachable!("split tree path crosses a pane"),
        SplitNode::Group { group } => {
            let mut group = group.clone();
            if let Some(child) = group.children.get_mut(path[0]) {
                *child = replace_node_at_path(child, &path[1..], replacement);
            }
            group.sizes = normalize_sizes(&group.sizes, group.children.len());
            SplitNode::Group { group }
        }
    }
}

fn remove_pane_by_path(root: &SplitNode, path: &[usize]) -> SplitNode {
    debug_assert!(!path.is_empty(), "the last pane is never removed");
    if path.len() == 1 {
        if let SplitNode::Group { group } = root {
            let mut group = group.clone();
            group.children.remove(path[0]);
            if !group.sizes.is_empty() && group.sizes.len() > path[0] {
                group.sizes.remove(path[0]);
            }
            group.sizes = normalize_sizes(&group.sizes, group.children.len());
            if group.children.len() == 1 {
                return group.children.remove(0);
            }
            return SplitNode::Group { group };
        }
    }
    match root {
        SplitNode::Group { group } => {
            let mut group = group.clone();
            if let Some(child) = group.children.get_mut(path[0]) {
                *child = remove_pane_by_path(child, &path[1..]);
            }
            group.sizes = normalize_sizes(&group.sizes, group.children.len());
            SplitNode::Group { group }
        }
        SplitNode::Pane { .. } => unreachable!("split tree path crosses a pane"),
    }
}

/// Ports `findNearestSiblingPaneId`: the closest pane walking outward through
/// ancestor groups, preferring the previous sibling side.
fn nearest_sibling_pane(root: &SplitNode, pane_id: &str) -> Option<String> {
    let path = find_pane_path(root, pane_id)?;
    if path.is_empty() {
        return None;
    }
    for depth in (0..path.len()).rev() {
        let parent_path = &path[..depth];
        let child_index = path[depth];
        let parent = if parent_path.is_empty() {
            root.clone()
        } else {
            node_at_path(root, parent_path)
        };
        let SplitNode::Group { group } = parent else {
            continue;
        };
        let mut before = group.children[..child_index].to_vec();
        before.reverse();
        for sibling in before {
            let mut panes = Vec::new();
            collect_panes(&sibling, &mut panes);
            if let Some(pane) = panes.last() {
                return Some(pane.id.clone());
            }
        }
        for sibling in &group.children[child_index + 1..] {
            let mut panes = Vec::new();
            collect_panes(sibling, &mut panes);
            if let Some(pane) = panes.first() {
                return Some(pane.id.clone());
            }
        }
    }
    None
}

#[derive(Clone, Debug)]
struct PaneBounds {
    id: String,
    left: f64,
    top: f64,
    right: f64,
    bottom: f64,
}

impl PaneBounds {
    fn center_x(&self) -> f64 {
        (self.left + self.right) / 2.0
    }
    fn center_y(&self) -> f64 {
        (self.top + self.bottom) / 2.0
    }
}

#[derive(Clone, Debug)]
struct PaneCandidate {
    id: String,
    primary_distance: f64,
    secondary_distance: f64,
    center_distance: f64,
    overlap: f64,
}

fn collect_pane_bounds(node: &SplitNode, area: (f64, f64, f64, f64)) -> Vec<PaneBounds> {
    let (left, top, right, bottom) = area;
    match node {
        SplitNode::Pane { pane } => vec![PaneBounds {
            id: pane.id.clone(),
            left,
            top,
            right,
            bottom,
        }],
        SplitNode::Group { group } => {
            let mut panes = Vec::new();
            let total_width = right - left;
            let total_height = bottom - top;
            let mut offset = 0.0;
            for (child, size) in group.children.iter().zip(group.sizes.iter()) {
                if group.direction == SplitDirection::Horizontal {
                    let child_left = left + total_width * offset;
                    offset += size;
                    panes.extend(collect_pane_bounds(
                        child,
                        (child_left, top, left + total_width * offset, bottom),
                    ));
                } else {
                    let child_top = top + total_height * offset;
                    offset += size;
                    panes.extend(collect_pane_bounds(
                        child,
                        (left, child_top, right, top + total_height * offset),
                    ));
                }
            }
            panes
        }
    }
}

fn gap_length(start_a: f64, end_a: f64, start_b: f64, end_b: f64) -> f64 {
    if end_a < start_b {
        return start_b - end_a;
    }
    if end_b < start_a {
        return start_a - end_b;
    }
    0.0
}

fn overlap_length(start_a: f64, end_a: f64, start_b: f64, end_b: f64) -> f64 {
    (end_a.min(end_b) - start_a.max(start_b)).max(0.0)
}

fn build_candidate(
    pane: &PaneBounds,
    focused: &PaneBounds,
    direction: FocusDirection,
) -> Option<PaneCandidate> {
    let (primary_distance, secondary_distance, center_distance, overlap) = match direction {
        FocusDirection::Left => {
            let primary = focused.left - pane.right;
            (
                primary,
                gap_length(pane.top, pane.bottom, focused.top, focused.bottom),
                (pane.center_y() - focused.center_y()).abs(),
                overlap_length(pane.top, pane.bottom, focused.top, focused.bottom),
            )
        }
        FocusDirection::Right => {
            let primary = pane.left - focused.right;
            (
                primary,
                gap_length(pane.top, pane.bottom, focused.top, focused.bottom),
                (pane.center_y() - focused.center_y()).abs(),
                overlap_length(pane.top, pane.bottom, focused.top, focused.bottom),
            )
        }
        FocusDirection::Up => {
            let primary = focused.top - pane.bottom;
            (
                primary,
                gap_length(pane.left, pane.right, focused.left, focused.right),
                (pane.center_x() - focused.center_x()).abs(),
                overlap_length(pane.left, pane.right, focused.left, focused.right),
            )
        }
        FocusDirection::Down => {
            let primary = pane.top - focused.bottom;
            (
                primary,
                gap_length(pane.left, pane.right, focused.left, focused.right),
                (pane.center_x() - focused.center_x()).abs(),
                overlap_length(pane.left, pane.right, focused.left, focused.right),
            )
        }
    };
    if primary_distance < -FLOAT_TOLERANCE {
        return None;
    }
    Some(PaneCandidate {
        id: pane.id.clone(),
        primary_distance,
        secondary_distance,
        center_distance,
        overlap,
    })
}

fn compare_candidates(left: &PaneCandidate, right: &PaneCandidate) -> std::cmp::Ordering {
    left.primary_distance
        .partial_cmp(&right.primary_distance)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then(
            left.secondary_distance
                .partial_cmp(&right.secondary_distance)
                .unwrap_or(std::cmp::Ordering::Equal),
        )
        .then(
            right
                .overlap
                .partial_cmp(&left.overlap)
                .unwrap_or(std::cmp::Ordering::Equal),
        )
        .then(
            left.center_distance
                .partial_cmp(&right.center_distance)
                .unwrap_or(std::cmp::Ordering::Equal),
        )
        .then(left.id.cmp(&right.id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane_ids(layout: &SplitLayout) -> Vec<String> {
        layout.panes().iter().map(|pane| pane.id.clone()).collect()
    }

    #[test]
    fn split_right_then_down_halves_sizes_and_moves_focus() {
        let mut layout = SplitLayout::single(MAIN_PANE_ID);
        let mut ids = SplitIds::default();

        let right = layout.split_empty(MAIN_PANE_ID, SplitPosition::Right, &mut ids);
        assert_eq!(right, Some("pane-1".into()));
        assert_eq!(layout.focused_pane_id.as_deref(), Some("pane-1"));

        let down = layout.split_empty("pane-1", SplitPosition::Bottom, &mut ids);
        // Panes and groups share one counter, like one id source in the app.
        assert_eq!(down, Some("pane-3".into()));
        assert_eq!(pane_ids(&layout), vec![MAIN_PANE_ID, "pane-1", "pane-3"]);

        let SplitNode::Group { group } = &layout.root else {
            panic!("root must be a group after two splits");
        };
        assert_eq!(group.direction, SplitDirection::Horizontal);
        let SplitNode::Pane { pane: _ } = &group.children[0] else {
            panic!("first child must be the main pane");
        };
        let SplitNode::Group {
            group: vertical, ..
        } = &group.children[1]
        else {
            panic!("second child must be the split-down group");
        };
        assert_eq!(vertical.direction, SplitDirection::Vertical);
        assert!((group.sizes[0] - 0.5).abs() < 1e-9);
        assert!((group.sizes[1] - 0.5).abs() < 1e-9);
        assert!((vertical.sizes[0] - 0.5).abs() < 1e-9);
        assert!((vertical.sizes[1] - 0.5).abs() < 1e-9);
    }

    #[test]
    fn split_refuses_beyond_max_depth() {
        let mut layout = SplitLayout::single(MAIN_PANE_ID);
        let mut ids = SplitIds::default();
        let mut target = MAIN_PANE_ID.to_owned();
        // Alternating directions never match the parent group, so every split
        // nests one more level; splitting the same direction twice would reuse
        // the parent instead.
        let mut position = SplitPosition::Right;
        for _ in 1..MAX_TREE_DEPTH {
            target = layout
                .split_empty(&target, position, &mut ids)
                .expect("split within depth");
            position = match position {
                SplitPosition::Right => SplitPosition::Bottom,
                _ => SplitPosition::Right,
            };
        }
        assert_eq!(layout.depth(), MAX_TREE_DEPTH);
        assert_eq!(
            layout.split_empty(&target, position, &mut ids),
            None,
            "depth beyond the app limit must be refused"
        );
    }

    #[test]
    fn repeated_splits_beside_one_pane_keep_every_id_unique() {
        let mut layout = SplitLayout::single(MAIN_PANE_ID);
        let mut ids = SplitIds::default();
        let mut target = MAIN_PANE_ID.to_owned();
        // Each split targets the pane the previous one created, so the parent
        // group is reused every time and grows instead of nesting a copy of
        // itself (which duplicated every id in that group).
        for expected in ["pane-1", "pane-3", "pane-4"] {
            target = layout
                .split_empty(&target, SplitPosition::Right, &mut ids)
                .expect("split reuses the parent group");
            assert_eq!(target, expected);
        }
        assert_eq!(layout.panes().len(), 4);
        assert_eq!(layout.depth(), 2, "one horizontal group, no nested copy");

        let restored = crate::workspace_layout_storage::validate_layout(layout.clone())
            .expect("a live layout must be storable");
        assert_eq!(restored, layout);
        let SplitNode::Group { group } = &layout.root else {
            panic!("expected one group");
        };
        assert_eq!(group.children.len(), 4);
        assert!((group.sizes.iter().sum::<f64>() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn tab_limit_rejects_the_operation_instead_of_dropping_a_tab_later() {
        let mut layout = SplitLayout::single(MAIN_PANE_ID);
        for index in 0..MAX_TABS_PER_PANE {
            assert_eq!(
                layout.attach_tab(&format!("term-{index}")),
                Some(MAIN_PANE_ID.into())
            );
        }
        assert_eq!(layout.attach_tab("term-extra"), None);
        assert!(!layout.adopt_tab("term-extra"));
        assert_eq!(
            layout.pane(MAIN_PANE_ID).unwrap().tabs.len(),
            MAX_TABS_PER_PANE
        );

        // A move into a full pane leaves both panes as they were.
        let mut ids = SplitIds::default();
        let other = layout
            .split_empty(MAIN_PANE_ID, SplitPosition::Right, &mut ids)
            .expect("split within depth");
        assert_eq!(layout.attach_tab("term-other"), Some(other));
        let before = layout.clone();
        assert!(!layout.move_tab_to_pane("term-other", MAIN_PANE_ID));
        assert_eq!(layout, before);
    }

    #[test]
    fn a_restored_maximum_id_exhausts_the_counter_instead_of_overflowing() {
        let mut layout = SplitLayout {
            root: SplitNode::Group {
                group: SplitGroup {
                    id: "group-1".into(),
                    direction: SplitDirection::Horizontal,
                    children: vec![
                        SplitNode::Pane {
                            pane: SplitPane {
                                id: format!("pane-{}", u64::MAX),
                                ..Default::default()
                            },
                        },
                        SplitNode::Pane {
                            pane: SplitPane {
                                id: MAIN_PANE_ID.into(),
                                ..Default::default()
                            },
                        },
                    ],
                    sizes: vec![0.5, 0.5],
                },
            },
            focused_pane_id: Some(MAIN_PANE_ID.into()),
        };
        let mut ids = SplitIds::for_layout(&layout.root);
        assert_eq!(ids.create("pane"), None);
        assert_eq!(
            layout.split_empty(MAIN_PANE_ID, SplitPosition::Right, &mut ids),
            None
        );
        assert_eq!(layout.panes().len(), 2, "a refused split changes nothing");
    }

    #[test]
    fn close_pane_keeps_the_last_pane_and_rebalances() {
        let mut layout = SplitLayout::single(MAIN_PANE_ID);
        let mut ids = SplitIds::default();
        assert!(
            !layout.close_pane(MAIN_PANE_ID),
            "the last visible pane never goes"
        );
        assert!(!layout.close_pane("missing"));

        let second = layout
            .split_empty(MAIN_PANE_ID, SplitPosition::Right, &mut ids)
            .unwrap();
        let third = layout
            .split_empty(&second, SplitPosition::Right, &mut ids)
            .unwrap();
        assert_eq!(layout.panes().len(), 3);
        assert!(layout.close_pane(&third));
        assert_eq!(pane_ids(&layout), vec![MAIN_PANE_ID, "pane-1"]);
        assert_eq!(layout.focused_pane_id.as_deref(), Some("pane-1"));

        assert!(layout.close_pane("pane-1"));
        assert_eq!(pane_ids(&layout), vec![MAIN_PANE_ID]);
        let SplitNode::Pane { pane } = &layout.root else {
            panic!("a single remaining pane collapses the group");
        };
        assert_eq!(pane.id, MAIN_PANE_ID);
    }

    #[test]
    fn resize_clamps_to_min_split_size() {
        let mut layout = SplitLayout::single(MAIN_PANE_ID);
        let mut ids = SplitIds::default();
        layout.split_empty(MAIN_PANE_ID, SplitPosition::Right, &mut ids);
        let group_id = match &layout.root {
            SplitNode::Group { group } => group.id.clone(),
            _ => panic!("expected group"),
        };

        assert!(layout.resize_group(&group_id, vec![0.02, 0.98]));
        let SplitNode::Group { group } = &layout.root else {
            panic!("expected group");
        };
        assert!((group.sizes[0] - MIN_SPLIT_SIZE).abs() < 1e-9);
        assert!((group.sizes[1] - (1.0 - MIN_SPLIT_SIZE)).abs() < 1e-9);
        assert!(!layout.resize_group(&group_id, vec![0.5, 0.3, 0.2]));
    }

    #[test]
    fn focus_traversal_walks_a_grid_spatially() {
        let mut layout = SplitLayout::single(MAIN_PANE_ID);
        let mut ids = SplitIds::default();
        layout.split_empty(MAIN_PANE_ID, SplitPosition::Right, &mut ids);
        layout.split_empty(MAIN_PANE_ID, SplitPosition::Bottom, &mut ids);
        layout.split_empty("pane-1", SplitPosition::Bottom, &mut ids);
        // Grid: main (top-left), pane-3 (bottom-left), pane-1 (top-right), pane-5 (bottom-right).
        assert_eq!(
            pane_ids(&layout),
            vec![MAIN_PANE_ID, "pane-3", "pane-1", "pane-5"]
        );

        assert_eq!(
            layout.find_adjacent_pane(MAIN_PANE_ID, FocusDirection::Right),
            Some("pane-1".into())
        );
        assert_eq!(
            layout.find_adjacent_pane(MAIN_PANE_ID, FocusDirection::Down),
            Some("pane-3".into())
        );
        assert_eq!(
            layout.find_adjacent_pane("pane-5", FocusDirection::Left),
            Some("pane-3".into())
        );
        assert_eq!(
            layout.find_adjacent_pane("pane-1", FocusDirection::Down),
            Some("pane-5".into())
        );
        assert_eq!(
            layout.find_adjacent_pane("pane-5", FocusDirection::Up),
            Some("pane-1".into())
        );
        assert_eq!(
            layout.find_adjacent_pane(MAIN_PANE_ID, FocusDirection::Left),
            None
        );
    }

    #[test]
    fn tab_ownership_moves_between_panes_and_survives_a_persistence_roundtrip() {
        let mut layout = SplitLayout::single(MAIN_PANE_ID);
        let mut ids = SplitIds::default();
        layout.split_empty(MAIN_PANE_ID, SplitPosition::Right, &mut ids);
        layout.focus_pane(MAIN_PANE_ID);

        assert_eq!(layout.attach_tab("term-a"), Some(MAIN_PANE_ID.into()));
        assert_eq!(layout.attach_tab("term-b"), Some(MAIN_PANE_ID.into()));
        assert_eq!(layout.focused_pane_id.as_deref(), Some(MAIN_PANE_ID));

        // Revealing a tab keeps it where the user placed it and focuses that pane.
        layout.focus_pane("pane-1");
        assert_eq!(layout.attach_tab("term-a"), Some(MAIN_PANE_ID.into()));
        assert_eq!(layout.focused_pane_id.as_deref(), Some(MAIN_PANE_ID));
        assert!(
            layout
                .pane(MAIN_PANE_ID)
                .unwrap()
                .tabs
                .iter()
                .any(|tab| tab == "term-a")
        );
        assert!(layout.pane("pane-1").unwrap().tabs.is_empty());

        // Only an explicit move relocates it.
        assert!(layout.move_tab_to_pane("term-a", "pane-1"));
        assert_eq!(
            layout.find_pane_containing_tab("term-a").unwrap().id,
            "pane-1"
        );
        assert!(
            !layout
                .pane(MAIN_PANE_ID)
                .unwrap()
                .tabs
                .iter()
                .any(|tab| tab == "term-a")
        );

        // term-c joins pane-1's tab list without moving out of it later.
        assert_eq!(layout.attach_tab("term-c"), Some("pane-1".into()));
        layout.focus_tab("term-a");
        layout.focus_pane(MAIN_PANE_ID);
        assert_eq!(
            layout.pane("pane-1").unwrap().focused_tab.as_deref(),
            Some("term-a")
        );

        let serialized = serde_json::to_string(&layout).unwrap();
        let restored: SplitLayout = serde_json::from_str(&serialized).unwrap();
        assert_eq!(restored, layout);
        assert_eq!(
            restored.find_pane_containing_tab("term-c").unwrap().id,
            "pane-1"
        );
    }

    #[test]
    fn pruning_dead_tabs_releases_empty_panes_but_keeps_the_last_one() {
        let mut layout = SplitLayout::single(MAIN_PANE_ID);
        let mut ids = SplitIds::default();
        layout.split_empty(MAIN_PANE_ID, SplitPosition::Right, &mut ids);
        layout.focus_pane(MAIN_PANE_ID);
        layout.attach_tab("term-a");
        layout.attach_tab("term-b");

        layout.prune_tabs(|tab| tab != "term-a");
        assert_eq!(pane_ids(&layout), vec![MAIN_PANE_ID, "pane-1"]);
        assert_eq!(layout.pane(MAIN_PANE_ID).unwrap().tabs, vec!["term-b"]);
        assert_eq!(
            layout.pane(MAIN_PANE_ID).unwrap().focused_tab.as_deref(),
            Some("term-b")
        );

        // A non-main pane that loses its last tab closes; the main pane
        // survives empty, like the app's preserved default pane.
        layout.focus_pane("pane-1");
        layout.attach_tab("term-c");
        layout.prune_tabs(|tab| tab != "term-c");
        assert_eq!(pane_ids(&layout), vec![MAIN_PANE_ID]);

        layout.attach_tab("term-d");
        layout.prune_tabs(|_| false);
        assert_eq!(pane_ids(&layout), vec![MAIN_PANE_ID]);
        assert!(layout.pane(MAIN_PANE_ID).unwrap().tabs.is_empty());
        assert_eq!(layout.focused_pane_id.as_deref(), Some(MAIN_PANE_ID));
    }

    #[test]
    fn closing_a_browser_tab_leaves_the_terminal_tab_in_its_pane() {
        let mut layout = SplitLayout::single(MAIN_PANE_ID);
        let mut ids = SplitIds::default();
        layout.split_empty(MAIN_PANE_ID, SplitPosition::Right, &mut ids);
        layout.focus_pane(MAIN_PANE_ID);
        layout.attach_tab("term-a");
        // A browser is a pane tab with its own id, not a window-wide overlay: it
        // shares a pane with the terminal and travels with it on a drag.
        layout.attach_tab("browser-1");
        assert_eq!(
            layout.pane(MAIN_PANE_ID).unwrap().focused_tab.as_deref(),
            Some("browser-1")
        );

        assert!(layout.move_tab_to_pane("browser-1", "pane-1"));
        assert_eq!(layout.pane("pane-1").unwrap().tabs, vec!["browser-1"]);
        assert_eq!(layout.pane(MAIN_PANE_ID).unwrap().tabs, vec!["term-a"]);

        // Closing the browser tab closes only the pane it emptied.
        layout.prune_tabs(|tab| tab != "browser-1");
        assert_eq!(pane_ids(&layout), vec![MAIN_PANE_ID]);
        assert_eq!(layout.pane(MAIN_PANE_ID).unwrap().tabs, vec!["term-a"]);
        assert_eq!(
            layout.pane(MAIN_PANE_ID).unwrap().focused_tab.as_deref(),
            Some("term-a")
        );
    }

    #[test]
    fn moving_a_tab_between_panes_transfers_ownership_and_focus() {
        let mut layout = SplitLayout::single(MAIN_PANE_ID);
        let mut ids = SplitIds::default();
        layout.split_empty(MAIN_PANE_ID, SplitPosition::Right, &mut ids);
        layout.focus_pane(MAIN_PANE_ID);
        layout.attach_tab("term-a");
        layout.attach_tab("term-b");

        assert!(layout.move_tab_to_pane("term-a", "pane-1"));
        assert_eq!(layout.focused_pane_id.as_deref(), Some("pane-1"));
        assert_eq!(layout.pane("pane-1").unwrap().tabs, vec!["term-a"]);
        assert_eq!(
            layout.pane("pane-1").unwrap().focused_tab.as_deref(),
            Some("term-a")
        );
        assert_eq!(layout.pane(MAIN_PANE_ID).unwrap().tabs, vec!["term-b"]);
        assert_eq!(
            layout.find_pane_containing_tab("term-a").unwrap().id,
            "pane-1"
        );

        // Moving it back leaves the main pane empty but present; the non-main
        // pane closes once its last tab moves out.
        assert!(layout.move_tab_to_pane("term-a", MAIN_PANE_ID));
        assert_eq!(pane_ids(&layout), vec![MAIN_PANE_ID]);
        assert_eq!(
            layout.pane(MAIN_PANE_ID).unwrap().tabs,
            vec!["term-b", "term-a"]
        );
        assert_eq!(
            layout.pane(MAIN_PANE_ID).unwrap().focused_tab.as_deref(),
            Some("term-a")
        );
        assert!(!layout.move_tab_to_pane("term-a", "missing"));
        assert!(!layout.move_tab_to_pane("term-missing", MAIN_PANE_ID));
    }

    #[test]
    fn adopting_tabs_keeps_them_reachable_without_moving_pane_focus() {
        let mut layout = SplitLayout::single(MAIN_PANE_ID);
        let mut ids = SplitIds::default();
        layout.split_empty(MAIN_PANE_ID, SplitPosition::Right, &mut ids);
        layout.focus_pane(MAIN_PANE_ID);
        layout.attach_tab("term-a");
        layout.focus_pane("pane-1");

        assert!(layout.adopt_tab("term-b"));
        assert!(!layout.adopt_tab("term-b"));
        // The existing tab host owns new tabs; the empty focused pane stays
        // empty until the user puts something in it.
        assert_eq!(
            layout.pane(MAIN_PANE_ID).unwrap().tabs,
            vec!["term-a", "term-b"]
        );
        assert!(layout.pane("pane-1").unwrap().tabs.is_empty());
        assert_eq!(layout.focused_pane_id.as_deref(), Some("pane-1"));
        assert_eq!(
            layout.pane(MAIN_PANE_ID).unwrap().focused_tab.as_deref(),
            Some("term-a")
        );

        layout.focus_pane(MAIN_PANE_ID);
        layout.attach_tab("term-c");
        assert_eq!(
            layout.pane(MAIN_PANE_ID).unwrap().focused_tab.as_deref(),
            Some("term-c")
        );
    }

    #[test]
    fn generated_ids_never_collide_with_a_restored_layout() {
        let mut layout = SplitLayout::single(MAIN_PANE_ID);
        let mut ids = SplitIds::default();
        for _ in 0..3 {
            layout.split_empty(MAIN_PANE_ID, SplitPosition::Right, &mut ids);
        }
        let mut restored_ids = SplitIds::for_layout(&layout.root);
        let fresh = restored_ids.create("pane").expect("ids remain");
        assert!(layout.panes().iter().all(|pane| pane.id != fresh));
        assert_ne!(restored_ids.create("pane").expect("ids remain"), fresh);
    }

    #[test]
    fn group_extent_is_derived_from_the_split_area_for_drag_math() {
        let mut layout = SplitLayout::single(MAIN_PANE_ID);
        let mut ids = SplitIds::default();
        layout.split_empty(MAIN_PANE_ID, SplitPosition::Right, &mut ids);
        let group_id = match &layout.root {
            SplitNode::Group { group } => group.id.clone(),
            _ => panic!("expected group"),
        };
        let area = (0.0, 0.0, 1000.0, 500.0);
        assert_eq!(layout.group_size_px(&group_id, area), Some(1000.0));
        assert_eq!(layout.group_size_px("group-missing", area), None);
    }
}

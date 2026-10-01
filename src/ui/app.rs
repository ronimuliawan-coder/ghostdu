use crate::fs::entry::{DeleteSafety, FileEntry};
use crate::fs::mount_info::{get_detailed_item_info, query_fs_info, DetailedItemInfo, FsMountInfo};
use crate::ghost::{
    classify_path, classify_safety, fetch_docker_disk_info, prune_docker_dangling,
    scan_deleted_open_files, DeletedOpenFile, DockerDiskInfo,
};
use crate::ops::{move_to_trash, permanently_delete};
use std::cell::Cell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveView {
    Filesystem,
    GhostInspector,
    HelpModal,
    ConfirmModal,
    ItemInfoModal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GhostFilterMode {
    ShowAll,
    HideGhost,
    GhostOnly,
}

impl GhostFilterMode {
    pub fn next(&self) -> Self {
        match self {
            GhostFilterMode::ShowAll => GhostFilterMode::HideGhost,
            GhostFilterMode::HideGhost => GhostFilterMode::GhostOnly,
            GhostFilterMode::GhostOnly => GhostFilterMode::ShowAll,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            GhostFilterMode::ShowAll => "All Files",
            GhostFilterMode::HideGhost => "Ghost Files Hidden",
            GhostFilterMode::GhostOnly => "Ghost Files ONLY",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortMode {
    BySizeDesc,
    BySizeAsc,
    ByName,
    ByItems,
}

impl SortMode {
    pub fn next(&self) -> Self {
        match self {
            SortMode::BySizeDesc => SortMode::BySizeAsc,
            SortMode::BySizeAsc => SortMode::ByName,
            SortMode::ByName => SortMode::ByItems,
            SortMode::ByItems => SortMode::BySizeDesc,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            SortMode::BySizeDesc => "Size (desc)",
            SortMode::BySizeAsc => "Size (asc)",
            SortMode::ByName => "Name",
            SortMode::ByItems => "Item Count",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmAction {
    MoveToTrash,
    PermanentDelete,
    DockerPrune,
}

pub struct App {
    pub root_entry: FileEntry,
    pub path_stack: Vec<usize>, // Index stack navigating into child directories
    pub selected_paths: HashSet<PathBuf>,
    pub active_view: ActiveView,
    pub previous_view: ActiveView,
    pub ghost_filter: GhostFilterMode,
    pub sort_mode: SortMode,
    pub apparent_size: bool,
    pub cursor_index: usize,
    pub scroll_offset: Cell<usize>,
    pub search_query: String,
    pub is_searching: bool,
    pub status_message: Option<(String, std::time::Instant)>,
    pub pending_action: Option<ConfirmAction>,
    pub action_targets: Vec<PathBuf>,
    pub action_total_size: u64,
    pub action_safety_blocked: bool,
    pub action_has_recheck: bool,
    pub action_has_system: bool,
    pub safe_only_filter: bool,

    // Ghost Inspector data
    pub docker_info: DockerDiskInfo,
    pub deleted_open_files: Vec<DeletedOpenFile>,
    pub ghost_tab_index: usize, // 0 = Docker, 1 = Deleted-Open Files
    pub ghost_cursor_index: usize,
    pub ghost_docker_scroll_offset: Cell<usize>,
    pub ghost_deleted_scroll_offset: Cell<usize>,

    // Global Filesystem and Detailed Item Info
    pub fs_info: Option<FsMountInfo>,
    pub item_info: Option<DetailedItemInfo>,
}

impl App {
    pub fn new(root_entry: FileEntry) -> Self {
        let docker_info = fetch_docker_disk_info();
        let deleted_open_files = scan_deleted_open_files();
        let fs_info = query_fs_info(&root_entry.path);

        Self {
            root_entry,
            path_stack: Vec::new(),
            selected_paths: HashSet::new(),
            active_view: ActiveView::Filesystem,
            previous_view: ActiveView::Filesystem,
            ghost_filter: GhostFilterMode::ShowAll,
            sort_mode: SortMode::BySizeDesc,
            apparent_size: false,
            cursor_index: 0,
            scroll_offset: Cell::new(0),
            search_query: String::new(),
            is_searching: false,
            status_message: None,
            pending_action: None,
            action_targets: Vec::new(),
            action_total_size: 0,
            action_safety_blocked: false,
            action_has_recheck: false,
            action_has_system: false,
            safe_only_filter: false,
            docker_info,
            deleted_open_files,
            ghost_tab_index: 0,
            ghost_cursor_index: 0,
            ghost_docker_scroll_offset: Cell::new(0),
            ghost_deleted_scroll_offset: Cell::new(0),
            fs_info,
            item_info: None,
        }
    }

    /// Refresh Docker and deleted-open ghost file info
    pub fn refresh_ghost_info(&mut self) {
        self.docker_info = fetch_docker_disk_info();
        self.deleted_open_files = scan_deleted_open_files();
        self.set_status("Refreshed Docker & Ghost files data");
    }

    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status_message = Some((msg.into(), std::time::Instant::now()));
    }

    pub fn current_status(&self) -> Option<&str> {
        if let Some((ref msg, time)) = self.status_message {
            if time.elapsed().as_secs() < 5 {
                return Some(msg.as_str());
            }
        }
        None
    }

    /// Retrieve the current directory node by traversing path_stack
    pub fn current_dir_entry(&self) -> &FileEntry {
        let mut curr = &self.root_entry;
        for &idx in &self.path_stack {
            if idx < curr.children.len() {
                curr = &curr.children[idx];
            } else {
                break;
            }
        }
        curr
    }

    /// Mutable traversal to current directory
    #[allow(dead_code)]
    pub fn current_dir_entry_mut(&mut self) -> &mut FileEntry {
        let mut curr = &mut self.root_entry;
        for &idx in &self.path_stack {
            if idx < curr.children.len() {
                curr = &mut curr.children[idx];
            } else {
                break;
            }
        }
        curr
    }

    /// Filtered and sorted child list for display
    pub fn visible_children(&self) -> Vec<&FileEntry> {
        let current = self.current_dir_entry();
        let mut list: Vec<&FileEntry> = current
            .children
            .iter()
            .filter(|entry| {
                // 1. Ghost filter
                match self.ghost_filter {
                    GhostFilterMode::ShowAll => true,
                    GhostFilterMode::HideGhost => !entry.ghost_kind.is_ghost(),
                    GhostFilterMode::GhostOnly => entry.ghost_kind.is_ghost(),
                }
            })
            .filter(|entry| {
                // 2. Search filter
                if self.search_query.is_empty() {
                    true
                } else {
                    entry
                        .name
                        .to_lowercase()
                        .contains(&self.search_query.to_lowercase())
                }
            })
            .filter(|entry| {
                // 3. Safe-to-clean filter (c key)
                if self.safe_only_filter {
                    entry.delete_safety.is_safe()
                } else {
                    true
                }
            })
            .collect();

        // Sort items
        match self.sort_mode {
            SortMode::BySizeDesc => {
                list.sort_by(|a, b| {
                    let s_b = b.display_size(self.apparent_size);
                    let s_a = a.display_size(self.apparent_size);
                    s_b.cmp(&s_a)
                });
            }
            SortMode::BySizeAsc => {
                list.sort_by(|a, b| {
                    let s_a = a.display_size(self.apparent_size);
                    let s_b = b.display_size(self.apparent_size);
                    s_a.cmp(&s_b)
                });
            }
            SortMode::ByName => {
                list.sort_by_key(|a| a.name.to_lowercase());
            }
            SortMode::ByItems => {
                list.sort_by_key(|a| std::cmp::Reverse(a.items_count));
            }
        }

        list
    }

    /// Move cursor down
    pub fn cursor_down(&mut self) {
        if self.active_view == ActiveView::GhostInspector {
            let max = if self.ghost_tab_index == 0 {
                self.docker_info.items.len()
            } else {
                self.deleted_open_files.len()
            };
            if max > 0 && self.ghost_cursor_index + 1 < max {
                self.ghost_cursor_index += 1;
            }
            return;
        }

        let total = self.visible_children().len();
        if total > 0 && self.cursor_index + 1 < total {
            self.cursor_index += 1;
        }
    }

    /// Move cursor up
    pub fn cursor_up(&mut self) {
        if self.active_view == ActiveView::GhostInspector {
            if self.ghost_cursor_index > 0 {
                self.ghost_cursor_index -= 1;
            }
            return;
        }

        if self.cursor_index > 0 {
            self.cursor_index -= 1;
        }
    }

    /// Page down
    pub fn page_down(&mut self, step: usize) {
        if self.active_view == ActiveView::GhostInspector {
            let max = if self.ghost_tab_index == 0 {
                self.docker_info.items.len()
            } else {
                self.deleted_open_files.len()
            };
            if max > 0 {
                self.ghost_cursor_index = (self.ghost_cursor_index + step).min(max - 1);
            }
            return;
        }

        let total = self.visible_children().len();
        if total > 0 {
            self.cursor_index = (self.cursor_index + step).min(total - 1);
        }
    }

    /// Page up
    pub fn page_up(&mut self, step: usize) {
        if self.active_view == ActiveView::GhostInspector {
            self.ghost_cursor_index = self.ghost_cursor_index.saturating_sub(step);
            return;
        }

        self.cursor_index = self.cursor_index.saturating_sub(step);
    }

    /// Jump to start / top
    pub fn cursor_to_start(&mut self) {
        if self.active_view == ActiveView::GhostInspector {
            self.ghost_cursor_index = 0;
            return;
        }
        self.cursor_index = 0;
    }

    /// Jump to end / bottom
    pub fn cursor_to_end(&mut self) {
        if self.active_view == ActiveView::GhostInspector {
            let max = if self.ghost_tab_index == 0 {
                self.docker_info.items.len()
            } else {
                self.deleted_open_files.len()
            };
            if max > 0 {
                self.ghost_cursor_index = max - 1;
            }
            return;
        }

        let total = self.visible_children().len();
        if total > 0 {
            self.cursor_index = total - 1;
        }
    }

    /// Enter directory
    pub fn enter_selected(&mut self) {
        let visible = self.visible_children();
        if let Some(target) = visible.get(self.cursor_index) {
            if target.is_dir {
                let target_path = target.path.clone();
                // Locate original child index in parent
                let current = self.current_dir_entry();
                if let Some(orig_idx) = current.children.iter().position(|c| c.path == target_path)
                {
                    self.path_stack.push(orig_idx);
                    self.cursor_index = 0;
                    self.scroll_offset.set(0);
                    self.search_query.clear();
                    self.refresh_fs_info();
                }
            }
        }
    }

    /// Navigate to parent directory, or return parent path to rescan if at root
    pub fn go_up(&mut self) -> Option<PathBuf> {
        if !self.path_stack.is_empty() {
            let last_idx = self.path_stack.pop().unwrap_or(0);
            self.cursor_index = last_idx;
            self.scroll_offset.set(0);
            self.search_query.clear();
            self.refresh_fs_info();
            None
        } else {
            // At root of current scan: if parent exists, return it to allow ascending
            if let Some(parent) = self.root_entry.path.parent() {
                if parent != self.root_entry.path && parent.exists() {
                    return Some(parent.to_path_buf());
                }
            }
            None
        }
    }

    /// Open detailed Item & Filesystem info modal (ncdu 'i' key)
    pub fn open_item_info(&mut self) {
        let visible = self.visible_children();
        if let Some(target) = visible.get(self.cursor_index) {
            self.item_info = get_detailed_item_info(&target.path, target.items_count);
            self.previous_view = self.active_view;
            self.active_view = ActiveView::ItemInfoModal;
        }
    }

    /// Refresh filesystem stats for current directory
    pub fn refresh_fs_info(&mut self) {
        let current_path = self.current_dir_entry().path.clone();
        self.fs_info = query_fs_info(&current_path);
    }

    /// Toggle selection of current item or marked set
    pub fn toggle_selection(&mut self) {
        let visible = self.visible_children();
        if let Some(target) = visible.get(self.cursor_index) {
            let p = target.path.clone();
            if self.selected_paths.contains(&p) {
                self.selected_paths.remove(&p);
            } else {
                self.selected_paths.insert(p);
            }
        }
    }

    /// Invert / select all in current visible directory
    pub fn select_all_visible(&mut self) {
        let paths: Vec<PathBuf> = self
            .visible_children()
            .into_iter()
            .map(|e| e.path.clone())
            .collect();
        let all_selected = paths.iter().all(|p| self.selected_paths.contains(p));
        if all_selected {
            for p in &paths {
                self.selected_paths.remove(p);
            }
        } else {
            for p in paths {
                self.selected_paths.insert(p);
            }
        }
    }

    /// Selected items count and total size
    pub fn selection_summary(&self) -> (usize, u64) {
        let count = self.selected_paths.len();
        let mut total_size = 0u64;

        // Traverse tree to calculate sizes
        fn sum_selected(
            entry: &FileEntry,
            selected: &HashSet<PathBuf>,
            apparent: bool,
            sum: &mut u64,
        ) {
            if selected.contains(&entry.path) {
                *sum = sum.saturating_add(entry.display_size(apparent));
            } else if entry.is_dir {
                for child in &entry.children {
                    sum_selected(child, selected, apparent, sum);
                }
            }
        }

        sum_selected(
            &self.root_entry,
            &self.selected_paths,
            self.apparent_size,
            &mut total_size,
        );
        (count, total_size)
    }

    /// Toggle filter showing only safe-to-clean items
    pub fn toggle_safe_filter(&mut self) {
        self.safe_only_filter = !self.safe_only_filter;
        self.cursor_index = 0;
        self.scroll_offset.set(0);
        if self.safe_only_filter {
            self.set_status("Safe-to-Clean filter: ON (showing only 🟢 safe items)");
        } else {
            self.set_status("Safe-to-Clean filter: OFF (showing all items)");
        }
    }

    /// Prepare Move to Wastebin confirmation
    pub fn prompt_move_to_trash(&mut self) {
        let mut targets = Vec::new();
        let mut total_size = 0u64;

        if !self.selected_paths.is_empty() {
            targets = self.selected_paths.iter().cloned().collect();
            let (_, sz) = self.selection_summary();
            total_size = sz;
        } else {
            let visible = self.visible_children();
            if let Some(entry) = visible.get(self.cursor_index) {
                targets.push(entry.path.clone());
                total_size = entry.display_size(self.apparent_size);
            }
        }

        if targets.is_empty() {
            self.set_status("No item selected to move to wastebin");
            return;
        }

        let mut has_system = false;
        let mut has_recheck = false;
        for path in &targets {
            let ghost = classify_path(path);
            let safety = classify_safety(path, ghost);
            if safety == DeleteSafety::System {
                has_system = true;
            } else if safety == DeleteSafety::Recheck {
                has_recheck = true;
            }
        }

        self.action_targets = targets;
        self.action_total_size = total_size;
        self.action_has_system = has_system;
        self.action_has_recheck = has_recheck;
        self.action_safety_blocked = has_system;
        self.pending_action = Some(ConfirmAction::MoveToTrash);
        self.previous_view = self.active_view;
        self.active_view = ActiveView::ConfirmModal;
    }

    /// Prepare Permanent Deletion confirmation
    pub fn prompt_permanent_delete(&mut self) {
        let mut targets = Vec::new();
        let mut total_size = 0u64;

        if !self.selected_paths.is_empty() {
            targets = self.selected_paths.iter().cloned().collect();
            let (_, sz) = self.selection_summary();
            total_size = sz;
        } else {
            let visible = self.visible_children();
            if let Some(entry) = visible.get(self.cursor_index) {
                targets.push(entry.path.clone());
                total_size = entry.display_size(self.apparent_size);
            }
        }

        if targets.is_empty() {
            self.set_status("No item selected to permanently delete");
            return;
        }

        let mut has_system = false;
        let mut has_recheck = false;
        for path in &targets {
            let ghost = classify_path(path);
            let safety = classify_safety(path, ghost);
            if safety == DeleteSafety::System {
                has_system = true;
            } else if safety == DeleteSafety::Recheck {
                has_recheck = true;
            }
        }

        self.action_targets = targets;
        self.action_total_size = total_size;
        self.action_has_system = has_system;
        self.action_has_recheck = has_recheck;
        self.action_safety_blocked = has_system;
        self.pending_action = Some(ConfirmAction::PermanentDelete);
        self.previous_view = self.active_view;
        self.active_view = ActiveView::ConfirmModal;
    }

    /// Prepare Docker Prune confirmation
    pub fn prompt_docker_prune(&mut self) {
        let total_reclaimable = self.docker_info.images_reclaimable_size
            + self.docker_info.containers_reclaimable_size
            + self.docker_info.volumes_reclaimable_size
            + self.docker_info.build_cache_reclaimable_size;

        self.action_targets.clear();
        self.action_total_size = total_reclaimable;
        self.action_has_system = false;
        self.action_has_recheck = false;
        self.action_safety_blocked = false;
        self.pending_action = Some(ConfirmAction::DockerPrune);
        self.previous_view = self.active_view;
        self.active_view = ActiveView::ConfirmModal;
    }

    /// Execute pending action after user confirms
    pub fn execute_pending_action(&mut self) {
        if self.action_safety_blocked {
            self.set_status("⛔ BLOCKED: Cannot delete protected system file/directory!");
            self.cancel_modal();
            return;
        }

        let action = match self.pending_action.take() {
            Some(a) => a,
            None => {
                self.active_view = self.previous_view;
                return;
            }
        };

        match action {
            ConfirmAction::MoveToTrash => {
                let targets = std::mem::take(&mut self.action_targets);
                let result = move_to_trash(&targets);
                let count = result.succeeded.len();
                let failed_count = result.failed.len();

                // Remove deleted paths from tree
                for path in &result.succeeded {
                    self.selected_paths.remove(path);
                    self.remove_path_from_tree(path);
                }

                if failed_count == 0 {
                    self.set_status(format!("✔ Moved {} items to Wastebin", count));
                } else {
                    self.set_status(format!(
                        "Moved {} to wastebin, {} failed (permissions/cross-fs)",
                        count, failed_count
                    ));
                }
            }
            ConfirmAction::PermanentDelete => {
                let targets = std::mem::take(&mut self.action_targets);
                let result = permanently_delete(&targets);
                let count = result.succeeded.len();
                let failed_count = result.failed.len();

                // Remove deleted paths from tree
                for path in &result.succeeded {
                    self.selected_paths.remove(path);
                    self.remove_path_from_tree(path);
                }

                if failed_count == 0 {
                    self.set_status(format!("✔ Permanently removed {} items", count));
                } else {
                    self.set_status(format!(
                        "Removed {} items, {} failed (permissions)",
                        count, failed_count
                    ));
                }
            }
            ConfirmAction::DockerPrune => match prune_docker_dangling() {
                Ok(msg) => {
                    self.set_status(format!("✔ Docker Prune: {}", msg));
                    self.refresh_ghost_info();
                }
                Err(err) => {
                    self.set_status(format!("❌ Docker Prune failed: {}", err));
                }
            },
        }

        // Adjust cursor
        let total = self.visible_children().len();
        if self.cursor_index >= total && total > 0 {
            self.cursor_index = total - 1;
        }

        self.action_safety_blocked = false;
        self.action_has_recheck = false;
        self.action_has_system = false;
        self.active_view = self.previous_view;
    }

    /// Cancel pending confirmation
    pub fn cancel_modal(&mut self) {
        self.pending_action = None;
        self.action_targets.clear();
        self.action_total_size = 0;
        self.action_safety_blocked = false;
        self.action_has_recheck = false;
        self.action_has_system = false;
        self.active_view = self.previous_view;
    }

    /// Remove deleted item from internal directory tree and recalculate sizes
    fn remove_path_from_tree(&mut self, path: &Path) {
        fn remove_rec(entry: &mut FileEntry, target: &Path) -> bool {
            let initial_len = entry.children.len();
            entry.children.retain(|c| c.path != target);
            if entry.children.len() != initial_len {
                // Item removed directly from this directory; recalculate
                recalc(entry);
                return true;
            }

            let mut found = false;
            for child in &mut entry.children {
                if child.is_dir && target.starts_with(&child.path) && remove_rec(child, target) {
                    found = true;
                    break;
                }
            }
            if found {
                recalc(entry);
            }
            found
        }

        fn recalc(entry: &mut FileEntry) {
            let mut total_size = 0u64;
            let mut total_disk = 0u64;
            let mut total_items = 0usize;
            for child in &entry.children {
                total_size = total_size.saturating_add(child.size);
                total_disk = total_disk.saturating_add(child.disk_usage);
                total_items = total_items.saturating_add(child.items_count);
            }
            entry.size = total_size;
            entry.disk_usage = total_disk;
            entry.items_count = total_items + 1;
        }

        remove_rec(&mut self.root_entry, path);
    }

    /// Replace a subtree at target path with a newly scanned node and recalculate ancestor sizes
    pub fn replace_subtree(&mut self, target: &Path, new_node: FileEntry) {
        fn replace_rec(entry: &mut FileEntry, target: &Path, new_node: &FileEntry) -> bool {
            for child in &mut entry.children {
                if child.path == target {
                    *child = new_node.clone();
                    recalc(entry);
                    return true;
                }
                if child.is_dir
                    && target.starts_with(&child.path)
                    && replace_rec(child, target, new_node)
                {
                    recalc(entry);
                    return true;
                }
            }
            false
        }

        fn recalc(entry: &mut FileEntry) {
            let mut total_size = 0u64;
            let mut total_disk = 0u64;
            let mut total_items = 0usize;
            for child in &entry.children {
                total_size = total_size.saturating_add(child.size);
                total_disk = total_disk.saturating_add(child.disk_usage);
                total_items = total_items.saturating_add(child.items_count);
            }
            entry.size = total_size;
            entry.disk_usage = total_disk;
            entry.items_count = total_items + 1;
        }

        replace_rec(&mut self.root_entry, target, &new_node);

        // Validate path_stack bounds
        let mut curr = &self.root_entry;
        let mut valid_depth = 0;
        for &idx in &self.path_stack {
            if idx < curr.children.len() {
                curr = &curr.children[idx];
                valid_depth += 1;
            } else {
                break;
            }
        }
        self.path_stack.truncate(valid_depth);
    }

    /// Refresh filesystem stats, directory tree, Docker storage, and ghost files without restarting
    pub fn refresh_all(&mut self) {
        let current_path = self.current_dir_entry().path.clone();
        let is_at_root = self.path_stack.is_empty();

        let stop_signal = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        if is_at_root {
            if let Ok(new_root) =
                crate::fs::scanner::scan_directory(&self.root_entry.path, None, stop_signal)
            {
                self.root_entry = new_root;
            }
        } else {
            if let Ok(new_subtree) =
                crate::fs::scanner::scan_directory(&current_path, None, stop_signal)
            {
                self.replace_subtree(&current_path, new_subtree);
            }
        }

        // 1. Refresh global fs statvfs
        self.refresh_fs_info();

        // 2. Refresh Docker & unlinked ghost files
        self.docker_info = crate::ghost::fetch_docker_disk_info();
        self.deleted_open_files = crate::ghost::scan_deleted_open_files();

        // 3. Refresh detailed item info if modal is open
        if self.active_view == ActiveView::ItemInfoModal {
            let visible = self.visible_children();
            if let Some(target) = visible.get(self.cursor_index) {
                self.item_info =
                    crate::fs::mount_info::get_detailed_item_info(&target.path, target.items_count);
            }
        }

        // 4. Clamp cursor
        let total = self.visible_children().len();
        if self.cursor_index >= total && total > 0 {
            self.cursor_index = total - 1;
        }

        self.set_status("⚡ Refreshed disk usage, free space & ghost details");
    }

    /// Navigate path_stack to reach target path if it exists within root_entry
    pub fn navigate_to_path(&mut self, target: &Path) -> bool {
        self.path_stack.clear();
        self.cursor_index = 0;
        self.scroll_offset.set(0);

        if target == self.root_entry.path {
            self.refresh_fs_info();
            return true;
        }

        let mut curr = &self.root_entry;
        loop {
            let mut matched = false;
            for (idx, child) in curr.children.iter().enumerate() {
                if child.path == target {
                    self.path_stack.push(idx);
                    self.refresh_fs_info();
                    return true;
                }
                if child.is_dir && target.starts_with(&child.path) {
                    self.path_stack.push(idx);
                    curr = child;
                    matched = true;
                    break;
                }
            }
            if !matched {
                break;
            }
        }

        self.refresh_fs_info();
        !self.path_stack.is_empty()
    }
}

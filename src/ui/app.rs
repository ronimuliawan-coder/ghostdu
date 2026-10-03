use crate::fs::entry::{DeleteSafety, FileEntry};
use crate::fs::mount_info::{get_detailed_item_info, query_fs_info, DetailedItemInfo, FsMountInfo};
use crate::ghost::{
    classify_path, classify_safety, fetch_docker_disk_info, prune_docker_dangling,
    scan_deleted_open_files, DeletedOpenFile, DockerDiskInfo,
};
use crate::ops::delete::permanently_delete_confirmed;
use crate::ops::trash::move_to_trash_confirmed;
use crate::ops::{TargetIdentities, TargetIdentity};
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
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
    selected_identities: TargetIdentities,
    action_identities: TargetIdentities,
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
            selected_identities: TargetIdentities::new(),
            action_identities: TargetIdentities::new(),
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
        let query_lower = if self.search_query.is_empty() {
            None
        } else {
            Some(self.search_query.to_lowercase())
        };

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
                match &query_lower {
                    None => true,
                    Some(q) => entry.name.to_lowercase().contains(q),
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
                    s_b.cmp(&s_a).then_with(|| a.name.cmp(&b.name))
                });
            }
            SortMode::BySizeAsc => {
                list.sort_by(|a, b| {
                    let s_a = a.display_size(self.apparent_size);
                    let s_b = b.display_size(self.apparent_size);
                    s_a.cmp(&s_b).then_with(|| a.name.cmp(&b.name))
                });
            }
            SortMode::ByName => {
                list.sort_by_key(|a| a.name.to_lowercase());
            }
            SortMode::ByItems => {
                list.sort_by(|a, b| {
                    b.items_count
                        .cmp(&a.items_count)
                        .then_with(|| a.name.cmp(&b.name))
                });
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
                self.selected_identities.remove(&p);
            } else {
                self.select_path(p);
            }
        }
    }

    /// Invert / select all in current visible directory
    pub fn select_all_visible(&mut self) {
        // Snapshot scanned identities once so the batch avoids a per-item
        // `find_entry` tree walk (O(n²) for large directories).
        let entries: Vec<(PathBuf, u64, u64, bool, bool)> = self
            .visible_children()
            .into_iter()
            .map(|e| (e.path.clone(), e.dev, e.ino, e.is_dir, e.is_symlink))
            .collect();
        let all_selected = entries
            .iter()
            .all(|(p, _, _, _, _)| self.selected_paths.contains(p));
        if all_selected {
            for (p, _, _, _, _) in &entries {
                self.selected_paths.remove(p);
                self.selected_identities.remove(p);
            }
        } else {
            let requested = entries
                .iter()
                .filter(|(path, _, _, _, _)| !self.selected_paths.contains(path))
                .count();
            let before = self.selected_paths.len();
            let limit = self.selection_limit();
            for (p, dev, ino, is_dir, is_symlink) in entries {
                if !self.select_scanned_entry_with_limit(
                    p,
                    dev,
                    ino,
                    is_dir,
                    is_symlink,
                    limit.as_ref(),
                ) {
                    let reason = self
                        .current_status()
                        .unwrap_or("Selection stopped")
                        .to_string();
                    self.set_status(format!(
                        "Selected {} of {} additional items; {}",
                        self.selected_paths.len() - before,
                        requested,
                        reason
                    ));
                    break;
                }
            }
        }
    }

    fn selection_limit(&self) -> std::io::Result<usize> {
        const MAX_SELECTIONS: usize = 256;
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // getrlimit writes a valid rlimit to this live, writable object.
        if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let soft = usize::try_from(limit.rlim_cur).unwrap_or(usize::MAX);
        // Leave headroom for traversal, trash destinations, sockets and the UI.
        let reserve = 64.min(soft / 2).max(1);
        let open = std::fs::read_dir("/proc/self/fd")?
            .try_fold(0usize, |count, entry| entry.map(|_| count + 1))?;
        let available = soft.saturating_sub(open).saturating_sub(reserve);
        Ok(MAX_SELECTIONS.min(self.selected_identities.len().saturating_add(available)))
    }

    fn select_path_with_limit(
        &mut self,
        path: PathBuf,
        limit_res: Result<&usize, &std::io::Error>,
    ) -> bool {
        // Do not silently rebind an existing selection to a replacement object.
        if self.selected_paths.contains(&path) {
            return true;
        }
        match limit_res {
            Ok(&limit) if self.selected_identities.len() >= limit => {
                self.set_status(format!(
                    "Selection limit reached ({limit} items); deselect items to leave file handles available"
                ));
                return false;
            }
            Err(error) => {
                self.set_status(format!("Cannot determine safe selection limit: {error}"));
                return false;
            }
            Ok(_) => {}
        }
        match self.capture_verified_identity(&path) {
            Ok(identity) => {
                self.selected_identities.insert(path.clone(), identity);
                self.selected_paths.insert(path);
                true
            }
            Err(error) => {
                self.set_status(format!("Cannot select item: {error}"));
                false
            }
        }
    }

    fn select_scanned_entry_with_limit(
        &mut self,
        path: PathBuf,
        dev: u64,
        ino: u64,
        is_dir: bool,
        is_symlink: bool,
        limit_res: Result<&usize, &std::io::Error>,
    ) -> bool {
        if self.selected_paths.contains(&path) {
            return true;
        }
        match limit_res {
            Ok(&limit) if self.selected_identities.len() >= limit => {
                self.set_status(format!(
                    "Selection limit reached ({limit} items); deselect items to leave file handles available"
                ));
                return false;
            }
            Err(error) => {
                self.set_status(format!("Cannot determine safe selection limit: {error}"));
                return false;
            }
            Ok(_) => {}
        }
        // ponytail: fail closed on zero ids (error entries); caller passes scanned ids directly
        match TargetIdentity::capture(&path) {
            Ok(identity) if identity.matches_ids(dev, ino, is_dir, is_symlink) => {
                self.selected_identities.insert(path.clone(), identity);
                self.selected_paths.insert(path);
                true
            }
            Ok(_) => {
                self.set_status(
                    "Cannot select item: Target changed since scan; refresh and select it again",
                );
                false
            }
            Err(error) => {
                self.set_status(format!("Cannot select item: {error}"));
                false
            }
        }
    }

    fn capture_verified_identity(&self, path: &Path) -> std::io::Result<TargetIdentity> {
        TargetIdentity::capture(path).and_then(|identity| match self.find_entry(path) {
            Some(entry)
                if identity.matches_ids(entry.dev, entry.ino, entry.is_dir, entry.is_symlink) =>
            {
                Ok(identity)
            }
            _ => Err(std::io::Error::other(
                "Target changed since scan; refresh and select it again",
            )),
        })
    }

    fn select_path(&mut self, path: PathBuf) -> bool {
        let limit = self.selection_limit();
        self.select_path_with_limit(path, limit.as_ref())
    }

    fn capture_confirmation(&mut self, targets: &[PathBuf]) -> bool {
        self.action_identities.clear();
        for path in targets {
            let identity = if self.selected_paths.contains(path) {
                self.selected_identities
                    .get(path)
                    .cloned()
                    .filter(|identity| identity.matches_path(path))
                    .ok_or_else(|| {
                        std::io::Error::other(
                            "Selection changed; select it again and confirm a new action",
                        )
                    })
            } else {
                self.capture_verified_identity(path)
            };
            match identity {
                Ok(identity) => {
                    self.action_identities.insert(path.clone(), identity);
                }
                Err(error) => {
                    self.selected_paths.remove(path);
                    self.selected_identities.remove(path);
                    self.action_identities.clear();
                    self.set_status(format!("Cannot confirm action: {error}"));
                    return false;
                }
            }
        }
        true
    }

    fn discard_changed_selections(&mut self) {
        self.selected_paths.retain(|path| {
            self.selected_identities
                .get(path)
                .is_some_and(|identity| identity.matches_path(path))
        });
        self.selected_identities
            .retain(|path, _| self.selected_paths.contains(path));
    }

    /// Selected items count and total size
    pub fn selection_summary(&self) -> (usize, u64) {
        let count = self.selected_paths.len();
        if count == 0 {
            return (0, 0);
        }
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

        if !has_system && !self.capture_confirmation(&targets) {
            return;
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

        if !has_system && !self.capture_confirmation(&targets) {
            return;
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
        self.action_identities.clear();
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

        let current_path = self.current_dir_entry().path.clone();
        match action {
            ConfirmAction::MoveToTrash => {
                let targets = std::mem::take(&mut self.action_targets);
                let result = move_to_trash_confirmed(&targets, &self.action_identities);
                let count = result.succeeded.len();
                let failed_count = result.failed.len();

                // Remove deleted paths from tree
                for path in &result.succeeded {
                    self.selected_paths.remove(path);
                }
                self.apply_tree_updates(
                    result
                        .succeeded
                        .iter()
                        .cloned()
                        .map(|path| (path, None))
                        .collect(),
                );

                self.navigate_to_path(&current_path);
                if count > 0 {
                    self.refresh_fs_info();
                }

                if failed_count == 0 {
                    self.set_status(format!("✔ Moved {} items to Wastebin", count));
                } else {
                    self.set_status(format!(
                        "Moved {} to wastebin, {} failed: {}",
                        count, failed_count, result.failed[0].1
                    ));
                }
            }
            ConfirmAction::PermanentDelete => {
                let targets = std::mem::take(&mut self.action_targets);
                let result = permanently_delete_confirmed(&targets, &self.action_identities);
                let count = result.succeeded.len();
                let failed_count = result.failed.len();

                // Remove deleted paths from tree
                for path in &result.succeeded {
                    self.selected_paths.remove(path);
                }
                self.apply_tree_updates(
                    result
                        .succeeded
                        .iter()
                        .cloned()
                        .map(|path| (path, None))
                        .collect(),
                );

                // A failed recursive deletion may already have removed children.
                let reconciled = if failed_count == 0 {
                    self.navigate_to_path(&current_path);
                    true
                } else {
                    self.reconcile_after_delete_failure(&current_path, &result.failed)
                };
                if count > 0 || failed_count > 0 {
                    self.refresh_fs_info();
                }

                if failed_count == 0 {
                    self.set_status(format!("✔ Permanently removed {} items", count));
                } else {
                    self.set_status(format!(
                        "Removed {} items, {} failed: {}{}",
                        count,
                        failed_count,
                        result.failed[0].1,
                        if reconciled {
                            ""
                        } else {
                            "; rescan failed, displayed tree may be stale"
                        }
                    ));
                }
            }
            ConfirmAction::DockerPrune => match prune_docker_dangling() {
                Ok(msg) => {
                    self.refresh_ghost_info();
                    self.set_status(format!("✔ Docker Prune: {}", msg));
                }
                Err(err) => {
                    // Some categories may have succeeded before another request failed.
                    self.refresh_ghost_info();
                    self.set_status(format!("❌ Docker Prune failed: {}", err));
                }
            },
        }

        self.action_identities.clear();
        self.discard_changed_selections();
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

    fn reconcile_after_delete_failure(
        &mut self,
        current_path: &Path,
        failed: &[(PathBuf, String)],
    ) -> bool {
        // Coalesce overlapping failed targets; never rescan unrelated siblings.
        let mut paths: Vec<_> = failed.iter().map(|(path, _)| path.clone()).collect();
        paths.sort();
        paths.dedup();
        let mut roots: Vec<PathBuf> = Vec::new();
        for path in paths {
            if !roots.last().is_some_and(|root| path.starts_with(root)) {
                roots.push(path);
            }
        }
        fn retained_inodes(
            entry: &FileEntry,
            roots: &HashSet<PathBuf>,
            seen: &mut HashSet<(u64, u64)>,
        ) {
            // Stop at each failed root, so descendants need no prefix comparisons.
            if roots.contains(&entry.path) {
                return;
            }
            if entry.is_dir || entry.size > 0 || entry.disk_usage > 0 || entry.safe_items > 0 {
                seen.insert((entry.dev, entry.ino));
            }
            for child in &entry.children {
                retained_inodes(child, roots, seen);
            }
        }
        fn has_errors(entry: &FileEntry) -> bool {
            entry.has_err || entry.children.iter().any(has_errors)
        }
        let mut seen = HashSet::new();
        retained_inodes(
            &self.root_entry,
            &roots.iter().cloned().collect(),
            &mut seen,
        );
        let mut reconciled = true;
        let mut updates = HashMap::new();
        for path in roots {
            match crate::fs::scanner::rescan_entry(&path, self.root_entry.dev, &mut seen) {
                Ok(entry) => {
                    reconciled &= !has_errors(&entry);
                    updates.insert(path, Some(entry));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    updates.insert(path, None);
                }
                Err(_) => {
                    reconciled = false;
                }
            }
        }
        self.apply_tree_updates(updates);
        if !reconciled {
            self.root_entry.has_err = true;
        }
        self.navigate_to_path(current_path);
        self.discard_changed_selections();
        reconciled
    }

    /// Cancel pending confirmation
    pub fn cancel_modal(&mut self) {
        self.pending_action = None;
        self.action_targets.clear();
        self.action_identities.clear();
        self.action_total_size = 0;
        self.action_safety_blocked = false;
        self.action_has_recheck = false;
        self.action_has_system = false;
        self.active_view = self.previous_view;
    }

    /// Apply a whole batch in one walk and recalculate each changed ancestor once.
    /// Returns visited nodes so regression tests can assert linear traversal work.
    fn apply_tree_updates(&mut self, mut updates: HashMap<PathBuf, Option<FileEntry>>) -> usize {
        fn recalc(entry: &mut FileEntry) {
            let mut total_size = 0u64;
            let mut total_disk = 0u64;
            let mut total_reclaimable = 0u64;
            let mut total_items = 0usize;
            let mut total_safe_reclaimable = 0u64;
            let mut total_safe_items = 0usize;
            for child in &entry.children {
                total_size = total_size.saturating_add(child.size);
                total_disk = total_disk.saturating_add(child.disk_usage);
                total_reclaimable = total_reclaimable.saturating_add(child.reclaimable);
                total_items = total_items.saturating_add(child.items_count);
                total_safe_reclaimable =
                    total_safe_reclaimable.saturating_add(child.safe_reclaimable_bytes());
                total_safe_items = total_safe_items.saturating_add(child.safe_items_count());
            }
            entry.size = total_size;
            entry.disk_usage = total_disk;
            entry.reclaimable = total_reclaimable;
            entry.items_count = total_items + 1;
            if entry.delete_safety == DeleteSafety::Safe {
                entry.safe_reclaimable = total_reclaimable;
                entry.safe_items = 1;
            } else {
                entry.safe_reclaimable = total_safe_reclaimable;
                entry.safe_items = total_safe_items;
            }
        }

        fn apply(
            entry: &mut FileEntry,
            updates: &mut HashMap<PathBuf, Option<FileEntry>>,
            ancestors: &HashSet<PathBuf>,
            visited: &mut usize,
        ) -> bool {
            if updates.is_empty() {
                return false;
            }
            *visited += 1;
            let mut changed = false;
            entry.children.retain_mut(|child| {
                if updates.is_empty() {
                    return true;
                }
                if let Some(replacement) = updates.remove(&child.path) {
                    *visited += 1;
                    changed = true;
                    if let Some(replacement) = replacement {
                        *child = replacement;
                    } else {
                        return false;
                    }
                } else if child.is_dir && ancestors.contains(&child.path) {
                    changed |= apply(child, updates, ancestors, visited);
                }
                true
            });
            if changed {
                recalc(entry);
            }
            changed
        }
        let ancestors = updates
            .keys()
            .flat_map(|path| path.ancestors().skip(1).map(Path::to_path_buf))
            .collect();
        let mut visited = 0;
        apply(&mut self.root_entry, &mut updates, &ancestors, &mut visited);
        // Navigation is restored from its saved path by action callers.
        let mut curr = &self.root_entry;
        let mut valid_depth = 0;
        for &idx in &self.path_stack {
            if let Some(child) = curr.children.get(idx) {
                curr = child;
                valid_depth += 1;
            } else {
                break;
            }
        }
        self.path_stack.truncate(valid_depth);
        visited
    }

    /// Replace a subtree and update its ancestors using the batch mutation path.
    pub fn replace_subtree(&mut self, target: &Path, new_node: FileEntry) {
        self.apply_tree_updates(HashMap::from([(target.to_path_buf(), Some(new_node))]));
    }

    /// Refresh filesystem stats, directory tree, Docker storage, and ghost files without restarting
    pub fn refresh_all(&mut self) {
        self.discard_changed_selections();
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

    /// Look up the scanned FileEntry for a given target path if present in root_entry
    pub fn find_entry(&self, target: &Path) -> Option<&FileEntry> {
        if !target.starts_with(&self.root_entry.path) {
            return None;
        }
        if target == self.root_entry.path {
            return Some(&self.root_entry);
        }
        let mut curr = &self.root_entry;
        loop {
            let mut matched = false;
            for child in &curr.children {
                if child.path == target {
                    return Some(child);
                }
                if child.is_dir && target.starts_with(&child.path) {
                    curr = child;
                    matched = true;
                    break;
                }
            }
            if !matched {
                return None;
            }
        }
    }
}

#[cfg(test)]
mod reconciliation_tests {
    use super::*;
    use std::fs;
    use std::sync::{atomic::AtomicBool, Arc};

    #[test]
    fn sparse_and_missing_updates_skip_unrelated_descendants() {
        let fixture = tempfile::tempdir().unwrap();
        let unrelated = fixture.path().join("unrelated");
        let affected = fixture.path().join("affected");
        fs::create_dir(&unrelated).unwrap();
        fs::create_dir(&affected).unwrap();
        for index in 0..512 {
            fs::write(unrelated.join(format!("file-{index}")), "large").unwrap();
        }
        let target = affected.join("target");
        fs::write(&target, "x").unwrap();
        let root = crate::fs::scanner::scan_directory(
            fixture.path(),
            None,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let mut app = App::new(root);
        assert_eq!(app.root_entry.children[0].path, unrelated);
        let visited = app.apply_tree_updates(HashMap::from([(target, None)]));
        assert_eq!(visited, 3); // root, affected directory, direct target
        let visited = app.apply_tree_updates(HashMap::from([(affected.join("missing"), None)]));
        assert_eq!(visited, 2); // root and affected; unrelated children never visited
        assert_eq!(app.root_entry.children[0].children.len(), 512);
    }

    #[test]
    fn selections_leave_descriptor_headroom_and_report_partial_batches() {
        const CHILD_LIMIT: &str = "GHOSTDU_SELECTION_LIMIT_CHILD";
        if let Ok(value) = std::env::var(CHILD_LIMIT) {
            // Close inherited file descriptors from parent environments (e.g. IDEs)
            // to ensure a hermetic environment for RLIMIT testing.
            if let Ok(entries) = fs::read_dir("/proc/self/fd") {
                let fds_to_close: Vec<i32> = entries
                    .filter_map(|e| e.ok())
                    .filter_map(|e| e.file_name().to_str()?.parse::<i32>().ok())
                    .filter(|&fd| fd > 2)
                    .filter(|&fd| {
                        // Do not close the descriptor opened by read_dir itself.
                        fs::read_link(format!("/proc/self/fd/{fd}"))
                            .map(|target| !target.ends_with("fd"))
                            .unwrap_or(true)
                    })
                    .collect();
                for fd in fds_to_close {
                    unsafe { libc::close(fd) };
                }
            }
            let fixture = tempfile::tempdir().unwrap();
            for index in 0..300 {
                fs::write(fixture.path().join(format!("file-{index}")), "x").unwrap();
            }
            let root = crate::fs::scanner::scan_directory(
                fixture.path(),
                None,
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap();
            let mut app = App::new(root);
            let mut limit = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            // This test branch is a disposable subprocess; it never alters the suite's limit.
            assert_eq!(
                unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) },
                0
            );
            limit.rlim_cur = value.parse::<libc::rlim_t>().unwrap().min(limit.rlim_max);
            assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) }, 0);
            let expected = app.selection_limit().unwrap();
            assert!(expected > 0 && expected <= 256);
            app.select_all_visible();
            assert_eq!(app.selected_paths.len(), expected);
            assert!(app
                .current_status()
                .unwrap()
                .contains(&format!("Selected {expected} of 300")));
            assert!(app
                .current_status()
                .unwrap()
                .contains("Selection limit reached"));
            // Leave usable capacity for operations after the cap is reached.
            let extra: Vec<_> = (0..16)
                .map(|_| fs::File::open("/dev/null").unwrap())
                .collect();
            drop(extra);
            let selected = app.selected_paths.iter().next().unwrap().clone();
            app.search_query = selected.file_name().unwrap().to_string_lossy().into_owned();
            // Find the exact selected row even if the search also matches a longer name.
            app.cursor_index = app
                .visible_children()
                .iter()
                .position(|entry| entry.path == selected)
                .unwrap();
            app.toggle_selection();
            assert_eq!(app.selected_paths.len(), expected - 1);
            assert!(app.select_path(selected));
            assert_eq!(app.selected_paths.len(), expected);
            return;
        }
        for limit in ["96", "1024"] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "ui::app::reconciliation_tests::selections_leave_descriptor_headroom_and_report_partial_batches"])
                .env(CHILD_LIMIT, limit).output().unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[test]
    fn bulk_tree_updates_visit_each_cached_node_at_most_once() {
        let fixture = tempfile::tempdir().unwrap();
        for index in 0..512 {
            fs::write(fixture.path().join(format!("file-{index}")), "x").unwrap();
        }
        let root = crate::fs::scanner::scan_directory(
            fixture.path(),
            None,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let mut app = App::new(root);
        let updates = app
            .root_entry
            .children
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let replacement = if index % 2 == 0 {
                    None
                } else {
                    let mut updated = entry.clone();
                    updated.size = 2;
                    Some(updated)
                };
                (entry.path.clone(), replacement)
            })
            .collect();
        let visited = app.apply_tree_updates(updates);
        assert_eq!(visited, 513);
        assert_eq!(app.root_entry.children.len(), 256);
        assert_eq!(app.root_entry.items_count, 257);
        assert_eq!(app.root_entry.size, 512);
    }

    #[test]
    fn partial_failure_updates_ancestors_and_coalesces_nested_targets() {
        let fixture = tempfile::tempdir().unwrap();
        let affected = fixture.path().join("affected");
        fs::create_dir(&affected).unwrap();
        let removed = affected.join("removed");
        let remaining = affected.join("remaining");
        fs::write(&removed, "gone").unwrap();
        fs::write(&remaining, "keep").unwrap();
        let root = crate::fs::scanner::scan_directory(
            fixture.path(),
            None,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let mut app = App::new(root);
        assert!(app.navigate_to_path(&affected));
        app.select_all_visible();
        fs::remove_file(&removed).unwrap();
        assert!(app.reconcile_after_delete_failure(
            &affected,
            &[
                (affected.clone(), "partial failure".into()),
                (remaining.clone(), "nested failure".into()),
            ]
        ));
        assert_eq!(app.current_dir_entry().path, affected);
        assert_eq!(app.current_dir_entry().children.len(), 1);
        assert_eq!(app.root_entry.size, 4);
        assert_eq!(app.root_entry.items_count, 3);
        assert_eq!(app.selected_paths, HashSet::from([remaining]));
    }

    #[test]
    fn unreadable_target_retains_cached_tree_and_marks_it_stale() {
        let fixture = tempfile::tempdir().unwrap();
        let parent = fixture.path().join("parent");
        fs::create_dir(&parent).unwrap();
        let target = parent.join("target");
        fs::write(&target, "keep").unwrap();
        let root = crate::fs::scanner::scan_directory(
            fixture.path(),
            None,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let mut app = App::new(root);
        fs::rename(&parent, fixture.path().join("original")).unwrap();
        // A symlink loop reliably fails metadata resolution even when running as root.
        std::os::unix::fs::symlink("parent", &parent).unwrap();
        assert!(!app.reconcile_after_delete_failure(fixture.path(), &[(target, "failed".into())]));
        assert!(app.root_entry.has_err);
        assert_eq!(app.root_entry.children[0].children.len(), 1);
        assert_eq!(
            fs::read_to_string(fixture.path().join("original/target")).unwrap(),
            "keep"
        );
    }
}

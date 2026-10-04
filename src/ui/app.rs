use crate::fs::entry::{DeleteSafety, FileEntry, GhostKind};
use crate::fs::mount_info::{get_detailed_item_info, query_fs_info, DetailedItemInfo, FsMountInfo};
use crate::fs::scanner::ScannerOptions;
use crate::ghost::{
    classify_path, classify_safety, describe_pid_ghost, fetch_docker_disk_info, proc_start_time,
    prune_docker_dangling, scan_deleted_open_files, DeletedOpenFile, DockerDiskInfo,
};
use crate::ops::delete::permanently_delete_confirmed;
use crate::ops::trash::move_to_trash_confirmed;
use crate::ops::{TargetIdentities, TargetIdentity};
use std::cell::Cell;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveView {
    Filesystem,
    GhostInspector,
    TopFiles,
    Janitor,
    HelpModal,
    ConfirmModal,
    ItemInfoModal,
}

/// Leaderboard size; the ranking heap never holds more entries.
const TOP_FILES_LIMIT: usize = 50;

/// One ranked row of the Top-50 leaderboard. Snapshot data for display; the
/// destructive actions re-verify the live target before touching disk.
#[derive(Debug, Clone)]
pub struct TopFile {
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
    pub disk_usage: u64,
    pub safety: DeleteSafety,
}

/// Scope of the Janitor view: the current directory or the whole scan tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JanitorScope {
    Current,
    Global,
}

/// One toggleable janitor row. A directory unit covers its whole subtree;
/// grouped loose files share their parent row but only trash the listed files.
#[derive(Debug, Clone)]
pub struct JanitorItem {
    pub display: String,
    pub targets: Vec<PathBuf>,
    pub size: u64,
    /// Trash contents cannot be trashed again; only permanent delete applies.
    pub trash_only_delete: bool,
    pub selected: bool,
}

#[derive(Debug, Clone)]
pub struct JanitorCategory {
    pub title: &'static str,
    pub items: Vec<JanitorItem>,
    pub expanded: bool,
    /// Sum of item sizes, computed at build so rendering never re-sums.
    pub total: u64,
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

#[derive(Debug)]
pub enum ConfirmAction {
    MoveToTrash,
    PermanentDelete,
    DockerPrune,
    KillProcess {
        pid: u32,
        name: String,
        start_time: u64,
        /// Pinned handle to the confirmed instance. Signals go through it, so
        /// PID reuse cannot redirect them. `None` only on kernels predating
        /// pidfd (below the app's 5.6 floor): execution refuses without one.
        pidfd: Option<rustix::fd::OwnedFd>,
    },
}

pub struct App {
    pub root_entry: FileEntry,
    /// Scan flags in effect; refreshes reuse them so CLI options persist.
    pub scan_options: ScannerOptions,
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
    /// Scroll offset for the multi-target list in the confirmation modal.
    pub confirm_list_offset: usize,
    pub action_safety_blocked: bool,
    pub action_has_recheck: bool,
    pub action_has_system: bool,
    pub safe_only_filter: bool,

    // Ghost Inspector data
    pub docker_info: DockerDiskInfo,
    pub deleted_open_files: Vec<DeletedOpenFile>,
    pub ghost_tab_index: usize, // 0 = Docker, 1 = Deleted-Open Files
    pub ghost_cursor_index: usize,
    pub top_files: Vec<TopFile>,
    pub top_cursor: usize,
    pub janitor_cats: Vec<JanitorCategory>,
    pub janitor_cursor: usize,
    pub janitor_scope: JanitorScope,
    /// Selected-byte total, refreshed on rebuild/toggle so rendering is O(page).
    pub janitor_selected_bytes: u64,
    pub janitor_offset: Cell<usize>,
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
            scan_options: ScannerOptions::default(),
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
            confirm_list_offset: 0,
            action_safety_blocked: false,
            action_has_recheck: false,
            action_has_system: false,
            safe_only_filter: false,
            docker_info,
            deleted_open_files,
            ghost_tab_index: 0,
            ghost_cursor_index: 0,
            top_files: Vec::new(),
            top_cursor: 0,
            janitor_cats: Vec::new(),
            janitor_cursor: 0,
            janitor_scope: JanitorScope::Global,
            janitor_selected_bytes: 0,
            janitor_offset: Cell::new(0),
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
        // Excluded from line coverage: the syscall cannot fail in-process.
        #[cfg(not(tarpaulin_include))]
        #[allow(unexpected_cfgs)]
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
        self.open_action_confirm(targets, total_size, ConfirmAction::MoveToTrash);
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
        self.open_action_confirm(targets, total_size, ConfirmAction::PermanentDelete);
    }

    /// Shared confirmation gate: system check, identity capture, modal setup.
    fn open_action_confirm(
        &mut self,
        targets: Vec<PathBuf>,
        total_size: u64,
        action: ConfirmAction,
    ) {
        let (has_system, has_recheck) = Self::action_safety_flags(&targets);
        if !has_system && !self.capture_confirmation(&targets) {
            return;
        }
        self.finish_action_confirm(targets, total_size, has_system, has_recheck, action);
    }

    /// System/Recheck classification shared by every confirmation entry point.
    fn action_safety_flags(targets: &[PathBuf]) -> (bool, bool) {
        let mut has_system = false;
        let mut has_recheck = false;
        for path in targets {
            let ghost = classify_path(path);
            let safety = classify_safety(path, ghost);
            if safety == DeleteSafety::System {
                has_system = true;
            } else if safety == DeleteSafety::Recheck {
                has_recheck = true;
            }
        }
        (has_system, has_recheck)
    }

    /// Store targets and open the confirmation modal.
    fn finish_action_confirm(
        &mut self,
        targets: Vec<PathBuf>,
        total_size: u64,
        has_system: bool,
        has_recheck: bool,
        action: ConfirmAction,
    ) {
        self.action_targets = targets;
        self.action_total_size = total_size;
        self.action_has_system = has_system;
        self.action_has_recheck = has_recheck;
        self.action_safety_blocked = has_system;
        self.confirm_list_offset = 0;
        self.pending_action = Some(action);
        self.previous_view = self.active_view;
        self.active_view = ActiveView::ConfirmModal;
    }

    /// Resolve scanned identities for many targets in one tree walk, avoiding
    /// a linear `find_entry` scan per target.
    fn resolve_scan_identities(
        &self,
        targets: &[PathBuf],
    ) -> HashMap<PathBuf, (u64, u64, bool, bool)> {
        let wanted: HashSet<&Path> = targets.iter().map(PathBuf::as_path).collect();
        fn walk(
            entry: &FileEntry,
            wanted: &HashSet<&Path>,
            map: &mut HashMap<PathBuf, (u64, u64, bool, bool)>,
        ) {
            if wanted.contains(entry.path.as_path()) {
                map.insert(
                    entry.path.clone(),
                    (entry.dev, entry.ino, entry.is_dir, entry.is_symlink),
                );
            }
            for child in &entry.children {
                walk(child, wanted, map);
            }
        }
        let mut map = HashMap::new();
        walk(&self.root_entry, &wanted, &mut map);
        map
    }

    /// Capture confirmation identities for a pre-resolved batch, honouring the
    /// same descriptor-aware limit as interactive selection. All-or-nothing,
    /// like `capture_confirmation`.
    fn capture_confirmed_batch(
        &mut self,
        targets: &[PathBuf],
        resolved: &HashMap<PathBuf, (u64, u64, bool, bool)>,
    ) -> bool {
        self.action_identities.clear();
        let limit = match self.selection_limit() {
            Ok(limit) => limit,
            Err(error) => {
                self.set_status(format!("Cannot determine safe selection limit: {error}"));
                return false;
            }
        };
        for path in targets {
            if self.selected_identities.len() + self.action_identities.len() >= limit {
                self.action_identities.clear();
                self.set_status(format!(
                    "Selection limit reached ({limit} items); confirm a smaller batch"
                ));
                return false;
            }
            let identity = match (TargetIdentity::capture(path), resolved.get(path)) {
                (Ok(identity), Some(&(dev, ino, is_dir, is_symlink)))
                    if identity.matches_ids(dev, ino, is_dir, is_symlink) =>
                {
                    identity
                }
                _ => {
                    self.selected_paths.remove(path);
                    self.selected_identities.remove(path);
                    self.action_identities.clear();
                    self.set_status(
                        "Cannot confirm action: Target changed since scan; refresh and select it again",
                    );
                    return false;
                }
            };
            self.action_identities.insert(path.clone(), identity);
        }
        true
    }

    /// Rank the largest files across the scanned tree (files only, no symlinks).
    /// Bounded heap keeps O(50) entries: keys only in pass one, full rows for
    /// winners in pass two. No per-file allocation beyond the traversal itself.
    fn rank_top_files(root: &FileEntry) -> Vec<TopFile> {
        fn collect_keys<'a>(
            entry: &'a FileEntry,
            heap: &mut BinaryHeap<std::cmp::Reverse<(u64, u64, u64, &'a Path)>>,
        ) {
            if !entry.is_dir && !entry.is_symlink {
                heap.push(std::cmp::Reverse((
                    entry.disk_usage,
                    entry.dev,
                    entry.ino,
                    entry.path.as_path(),
                )));
                if heap.len() > TOP_FILES_LIMIT {
                    heap.pop();
                }
            }
            for child in &entry.children {
                collect_keys(child, heap);
            }
        }
        fn collect_winners(entry: &FileEntry, winners: &HashSet<&Path>, out: &mut Vec<TopFile>) {
            if !entry.is_dir && !entry.is_symlink && winners.contains(entry.path.as_path()) {
                out.push(TopFile {
                    path: entry.path.clone(),
                    name: entry.name.clone(),
                    size: entry.size,
                    disk_usage: entry.disk_usage,
                    safety: entry.delete_safety,
                });
            }
            for child in &entry.children {
                collect_winners(child, winners, out);
            }
        }
        let mut heap = BinaryHeap::new();
        collect_keys(root, &mut heap);
        // Winners keyed by path: one inode with many hard links cannot
        // multiply into more rows than the limit.
        let winners: HashSet<&Path> = heap
            .into_iter()
            .map(|std::cmp::Reverse((_, _, _, path))| path)
            .collect();
        let mut files = Vec::with_capacity(winners.len().min(TOP_FILES_LIMIT));
        collect_winners(root, &winners, &mut files);
        // Total order: deterministic across runs and filesystems even on ties.
        files.sort_by(|a, b| {
            b.disk_usage
                .cmp(&a.disk_usage)
                .then_with(|| a.path.cmp(&b.path))
        });
        files
    }

    /// Open the Janitor. Defaults to the current folder when navigating below
    /// the scan root, otherwise the whole tree.
    pub fn open_janitor(&mut self) {
        self.janitor_scope = if self.path_stack.is_empty() {
            JanitorScope::Global
        } else {
            JanitorScope::Current
        };
        self.rebuild_janitor();
        self.janitor_cursor = 0;
        self.previous_view = self.active_view;
        self.active_view = ActiveView::Janitor;
    }

    fn janitor_scope_root(&self) -> &FileEntry {
        match self.janitor_scope {
            JanitorScope::Global => &self.root_entry,
            JanitorScope::Current => self.current_dir_entry(),
        }
    }

    fn rebuild_janitor(&mut self) {
        // Borrow the scope for collection; store the owned rows after it ends.
        let cats = {
            let scope = self.janitor_scope_root();
            Self::collect_janitor(scope)
        };
        self.janitor_cats = cats;
        self.janitor_cursor = self.janitor_cursor.min(self.janitor_row_count().max(1) - 1);
        self.refresh_janitor_selected();
    }

    /// Recompute the selected-byte total. Called on rebuild/toggle (keypress
    /// rate), never during rendering (frame rate).
    fn refresh_janitor_selected(&mut self) {
        self.janitor_selected_bytes = self
            .janitor_cats
            .iter()
            .flat_map(|cat| &cat.items)
            .filter(|item| item.selected)
            .map(|item| item.size)
            .sum();
    }

    /// Flat row count (category headers plus expanded items).
    pub fn janitor_row_count(&self) -> usize {
        self.janitor_cats
            .iter()
            .map(|cat| 1 + if cat.expanded { cat.items.len() } else { 0 })
            .sum()
    }

    /// Flat start index of each category header; O(categories).
    pub(crate) fn janitor_offsets(&self) -> Vec<usize> {
        let mut offsets = Vec::with_capacity(self.janitor_cats.len());
        let mut index = 0;
        for cat in &self.janitor_cats {
            offsets.push(index);
            index += 1 + if cat.expanded { cat.items.len() } else { 0 };
        }
        offsets
    }

    /// Resolve a flat cursor position to (category, item). `None` = header row.
    /// Indexed by category offsets; never scans item rows.
    pub fn janitor_row_at(&self, cursor: usize) -> Option<(usize, Option<usize>)> {
        let offsets = self.janitor_offsets();
        let position = offsets.partition_point(|&start| start <= cursor);
        if position == 0 {
            return None;
        }
        let cat_idx = position - 1;
        let cat = &self.janitor_cats[cat_idx];
        if cursor == offsets[cat_idx] {
            return Some((cat_idx, None));
        }
        let item_idx = cursor - offsets[cat_idx] - 1;
        if cat.expanded && item_idx < cat.items.len() {
            return Some((cat_idx, Some(item_idx)));
        }
        None
    }

    /// Toggle scope between the current folder and the whole tree.
    pub fn toggle_janitor_scope(&mut self) {
        self.janitor_scope = match self.janitor_scope {
            JanitorScope::Current => JanitorScope::Global,
            JanitorScope::Global => JanitorScope::Current,
        };
        self.rebuild_janitor();
        self.janitor_cursor = 0;
        self.set_status(format!(
            "Janitor scope: {}",
            match self.janitor_scope {
                JanitorScope::Current => "current folder",
                JanitorScope::Global => "whole scan",
            }
        ));
    }

    /// Toggle the row under the janitor cursor: a whole category on headers.
    /// Selected bytes update incrementally (O(row), not O(tree)).
    pub fn toggle_janitor_row(&mut self) {
        if let Some((cat_idx, item_idx)) = self.janitor_row_at(self.janitor_cursor) {
            match item_idx {
                None => {
                    let all_on = self.janitor_cats[cat_idx]
                        .items
                        .iter()
                        .all(|item| item.selected);
                    for item in &mut self.janitor_cats[cat_idx].items {
                        if item.selected == all_on {
                            if all_on {
                                self.janitor_selected_bytes =
                                    self.janitor_selected_bytes.saturating_sub(item.size);
                            } else {
                                self.janitor_selected_bytes =
                                    self.janitor_selected_bytes.saturating_add(item.size);
                            }
                        }
                        item.selected = !all_on;
                    }
                }
                Some(item_idx) => {
                    let item = &mut self.janitor_cats[cat_idx].items[item_idx];
                    item.selected = !item.selected;
                    if item.selected {
                        self.janitor_selected_bytes =
                            self.janitor_selected_bytes.saturating_add(item.size);
                    } else {
                        self.janitor_selected_bytes =
                            self.janitor_selected_bytes.saturating_sub(item.size);
                    }
                }
            }
        }
    }

    /// Select all janitor rows, or clear when everything is already selected.
    /// One aggregate pass for the bulk action (explicitly allowed); rebuilds
    /// recompute from scratch.
    pub fn toggle_janitor_all(&mut self) {
        let all_on = self
            .janitor_cats
            .iter()
            .flat_map(|cat| &cat.items)
            .all(|item| item.selected);
        for cat in &mut self.janitor_cats {
            for item in &mut cat.items {
                if item.selected == all_on {
                    if all_on {
                        self.janitor_selected_bytes =
                            self.janitor_selected_bytes.saturating_sub(item.size);
                    } else {
                        self.janitor_selected_bytes =
                            self.janitor_selected_bytes.saturating_add(item.size);
                    }
                }
                item.selected = !all_on;
            }
        }
    }

    /// Trash or delete checked janitor rows through the standard confirmation
    /// flow. Trash contents route to permanent delete with an explanation.
    pub fn janitor_action(&mut self, to_trash: bool) {
        let mut targets = Vec::new();
        let mut total = 0u64;
        let mut trash_only_hit = false;
        for cat in &self.janitor_cats {
            for item in &cat.items {
                if item.selected {
                    if item.trash_only_delete {
                        trash_only_hit = true;
                    }
                    targets.extend(item.targets.iter().cloned());
                    total = total.saturating_add(item.size);
                }
            }
        }
        if targets.is_empty() {
            self.set_status("Nothing selected (Space toggles, a selects all)");
            return;
        }
        if to_trash && trash_only_hit {
            self.set_status("Trash contents can only be permanently deleted (D)");
            return;
        }
        let action = if to_trash {
            ConfirmAction::MoveToTrash
        } else {
            ConfirmAction::PermanentDelete
        };
        let (has_system, has_recheck) = Self::action_safety_flags(&targets);
        if !has_system {
            let resolved = self.resolve_scan_identities(&targets);
            if !self.capture_confirmed_batch(&targets, &resolved) {
                return;
            }
        }
        self.finish_action_confirm(targets, total, has_system, has_recheck, action);
    }

    /// Open the Top-50 leaderboard, ranked on demand from the live tree.
    pub fn open_top_files(&mut self) {
        let files = Self::rank_top_files(&self.root_entry);
        if files.is_empty() {
            self.set_status("No files in the scanned tree");
            return;
        }
        self.top_files = files;
        self.top_cursor = 0;
        self.previous_view = self.active_view;
        self.active_view = ActiveView::TopFiles;
    }

    /// Jump from the leaderboard to the file's parent in the explorer.
    pub fn jump_to_top_file(&mut self) -> bool {
        let target = match self.top_files.get(self.top_cursor) {
            Some(top) => top.path.clone(),
            None => return false,
        };
        let parent = target
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| self.root_entry.path.clone());
        if !self.navigate_to_path(&parent) {
            return false;
        }
        self.active_view = ActiveView::Filesystem;
        let visible = self.visible_children();
        if let Some(index) = visible.iter().position(|e| e.path == target) {
            self.cursor_index = index;
        }
        true
    }

    /// Trash or delete the highlighted leaderboard row through the standard
    /// confirmation flow (live identity re-verified before any mutation).
    pub fn top_file_action(&mut self, to_trash: bool) {
        let Some(top) = self.top_files.get(self.top_cursor).cloned() else {
            return;
        };
        let total = if self.apparent_size {
            top.size
        } else {
            top.disk_usage
        };
        let action = if to_trash {
            ConfirmAction::MoveToTrash
        } else {
            ConfirmAction::PermanentDelete
        };
        self.open_action_confirm(vec![top.path], total, action);
    }

    /// Janitor category of a cleanable entry, if it belongs in the view.
    fn janitor_group(kind: GhostKind) -> Option<(&'static str, usize)> {
        match kind {
            GhostKind::BrowserCache => Some(("🌐 Browser Caches", 0)),
            GhostKind::BuildCache | GhostKind::PackageCache => {
                Some(("📦 Package & Build Caches", 1))
            }
            GhostKind::Trash => Some(("🗑️ FreeDesktop Trash", 2)),
            GhostKind::LogFiles | GhostKind::CoreDump => Some(("📜 Logs & Crash Dumps", 3)),
            _ => None,
        }
    }

    /// Collect toggleable janitor rows under `scope`: cleanable Safe units become
    /// single rows covering their subtree; loose cleanable files group by parent
    /// so only the listed files are ever targeted.
    fn collect_janitor(scope: &FileEntry) -> Vec<JanitorCategory> {
        const TITLES: [&str; 4] = [
            "🌐 Browser Caches",
            "📦 Package & Build Caches",
            "🗑️ FreeDesktop Trash",
            "📜 Logs & Crash Dumps",
        ];
        let mut groups: [Vec<JanitorItem>; 4] = Default::default();
        // (parent, category slot) -> (files, bytes, trash_only).
        let mut loose: HashMap<(PathBuf, usize), (Vec<PathBuf>, u64, bool)> = HashMap::new();
        fn walk(
            node: &FileEntry,
            scope_root: &Path,
            groups: &mut [Vec<JanitorItem>; 4],
            loose: &mut HashMap<(PathBuf, usize), (Vec<PathBuf>, u64, bool)>,
        ) {
            if node.path != scope_root && node.delete_safety == DeleteSafety::Safe {
                if let Some((_, slot)) = App::janitor_group(node.ghost_kind) {
                    let trash_only = node.ghost_kind == GhostKind::Trash;
                    // Generic containers group distinct apps (per-app rows under
                    // ~/.cache, files/ + info/ under Trash); everything else is
                    // one unit covering its subtree.
                    let is_container = node
                        .path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n == ".cache" || n == "Trash");
                    if node.is_dir && !node.is_symlink && !is_container {
                        groups[slot].push(JanitorItem {
                            display: node.path.to_string_lossy().into_owned(),
                            targets: vec![node.path.clone()],
                            size: node.disk_usage,
                            trash_only_delete: trash_only,
                            selected: false,
                        });
                        return; // Unit covers its subtree; do not descend.
                    }
                    if !node.is_dir {
                        let parent = node
                            .path
                            .parent()
                            .map(|p| p.to_path_buf())
                            .unwrap_or_else(|| node.path.clone());
                        let entry = loose
                            .entry((parent, slot))
                            .or_insert((Vec::new(), 0, false));
                        entry.0.push(node.path.clone());
                        entry.1 += node.disk_usage;
                        entry.2 |= trash_only;
                        return;
                    }
                }
            }
            for child in &node.children {
                walk(child, scope_root, groups, loose);
            }
        }
        walk(scope, &scope.path.clone(), &mut groups, &mut loose);
        for ((parent, slot), (mut files, bytes, trash_only)) in loose {
            files.sort();
            groups[slot].push(JanitorItem {
                display: format!("{} ({} files)", parent.display(), files.len()),
                targets: files,
                size: bytes,
                trash_only_delete: trash_only,
                selected: false,
            });
        }
        TITLES
            .into_iter()
            .enumerate()
            .map(|(index, title)| {
                let mut items = std::mem::take(&mut groups[index]);
                items.sort_by_key(|item| std::cmp::Reverse(item.size));
                let total = items.iter().map(|item| item.size).sum();
                JanitorCategory {
                    title,
                    items,
                    expanded: true,
                    total,
                }
            })
            .collect()
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

    /// Execute a confirmed Docker prune. Excluded from line coverage: every
    /// outcome needs a live daemon, and success would mutate real state.
    #[cfg(not(tarpaulin_include))]
    #[allow(unexpected_cfgs)]
    fn execute_docker_prune(&mut self) {
        match prune_docker_dangling() {
            Ok(msg) => {
                self.refresh_ghost_info();
                self.set_status(format!("✔ Docker Prune: {}", msg));
            }
            Err(err) => {
                self.refresh_ghost_info();
                self.set_status(format!("❌ Docker Prune failed: {}", err));
            }
        }
    }

    /// Open the process-termination confirmation for a ghost-table PID. The row,
    /// confirmation, and signal bind to one process instance: the row's recorded
    /// identity must still match the live holder, which must still hold a ghost
    /// file (checked per-PID, never via a full table walk on the key path).
    /// A recycled PID is refused instead of adopted; the modal shows the
    /// freshly validated name, and the instance is pinned with a pidfd.
    /// Callers pass PIDs from the ghost table, never free-form input.
    pub fn prompt_kill_process(&mut self, pid: u32) {
        let recorded = self
            .deleted_open_files
            .iter()
            .find(|f| f.pid == pid)
            .and_then(|f| f.start_time);
        let Some(recorded) = recorded else {
            self.set_status("Process row carries no recorded identity; press r to refresh");
            return;
        };
        // Targeted liveness: only this PID's fd directory is walked.
        let Some((name, start_time)) = describe_pid_ghost(pid) else {
            self.set_status(format!(
                "Process {pid} no longer holds ghost files; press r to refresh"
            ));
            return;
        };
        if start_time != recorded {
            self.set_status(format!(
                "Process {pid} changed since scan; press r to refresh, then select it again"
            ));
            return;
        }
        // Pin the verified instance; a recycled PID names a different process,
        // never this handle. Execution refuses without a pinned handle.
        let pidfd = rustix::process::Pid::from_raw(pid as i32).and_then(|target| {
            rustix::process::pidfd_open(target, rustix::process::PidfdFlags::empty()).ok()
        });
        // The handle pins whoever holds the PID now; refuse if that is already
        // a different instance than the verified one.
        if pidfd.is_some() && proc_start_time(pid) != Some(start_time) {
            self.set_status(format!(
                "Process {pid} changed during confirmation setup; not opened"
            ));
            return;
        }
        self.pending_action = Some(ConfirmAction::KillProcess {
            pid,
            name,
            start_time,
            pidfd,
        });
        self.previous_view = self.active_view;
        self.active_view = ActiveView::ConfirmModal;
    }

    /// Signal the stored confirmed kill target. Consumes the confirmation and
    /// refuses when the process instance changed since confirmation.
    /// `sigterm` selects SIGTERM (graceful) over SIGKILL. Kernel ESRCH/EPERM
    /// surface as status messages.
    /// Signal the stored confirmed kill target through its pinned handle, which
    /// is immune to PID reuse. Refuses when no handle could be pinned: there is
    /// no numeric-PID fallback, so a recycled PID can never be signalled.
    /// `sigterm` selects SIGTERM (graceful) over SIGKILL. Kernel errors surface
    /// as status messages.
    pub fn execute_kill(&mut self, sigterm: bool) {
        let confirmed = self.pending_action.take();
        self.active_view = self.previous_view;
        let Some(ConfirmAction::KillProcess {
            pid,
            name: _,
            start_time,
            pidfd: Some(fd),
        }) = confirmed
        else {
            self.set_status("Cannot signal: process was not pinned at confirmation");
            return;
        };
        // The handle pins the confirmed instance, but refuse when the current
        // holder already moved on: signalling it deserves refusal, not ESRCH.
        if proc_start_time(pid) != Some(start_time) {
            self.set_status(format!(
                "Process {pid} changed since confirmation; not signalled"
            ));
            return;
        }
        let signal = if sigterm {
            rustix::process::Signal::TERM
        } else {
            rustix::process::Signal::KILL
        };
        if let Err(error) = rustix::process::pidfd_send_signal(&fd, signal) {
            self.set_status(format!(
                "Cannot signal process {pid}: {}",
                std::io::Error::from(error)
            ));
            return;
        }
        // Drop the signalled rows surgically; the next manual refresh
        // re-scans the table. No full procfs walk on the key path.
        self.deleted_open_files.retain(|f| f.pid != pid);
        self.ghost_cursor_index = self
            .ghost_cursor_index
            .min(self.deleted_open_files.len().saturating_sub(1));
        self.set_status(format!("Signaled process {pid}"));
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
            ConfirmAction::DockerPrune => self.execute_docker_prune(),
            // Termination uses the 1/2 number keys, never y. Reaching here
            // means an unexpected confirm path; cancel without signaling.
            ConfirmAction::KillProcess { .. } => {
                self.cancel_modal();
            }
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
        // The leaderboard snapshot predates the mutation; rebuild it in place.
        if self.active_view == ActiveView::TopFiles {
            self.top_files = Self::rank_top_files(&self.root_entry);
            self.top_cursor = self.top_cursor.min(self.top_files.len().saturating_sub(1));
        }
        if self.active_view == ActiveView::Janitor {
            self.rebuild_janitor();
        }
    }

    /// Refresh one subtree after external changes (e.g. subshell exit) using the
    /// active scan policy. Returns false when the rescan itself failed.
    pub fn refresh_path(&mut self, path: &Path) -> bool {
        let roots: HashSet<PathBuf> = HashSet::from([path.to_path_buf()]);
        let mut seen = HashSet::new();
        Self::seed_retained_inodes(&self.root_entry, &roots, &mut seen);
        let base_depth = path
            .strip_prefix(&self.root_entry.path)
            .map(|p| p.components().count())
            .unwrap_or(0);
        match crate::fs::scanner::rescan_entry(
            path,
            self.root_entry.dev,
            &mut seen,
            &self.scan_options,
            base_depth,
        ) {
            Ok(entry) => {
                if path == self.root_entry.path {
                    let current = self.current_dir_entry().path.clone();
                    self.root_entry = entry;
                    self.navigate_to_path(&current);
                } else {
                    self.replace_subtree(path, entry);
                }
                self.discard_changed_selections();
                true
            }
            Err(_) => false,
        }
    }

    /// Seed hard-link accounting from the retained tree, stopping at rescan roots
    /// so their descendants are not double-counted.
    fn seed_retained_inodes(
        entry: &FileEntry,
        roots: &HashSet<PathBuf>,
        seen: &mut HashSet<(u64, u64)>,
    ) {
        // Stop at each rescan root, so descendants need no prefix comparisons.
        if roots.contains(&entry.path) {
            return;
        }
        if entry.is_dir || entry.size > 0 || entry.disk_usage > 0 || entry.safe_items > 0 {
            seen.insert((entry.dev, entry.ino));
        }
        for child in &entry.children {
            Self::seed_retained_inodes(child, roots, seen);
        }
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
        fn has_errors(entry: &FileEntry) -> bool {
            entry.has_err || entry.children.iter().any(has_errors)
        }
        let mut seen = HashSet::new();
        Self::seed_retained_inodes(
            &self.root_entry,
            &roots.iter().cloned().collect(),
            &mut seen,
        );
        let mut reconciled = true;
        let mut updates = HashMap::new();
        for path in roots {
            // Depth relative to the scan root keeps the original budget;
            // unknown layouts fall back to full depth (previous behavior).
            let base_depth = path
                .strip_prefix(&self.root_entry.path)
                .map(|p| p.components().count())
                .unwrap_or(0);
            match crate::fs::scanner::rescan_entry(
                &path,
                self.root_entry.dev,
                &mut seen,
                &self.scan_options,
                base_depth,
            ) {
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
            if let Ok(new_root) = crate::fs::scanner::scan_directory_with_options(
                &self.root_entry.path,
                None,
                stop_signal,
                self.scan_options.clone(),
            ) {
                self.root_entry = new_root;
            }
        } else {
            // A subtree refresh restarts traversal at the current directory, so
            // shrink the depth budget by its level below the scan root. A zero
            // remainder rescans the directory alone (see ScannerOptions).
            let mut options = self.scan_options.clone();
            if let Some(max) = options.max_depth {
                let depth = current_path
                    .strip_prefix(&self.root_entry.path)
                    .map(|p| p.components().count())
                    .unwrap_or(0);
                options.max_depth = Some(max.saturating_sub(depth));
            }
            if let Ok(new_subtree) = crate::fs::scanner::scan_directory_with_options(
                &current_path,
                None,
                stop_signal,
                options,
            ) {
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
    fn janitor_collects_toggleable_units_and_groups_loose_files() {
        let fixture = tempfile::tempdir().unwrap();
        // ~/.cache style: dir unit with a nested file (covered whole).
        let chrome = fixture.path().join(".cache").join("google-chrome");
        fs::create_dir_all(&chrome).unwrap();
        fs::write(chrome.join("data"), vec![0u8; 10000]).unwrap();
        // Loose cleanable files group under their parent.
        fs::write(fixture.path().join("app.log"), vec![0u8; 1000]).unwrap();
        fs::write(fixture.path().join("old.log.1"), vec![0u8; 1000]).unwrap();
        // Non-cleanable file must never appear.
        fs::write(fixture.path().join("notes.txt"), "x").unwrap();
        let root = crate::fs::scanner::scan_directory(
            fixture.path(),
            None,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let mut app = App::new(root);
        app.open_janitor();
        assert_eq!(app.active_view, ActiveView::Janitor);
        // At scan root the default scope is global.
        assert_eq!(app.janitor_scope, JanitorScope::Global);
        let unit = app
            .janitor_cats
            .iter()
            .flat_map(|cat| &cat.items)
            .find(|item| item.display.contains("google-chrome"))
            .expect("cache dir unit");
        assert_eq!(unit.targets.len(), 1);
        let grouped = app
            .janitor_cats
            .iter()
            .flat_map(|cat| &cat.items)
            .find(|item| item.display.contains("(2 files)"))
            .expect("grouped logs");
        assert_eq!(grouped.targets.len(), 2);
        assert!(!app
            .janitor_cats
            .iter()
            .flat_map(|cat| &cat.items)
            .any(|item| item.display.contains("notes.txt")));
        // Header toggle selects the whole category, cursor rows resolve.
        assert!(app.janitor_row_count() > 0);
        app.toggle_janitor_all();
        assert!(app
            .janitor_cats
            .iter()
            .flat_map(|cat| &cat.items)
            .all(|item| item.selected));
        let selected_total: u64 = app
            .janitor_cats
            .iter()
            .flat_map(|cat| &cat.items)
            .map(|item| item.size)
            .sum();
        assert_eq!(app.janitor_selected_bytes, selected_total);
        // Single-row toggle adjusts the total incrementally and reversibly.
        app.toggle_janitor_all();
        assert_eq!(app.janitor_selected_bytes, 0);
        app.janitor_cursor = 1;
        app.toggle_janitor_row();
        let one = app.janitor_selected_bytes;
        assert!(one > 0);
        app.toggle_janitor_row();
        assert_eq!(app.janitor_selected_bytes, 0);
        // Action routes through the standard trash confirmation.
        app.toggle_janitor_all();
        app.janitor_action(true);
        assert_eq!(app.active_view, ActiveView::ConfirmModal);
        assert!(!app.action_targets.is_empty());
    }

    #[test]
    fn confirm_list_scrolls_through_frozen_batch() {
        use crate::ui::handle_key_event;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let fixture = tempfile::tempdir().unwrap();
        let chrome = fixture.path().join(".cache").join("google-chrome");
        fs::create_dir_all(&chrome).unwrap();
        fs::write(chrome.join("data"), vec![0u8; 10000]).unwrap();
        fs::write(fixture.path().join("a.log"), vec![0u8; 1000]).unwrap();
        fs::write(fixture.path().join("b.log"), vec![0u8; 1000]).unwrap();
        let root = crate::fs::scanner::scan_directory(
            fixture.path(),
            None,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let mut app = App::new(root);
        app.open_janitor();
        app.toggle_janitor_all();
        app.janitor_action(true);
        assert_eq!(app.active_view, ActiveView::ConfirmModal);
        assert!(app.action_targets.len() > 1);
        assert_eq!(app.confirm_list_offset, 0);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        handle_key_event(&mut app, key(KeyCode::Down));
        assert_eq!(app.confirm_list_offset, 1);
        handle_key_event(&mut app, key(KeyCode::Down));
        handle_key_event(&mut app, key(KeyCode::Down));
        assert_eq!(
            app.confirm_list_offset,
            app.action_targets.len().saturating_sub(1)
        );
        handle_key_event(&mut app, key(KeyCode::Up));
        assert_eq!(
            app.confirm_list_offset,
            app.action_targets.len().saturating_sub(2)
        );
    }

    #[test]
    fn top_files_rank_jump_and_confirm() {
        let fixture = tempfile::tempdir().unwrap();
        fs::write(fixture.path().join("small.txt"), "x").unwrap();
        fs::write(fixture.path().join("big.txt"), vec![0u8; 10000]).unwrap();
        let sub = fixture.path().join("sub");
        fs::create_dir(&sub).unwrap();
        fs::write(sub.join("mid.txt"), vec![0u8; 1000]).unwrap();
        let root = crate::fs::scanner::scan_directory(
            fixture.path(),
            None,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let mut app = App::new(root);
        app.open_top_files();
        assert_eq!(app.active_view, ActiveView::TopFiles);
        assert_eq!(app.top_files.len(), 3);
        assert_eq!(app.top_files[0].name, "big.txt");
        // Jump lands the explorer cursor on the file's parent entry. Look the
        // row up by name: small block counts can tie across filesystems.
        app.top_cursor = app
            .top_files
            .iter()
            .position(|f| f.name == "mid.txt")
            .unwrap();
        assert!(app.jump_to_top_file());
        assert_eq!(app.active_view, ActiveView::Filesystem);
        let visible = app.visible_children();
        assert_eq!(visible[app.cursor_index].name, "mid.txt");
        // Direct action opens the standard confirmation for the row.
        app.open_top_files();
        app.top_file_action(true);
        assert_eq!(app.active_view, ActiveView::ConfirmModal);
        assert_eq!(app.action_targets.len(), 1);
    }

    #[test]
    fn jump_to_missing_parent_reports_status() {
        let fixture = tempfile::tempdir().unwrap();
        std::fs::write(fixture.path().join("stay.txt"), "x").unwrap();
        let root = crate::fs::scanner::scan_directory(
            fixture.path(),
            None,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let mut app = App::new(root);
        app.open_top_files();
        // A row no live tree path can resolve: jump fails, view stays usable.
        app.top_files.push(TopFile {
            path: PathBuf::from("/nonexistent-xyz/deep/file.txt"),
            name: "file.txt".to_string(),
            size: 1,
            disk_usage: 1,
            safety: DeleteSafety::UserData,
        });
        app.top_cursor = app.top_files.len() - 1;
        assert!(!app.jump_to_top_file());
        assert_eq!(app.active_view, ActiveView::TopFiles);
    }

    #[test]
    fn kill_flow_confirms_stale_table_then_refuses_recycled_pid() {
        let fixture = tempfile::tempdir().unwrap();
        let root = crate::fs::scanner::scan_directory(
            fixture.path(),
            None,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let mut app = App::new(root);
        // Unknown PID: confirmation never opens, table refresh reported.
        app.prompt_kill_process(2_147_000_000);
        assert!(app.pending_action.is_none());
        assert_ne!(app.active_view, ActiveView::ConfirmModal);
        // Row recorded a different instance than the live PID holder:
        // replacement refused, never adopted into a confirmation. The test
        // process holds a deleted file so the targeted check reaches the
        // instance comparison instead of stopping at "no ghost file".
        let held = tempfile::NamedTempFile::new().unwrap();
        fs::write(held.path(), vec![0u8; 100]).unwrap();
        fs::remove_file(held.path()).unwrap();
        let own = std::process::id();
        app.deleted_open_files.push(DeletedOpenFile {
            pid: own,
            process_name: "test-proc".to_string(),
            original_path: "/deleted".to_string(),
            size: 100,
            fd: "3".to_string(),
            start_time: Some(u64::MAX),
        });
        app.prompt_kill_process(own);
        assert!(app.pending_action.is_none());
        assert!(app
            .current_status()
            .unwrap_or("")
            .contains("changed since scan"));
        // Same instance, still holding: confirmation opens with a pin.
        app.deleted_open_files.retain(|f| f.pid != own);
        app.deleted_open_files.push(DeletedOpenFile {
            pid: own,
            process_name: "test-proc".to_string(),
            original_path: "/deleted".to_string(),
            size: 100,
            fd: "3".to_string(),
            start_time: crate::ghost::proc_start_time(own),
        });
        app.prompt_kill_process(own);
        assert!(matches!(
            app.pending_action,
            Some(ConfirmAction::KillProcess { .. })
        ));
        app.cancel_modal();
        // Recycled PID: stored start time mismatches the live process.
        // Positive PID below i32::MAX that cannot exist (default pid_max is
        // far lower); kill(-1) would signal everything, so never test that.
        app.pending_action = Some(ConfirmAction::KillProcess {
            pid: 1,
            name: "init".to_string(),
            start_time: u64::MAX,
            pidfd: None,
        });
        app.active_view = ActiveView::ConfirmModal;
        app.execute_kill(true);
        assert!(app.pending_action.is_none());
        assert!(app.current_status().unwrap_or("").contains("not pinned"));
        // Sanity: the current process has a readable start time.
        assert!(crate::ghost::proc_start_time(std::process::id()).is_some());
    }

    #[test]
    fn kill_through_pidfd_terminates_only_the_pinned_child() {
        use std::process::Command;
        let fixture = tempfile::tempdir().unwrap();
        let root = crate::fs::scanner::scan_directory(
            fixture.path(),
            None,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let mut app = App::new(root);
        let mut child = Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("sleep must exist for the kill test");
        let pid = child.id();
        let start_time = crate::ghost::proc_start_time(pid).expect("child is alive");
        let pidfd = rustix::process::Pid::from_raw(pid as i32)
            .and_then(|target| {
                rustix::process::pidfd_open(target, rustix::process::PidfdFlags::empty()).ok()
            })
            .expect("pidfd must open on this kernel");
        app.pending_action = Some(ConfirmAction::KillProcess {
            pid,
            name: "sleep".to_string(),
            start_time,
            pidfd: Some(pidfd),
        });
        app.active_view = ActiveView::ConfirmModal;
        app.execute_kill(true);
        // SIGTERM ends the child; the confirmation is consumed either way.
        let exited = child.wait().expect("child reaped");
        assert!(app.pending_action.is_none());
        assert!(!exited.success());
        assert!(app
            .current_status()
            .unwrap_or("")
            .contains("Signaled process"));
    }

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

#[cfg(test)]
mod selection_limit_tests {
    use super::*;
    use std::sync::{atomic::AtomicBool, Arc};

    fn app_with_files(count: usize) -> (tempfile::TempDir, App) {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..count {
            std::fs::write(dir.path().join(format!("f{i:04}")), "x").unwrap();
        }
        let root = crate::fs::scanner::scan_directory(
            dir.path(),
            None,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let app = App::new(root);
        (dir, app)
    }

    #[test]
    fn selection_gates_and_capture_failures() {
        let (_dir, mut app) = app_with_files(3);
        let target = app.root_entry.children[0].path.clone();
        // Re-selecting is a no-op success.
        let limit = app.selection_limit();
        assert!(app.select_path_with_limit(target.clone(), limit.as_ref()));
        assert!(app.select_path_with_limit(target.clone(), limit.as_ref()));
        // Zero budget refuses with guidance (on a not-yet-selected path).
        // All failure arms below reuse it: failures never insert selections.
        let fresh = app.root_entry.children[1].path.clone();
        let zero = 0usize;
        assert!(!app.select_path_with_limit(fresh.clone(), Ok(&zero)));
        // Limit errors fail closed in both entry points.
        let err = std::io::Error::other("no fds");
        assert!(!app.select_path_with_limit(fresh.clone(), Err(&err)));
        let entry = app.find_entry(&fresh).unwrap();
        assert!(!app.select_scanned_entry_with_limit(
            fresh.clone(),
            entry.dev,
            entry.ino,
            entry.is_dir,
            entry.is_symlink,
            Err(&err),
        ));
        // Identity mismatch and missing paths fail closed.
        assert!(!app.select_scanned_entry_with_limit(
            fresh.clone(),
            999999,
            888888,
            false,
            false,
            limit.as_ref(),
        ));
        assert!(!app.select_scanned_entry_with_limit(
            PathBuf::from("/definitely/not/here-12345"),
            1,
            2,
            false,
            false,
            limit.as_ref(),
        ));
        assert!(!app.select_path_with_limit(
            PathBuf::from("/definitely/not/here-12345"),
            limit.as_ref(),
        ));
        // Capture confirmation fails closed on unknown targets.
        assert!(!app.capture_verified_identity(Path::new("/definitely/not/here-12345")).is_ok());
    }

    #[test]
    fn select_all_reports_partial_batches() {
        let (_dir, mut app) = app_with_files(300);
        app.select_all_visible();
        // 256-item cap stops the batch with an explanatory status.
        assert!(app.selected_paths.len() <= 256);
        assert!(app
            .current_status()
            .unwrap_or("")
            .contains("additional items"));
    }

    #[test]
    fn modes_labels_and_ghost_refresh() {
        assert_eq!(GhostFilterMode::ShowAll.label(), "All Files");
        assert_eq!(GhostFilterMode::HideGhost.next(), GhostFilterMode::GhostOnly);
        assert_eq!(GhostFilterMode::GhostOnly.next(), GhostFilterMode::ShowAll);
        assert_eq!(SortMode::ByItems.next(), SortMode::BySizeDesc);
        assert_eq!(SortMode::BySizeDesc.label(), "Size (desc)");
        let (_dir, mut app) = app_with_files(1);
        app.refresh_ghost_info();
        assert!(app
            .current_status()
            .unwrap_or("")
            .contains("Docker"));
        // Mutable current-dir access walks the stack.
        app.current_dir_entry_mut().size = 123;
        assert_eq!(app.current_dir_entry().size, 123);
    }
}

#[cfg(test)]
mod prompt_branch_tests {
    use super::*;
    use crate::ghost::docker::DockerItemSummary;
    use crate::ghost::DockerDiskInfo;
    use std::sync::{atomic::AtomicBool, Arc};

    fn app_with_files() -> (tempfile::TempDir, App) {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(dir.path().join("a.txt"), "aaaaaaaaaa").unwrap();
        std::fs::write(sub.join("b.txt"), "b").unwrap();
        let root = crate::fs::scanner::scan_directory(
            dir.path(),
            None,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let app = App::new(root);
        (dir, app)
    }

    fn docker_items() -> DockerDiskInfo {
        DockerDiskInfo {
            is_available: true,
            items: vec![
                DockerItemSummary {
                    category: "Image",
                    id_or_name: "i1".to_string(),
                    size: 10,
                    is_reclaimable: true,
                    details: "d".to_string(),
                },
                DockerItemSummary {
                    category: "Image",
                    id_or_name: "i2".to_string(),
                    size: 20,
                    is_reclaimable: false,
                    details: "d".to_string(),
                },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn misc_branches() {
        assert_eq!(GhostFilterMode::GhostOnly.label(), "Ghost Files ONLY");
        let (_dir, mut app) = app_with_files();
        // Mutable descent below the root.
        app.navigate_to_path(&app.root_entry.children[0].path.clone());
        app.current_dir_entry_mut().size = 7;
        assert_eq!(app.current_dir_entry().size, 7);
        // Docker-tab cursor movement with fabricated rows.
        app.docker_info = docker_items();
        app.ghost_tab_index = 0;
        app.active_view = ActiveView::GhostInspector;
        app.cursor_down();
        assert_eq!(app.ghost_cursor_index, 1);
        app.cursor_down();
        assert_eq!(app.ghost_cursor_index, 1);
        app.cursor_up();
        app.page_down(5);
        app.page_up(5);
        app.cursor_to_start();
        app.cursor_to_end();
        // Scan root without a parent has nowhere to ascend.
        app.root_entry.path = PathBuf::from("/");
        app.path_stack.clear();
        assert!(app.go_up().is_none());
    }

    #[test]
    fn prompt_target_sources_and_safety_flags() {
        let (_dir, mut app) = app_with_files();
        // Selected set feeds the prompt.
        app.toggle_selection();
        assert!(!app.selected_paths.is_empty());
        app.prompt_move_to_trash();
        assert_eq!(app.active_view, ActiveView::ConfirmModal);
        app.cancel_modal();
        app.prompt_permanent_delete();
        assert_eq!(app.active_view, ActiveView::ConfirmModal);
        app.cancel_modal();
        app.selected_paths.clear();
        app.selected_identities.clear();
        // Empty view prompts report instead of opening.
        app.safe_only_filter = true;
        app.prompt_move_to_trash();
        assert_ne!(app.active_view, ActiveView::ConfirmModal);
        app.prompt_permanent_delete();
        assert_ne!(app.active_view, ActiveView::ConfirmModal);
        app.safe_only_filter = false;
        // Safety flags route through the shared gate (pure path logic).
        let (system, _) =
            App::action_safety_flags(&[PathBuf::from("/etc/hostname")]);
        assert!(system);
        let (_, recheck) =
            App::action_safety_flags(&[PathBuf::from("/proj/node_modules/pkg")]);
        assert!(recheck);
        app.cancel_modal();
    }
}

#[cfg(test)]
mod janitor_edge_tests {
    use super::*;
    use std::sync::{atomic::AtomicBool, Arc};

    fn scan(path: &Path) -> FileEntry {
        crate::fs::scanner::scan_directory(path, None, Arc::new(AtomicBool::new(false))).unwrap()
    }

    #[test]
    fn janitor_groups_cover_all_kinds() {
        assert!(App::janitor_group(GhostKind::BrowserCache).is_some());
        assert!(App::janitor_group(GhostKind::BuildCache).is_some());
        assert!(App::janitor_group(GhostKind::PackageCache).is_some());
        assert!(App::janitor_group(GhostKind::Trash).is_some());
        assert!(App::janitor_group(GhostKind::LogFiles).is_some());
        assert!(App::janitor_group(GhostKind::CoreDump).is_some());
        assert!(App::janitor_group(GhostKind::None).is_none());
        assert!(App::janitor_group(GhostKind::DependencyTree).is_none());
        assert!(App::janitor_group(GhostKind::VmOrIso).is_none());
    }

    #[test]
    fn janitor_row_lookup_and_scope() {
        let dir = tempfile::tempdir().unwrap();
        let root = scan(dir.path());
        let mut app = App::new(root);
        // Empty categories: any cursor resolves to None.
        assert!(app.janitor_row_at(9999).is_none());
        // Scope toggle rebuilds and resets the cursor.
        app.open_janitor();
        app.toggle_janitor_scope();
        assert_eq!(app.janitor_scope, JanitorScope::Current);
        assert_eq!(app.janitor_cursor, 0);
        app.toggle_janitor_scope();
        assert_eq!(app.janitor_scope, JanitorScope::Global);
    }

    #[test]
    fn janitor_header_toggle_and_trash_only_gate() {
        let dir = tempfile::tempdir().unwrap();
        let trash_files = dir.path().join(".local/share/Trash/files");
        std::fs::create_dir_all(&trash_files).unwrap();
        std::fs::write(trash_files.join("old.txt"), vec![0u8; 1000]).unwrap();
        std::fs::write(dir.path().join("app.log"), vec![0u8; 1000]).unwrap();
        let mut app = App::new(scan(dir.path()));
        app.open_janitor();
        // Header toggle flips its whole category both ways.
        let trash_header = (0..app.janitor_row_count())
            .find(|&cursor| {
                matches!(app.janitor_row_at(cursor), Some((cat, None))
                    if !app.janitor_cats[cat].items.is_empty())
            })
            .unwrap();
        app.janitor_cursor = trash_header;
        app.toggle_janitor_row();
        assert!(app
            .janitor_cats
            .iter()
            .flat_map(|cat| &cat.items)
            .any(|item| item.selected));
        app.toggle_janitor_row();
        assert!(app
            .janitor_cats
            .iter()
            .flat_map(|cat| &cat.items)
            .all(|item| !item.selected));
        // Permanent delete on a normal selection opens the confirm flow.
        app.toggle_janitor_all();
        app.janitor_action(false);
        assert_eq!(app.active_view, ActiveView::ConfirmModal);
        app.cancel_modal();
        // Trash-category rows refuse the trash path with guidance.
        for cat in &mut app.janitor_cats {
            for item in &mut cat.items {
                item.selected = item.trash_only_delete;
            }
        }
        assert!(app
            .janitor_cats
            .iter()
            .flat_map(|cat| &cat.items)
            .any(|item| item.selected));
        app.janitor_action(true);
        assert_ne!(app.active_view, ActiveView::ConfirmModal);
        assert!(app
            .current_status()
            .unwrap_or("")
            .contains("permanently deleted"));
    }

    #[test]
    fn oversized_batches_abort_before_capturing() {
        // 300 loose logs in one directory exceed the 256-identity cap.
        let dir = tempfile::tempdir().unwrap();
        for i in 0..300 {
            std::fs::write(dir.path().join(format!("rotated-{i}.log")), "x").unwrap();
        }
        let mut app = App::new(scan(dir.path()));
        app.open_janitor();
        app.toggle_janitor_all();
        app.janitor_action(true);
        assert_ne!(app.active_view, ActiveView::ConfirmModal);
        assert!(app
            .current_status()
            .unwrap_or("")
            .contains("smaller batch"));
    }

    #[test]
    fn changed_targets_abort_batch_confirmation() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.log"), "x").unwrap();
        std::fs::write(dir.path().join("b.log"), "x").unwrap();
        let mut app = App::new(scan(dir.path()));
        app.open_janitor();
        app.toggle_janitor_all();
        // Remove a target out from under the snapshot: fail closed.
        std::fs::remove_file(dir.path().join("a.log")).unwrap();
        app.janitor_action(true);
        assert_ne!(app.active_view, ActiveView::ConfirmModal);
        assert!(app
            .current_status()
            .unwrap_or("")
            .contains("Target changed"));
    }

    #[test]
    fn top_files_empty_and_apparent() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(scan(dir.path()));
        app.open_top_files();
        assert_ne!(app.active_view, ActiveView::TopFiles);
        assert!(app
            .current_status()
            .unwrap_or("")
            .contains("No files"));
        // Apparent-size display path for direct actions.
        std::fs::write(dir.path().join("f.txt"), vec![0u8; 100]).unwrap();
        let mut app = App::new(scan(dir.path()));
        app.apparent_size = true;
        app.open_top_files();
        app.top_file_action(false);
        assert_eq!(app.active_view, ActiveView::ConfirmModal);
        app.cancel_modal();
    }
}

#[cfg(test)]
mod execution_tests {
    use super::*;
    use std::sync::{atomic::AtomicBool, Arc};

    fn scanned(dir: &std::path::Path) -> FileEntry {
        crate::fs::scanner::scan_directory(dir, None, Arc::new(AtomicBool::new(false))).unwrap()
    }

    #[test]
    fn execute_trash_and_delete_in_tempdir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("trash-me.txt"), "x").unwrap();
        std::fs::write(dir.path().join("delete-me.txt"), "x").unwrap();
        // Scope trash into the fixture so nothing touches the real wastebin.
        let saved_xdg = std::env::var_os("XDG_DATA_HOME");
        unsafe { std::env::set_var("XDG_DATA_HOME", dir.path().join("xdg")) };
        let mut app = App::new(scanned(dir.path()));
        // Trash flow succeeds end to end.
        app.cursor_index = app
            .visible_children()
            .iter()
            .position(|e| e.name == "trash-me.txt")
            .unwrap();
        app.toggle_selection();
        app.prompt_move_to_trash();
        app.execute_pending_action();
        assert!(app
            .current_status()
            .unwrap_or("")
            .contains("Wastebin"));
        assert!(!dir.path().join("trash-me.txt").exists());
        // Permanent delete flow succeeds end to end.
        app.cursor_index = app
            .visible_children()
            .iter()
            .position(|e| e.name == "delete-me.txt")
            .unwrap();
        app.toggle_selection();
        app.prompt_permanent_delete();
        app.execute_pending_action();
        assert!(!dir.path().join("delete-me.txt").exists());
        // Empty confirmation restores the view without acting.
        app.execute_pending_action();
        if let Some(saved) = saved_xdg {
            unsafe { std::env::set_var("XDG_DATA_HOME", saved) };
        } else {
            unsafe { std::env::remove_var("XDG_DATA_HOME") };
        }
    }

    #[test]
    fn docker_prune_prompt_opens_without_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(scanned(dir.path()));
        app.prompt_docker_prune();
        assert_eq!(app.active_view, ActiveView::ConfirmModal);
        assert!(matches!(
            app.pending_action,
            Some(ConfirmAction::DockerPrune)
        ));
        app.cancel_modal();
    }

    #[test]
    fn kill_sigkill_path_and_ghost_cursor_moves() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(scanned(dir.path()));
        // Ghost cursor movement across fabricated rows.
        app.docker_info = DockerDiskInfo {
            is_available: true,
            ..Default::default()
        };
        app.active_view = ActiveView::GhostInspector;
        app.cursor_down();
        app.cursor_up();
        app.page_down(5);
        app.page_up(5);
        app.cursor_to_start();
        app.cursor_to_end();
        // SIGKILL branch on a pinned child.
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("sleep must exist");
        let pid = child.id();
        let start = crate::ghost::proc_start_time(pid).expect("child alive");
        let pidfd = rustix::process::Pid::from_raw(pid as i32)
            .and_then(|t| {
                rustix::process::pidfd_open(t, rustix::process::PidfdFlags::empty()).ok()
            })
            .expect("pidfd on this kernel");
        app.pending_action = Some(ConfirmAction::KillProcess {
            pid,
            name: "sleep".to_string(),
            start_time: start,
            pidfd: Some(pidfd),
        });
        app.active_view = ActiveView::ConfirmModal;
        app.execute_kill(false);
        let exited = child.wait().expect("child reaped");
        assert!(!exited.success());
    }
}

#[cfg(test)]
mod refresh_lookup_tests {
    use super::*;
    use std::sync::{atomic::AtomicBool, Arc};

    fn scanned(dir: &std::path::Path) -> FileEntry {
        crate::fs::scanner::scan_directory(dir, None, Arc::new(AtomicBool::new(false))).unwrap()
    }

    #[test]
    fn refresh_all_covers_root_subtree_and_modal() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("b.txt"), "b").unwrap();
        let mut app = App::new(scanned(dir.path()));
        // Root refresh rescans everything.
        app.refresh_all();
        // Subtree refresh with a depth budget.
        app.scan_options.max_depth = Some(2);
        app.navigate_to_path(&sub);
        app.refresh_all();
        // Item-info modal gets fresh details on refresh.
        app.open_item_info();
        assert_eq!(app.active_view, ActiveView::ItemInfoModal);
        app.refresh_all();
        // Cursor clamp at the end of a shrunken list.
        app.active_view = ActiveView::Filesystem;
        app.cursor_index = 9999;
        app.refresh_all();
        assert!(app.cursor_index < 9999);
    }

    #[test]
    fn find_entry_resolves_all_shapes() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("b.txt"), "b").unwrap();
        let app = App::new(scanned(dir.path()));
        assert!(app.find_entry(Path::new("/definitely/not/here")).is_none());
        assert!(app.find_entry(&app.root_entry.path).is_some());
        assert!(app.find_entry(&sub.join("b.txt")).is_some());
        assert!(app.find_entry(&sub.join("missing.txt")).is_none());
    }

    #[test]
    fn refresh_path_covers_root_failure_and_subtree() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("b.txt"), "b").unwrap();
        let mut app = App::new(scanned(dir.path()));
        assert!(app.refresh_path(&sub));
        assert!(app.refresh_path(&app.root_entry.path.clone()));
        assert!(!app.refresh_path(Path::new("/definitely/not/here-12345")));
    }

    #[test]
    fn failed_delete_with_broken_rescan_marks_stale() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("victim.txt"), "x").unwrap();
        let mut app = App::new(scanned(dir.path()));
        // Fail the delete (no captured identity), then break rescans by
        // locking the whole tree: rescan reports the stale tree.
        let target = sub.join("victim.txt");
        app.action_targets = vec![target.clone()];
        app.action_identities = TargetIdentities::new();
        app.pending_action = Some(ConfirmAction::PermanentDelete);
        app.previous_view = ActiveView::Filesystem;
        app.active_view = ActiveView::ConfirmModal;
        // Make the rescan fail with a non-NotFound error: lock the root.
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(dir.path()).unwrap().permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(dir.path(), perms).unwrap();
        app.execute_pending_action();
        assert!(app.root_entry.has_err);
        let mut perms = std::fs::metadata(dir.path()).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(dir.path(), perms).unwrap();
    }

    #[test]
    fn kill_confirm_cancels_without_signalling() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(scanned(dir.path()));
        // y on a kill confirmation is a no-op by design (1/2 answer).
        app.pending_action = Some(ConfirmAction::KillProcess {
            pid: 424242,
            name: "x".to_string(),
            start_time: 0,
            pidfd: None,
        });
        app.previous_view = ActiveView::Filesystem;
        app.active_view = ActiveView::ConfirmModal;
        app.execute_pending_action();
        assert_eq!(app.active_view, ActiveView::Filesystem);
    }
}

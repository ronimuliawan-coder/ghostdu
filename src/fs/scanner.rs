use crate::fs::entry::FileEntry;
use crate::ghost::{classify_path, classify_safety, is_virtual_fs_path};
use crossbeam_channel::Sender;
use std::collections::HashSet;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ScanProgress {
    pub files_scanned: u64,
    pub bytes_scanned: u64,
    pub current_path: PathBuf,
    pub is_finished: bool,
}

#[allow(dead_code)]
pub struct ScannerOptions {
    pub cross_mounts: bool,
}

impl Default for ScannerOptions {
    fn default() -> Self {
        Self {
            cross_mounts: false,
        }
    }
}

pub fn scan_directory(
    root_path: &Path,
    progress_tx: Option<Sender<ScanProgress>>,
    stop_signal: Arc<AtomicBool>,
) -> Result<FileEntry, String> {
    let canonical = root_path
        .canonicalize()
        .map_err(|e| format!("Cannot resolve path {:?}: {}", root_path, e))?;

    let root_meta = fs::symlink_metadata(&canonical)
        .map_err(|e| format!("Cannot stat root path {:?}: {}", canonical, e))?;

    let root_dev = root_meta.dev();
    let root_ino = root_meta.ino();

    let ghost = classify_path(&canonical);
    let safety = classify_safety(&canonical, ghost);
    let mut root_entry = FileEntry::new_dir(
        canonical
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "/".to_string()),
        canonical.clone(),
        root_dev,
        root_ino,
        ghost,
        safety,
    );

    let mut seen_inodes: HashSet<(u64, u64)> = HashSet::new();
    seen_inodes.insert((root_dev, root_ino));

    let files_counter = Arc::new(AtomicU64::new(0));
    let bytes_counter = Arc::new(AtomicU64::new(0));
    let last_progress = Arc::new(Mutex::new(Instant::now()));

    scan_dir_recursive(
        &canonical,
        &mut root_entry,
        root_dev,
        &mut seen_inodes,
        &progress_tx,
        &stop_signal,
        &files_counter,
        &bytes_counter,
        &last_progress,
    );

    // Sort root children descending by disk usage
    root_entry
        .children
        .sort_by(|a, b| b.disk_usage.cmp(&a.disk_usage));

    if let Some(ref tx) = progress_tx {
        let _ = tx.send(ScanProgress {
            files_scanned: files_counter.load(Ordering::Relaxed),
            bytes_scanned: bytes_counter.load(Ordering::Relaxed),
            current_path: canonical,
            is_finished: true,
        });
    }

    Ok(root_entry)
}

fn scan_dir_recursive(
    dir_path: &Path,
    parent_entry: &mut FileEntry,
    root_dev: u64,
    seen_inodes: &mut HashSet<(u64, u64)>,
    progress_tx: &Option<Sender<ScanProgress>>,
    stop_signal: &Arc<AtomicBool>,
    files_counter: &Arc<AtomicU64>,
    bytes_counter: &Arc<AtomicU64>,
    last_progress: &Arc<Mutex<Instant>>,
) {
    if stop_signal.load(Ordering::Relaxed) {
        return;
    }

    let read_res = fs::read_dir(dir_path);
    let entries = match read_res {
        Ok(e) => e,
        Err(_) => {
            parent_entry.has_err = true;
            return;
        }
    };

    let mut sub_entries = Vec::new();

    for entry_res in entries {
        if stop_signal.load(Ordering::Relaxed) {
            return;
        }

        let entry = match entry_res {
            Ok(e) => e,
            Err(_) => {
                parent_entry.has_err = true;
                continue;
            }
        };

        let path = entry.path();

        // 1. Zero-flag smart filtering: automatically skip virtual/kernel filesystems
        if is_virtual_fs_path(&path) {
            continue;
        }

        let file_name = entry.file_name().to_string_lossy().to_string();

        let meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => {
                let ghost = classify_path(&path);
                let safety = classify_safety(&path, ghost);
                let mut err_entry = FileEntry::new_file(
                    file_name,
                    path,
                    0,
                    0,
                    false,
                    0,
                    0,
                    ghost,
                    safety,
                );
                err_entry.has_err = true;
                sub_entries.push(err_entry);
                continue;
            }
        };

        let dev = meta.dev();
        let ino = meta.ino();
        let is_symlink = meta.is_symlink();
        let is_dir = meta.is_dir() && !is_symlink;
        let apparent_size = meta.len();
        let disk_usage = meta.blocks() * 512;

        let ghost_kind = classify_path(&path);
        let delete_safety = classify_safety(&path, ghost_kind);

        // Progress counter update
        let total_files = files_counter.fetch_add(1, Ordering::Relaxed) + 1;
        bytes_counter.fetch_add(disk_usage, Ordering::Relaxed);

        // Send throttled progress update every ~50ms or every 200 items
        if total_files % 200 == 0 {
            if let Some(ref tx) = progress_tx {
                if let Ok(mut last) = last_progress.lock() {
                    if last.elapsed().as_millis() >= 50 {
                        *last = Instant::now();
                        let _ = tx.send(ScanProgress {
                            files_scanned: total_files,
                            bytes_scanned: bytes_counter.load(Ordering::Relaxed),
                            current_path: path.clone(),
                            is_finished: false,
                        });
                    }
                }
            }
        }

        if is_dir {
            let mut dir_node = FileEntry::new_dir(file_name, path.clone(), dev, ino, ghost_kind, delete_safety);

            // Recurse into subdirectory
            scan_dir_recursive(
                &path,
                &mut dir_node,
                root_dev,
                seen_inodes,
                progress_tx,
                stop_signal,
                files_counter,
                bytes_counter,
                last_progress,
            );

            // Sort child entries descending by disk usage
            dir_node
                .children
                .sort_by(|a, b| b.disk_usage.cmp(&a.disk_usage));

            sub_entries.push(dir_node);
        } else {
            // Regular file or symlink
            let is_duplicate_hardlink = meta.nlink() > 1 && !seen_inodes.insert((dev, ino));

            // If duplicate hardlink, don't double count for parent aggregates
            let (counted_size, counted_disk) = if is_duplicate_hardlink {
                (0, 0)
            } else {
                (apparent_size, disk_usage)
            };

            let mut file_node = FileEntry::new_file(
                file_name,
                path,
                apparent_size,
                disk_usage,
                is_symlink,
                dev,
                ino,
                ghost_kind,
                delete_safety,
            );

            // Store counted size for aggregation
            file_node.size = counted_size;
            file_node.disk_usage = counted_disk;

            sub_entries.push(file_node);
        }
    }

    // Aggregate values for parent_entry
    let mut total_size = 0u64;
    let mut total_disk = 0u64;
    let mut total_items = 0usize;

    for child in &sub_entries {
        total_size = total_size.saturating_add(child.size);
        total_disk = total_disk.saturating_add(child.disk_usage);
        total_items = total_items.saturating_add(child.items_count);
    }

    parent_entry.size = total_size;
    parent_entry.disk_usage = total_disk;
    parent_entry.items_count = total_items + 1; // plus the directory itself
    parent_entry.children = sub_entries;
}

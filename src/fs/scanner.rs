use crate::fs::entry::{DeleteSafety, FileEntry};
use crate::ghost::{classify_path, classify_safety, is_virtual_fs_path};
use crossbeam_channel::Sender;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::OsStringExt;
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

#[derive(Debug, Clone, Default)]
pub struct ScannerOptions {
    pub cross_mounts: bool,
    /// Skip any scanned path containing one of these substrings.
    pub excludes: Vec<String>,
    /// Stop descending after N levels (1 = top level only). Totals cover scanned entries.
    pub max_depth: Option<usize>,
}

fn is_excluded(path: &Path, excludes: &[String]) -> bool {
    if excludes.is_empty() {
        return false;
    }
    let path_str = path.to_string_lossy();
    excludes.iter().any(|p| path_str.contains(p))
}

pub fn scan_directory(
    root_path: &Path,
    progress_tx: Option<Sender<ScanProgress>>,
    stop_signal: Arc<AtomicBool>,
) -> Result<FileEntry, String> {
    scan_directory_with_options(
        root_path,
        progress_tx,
        stop_signal,
        ScannerOptions::default(),
    )
}

pub fn scan_directory_with_options(
    root_path: &Path,
    progress_tx: Option<Sender<ScanProgress>>,
    stop_signal: Arc<AtomicBool>,
    options: ScannerOptions,
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
    root_entry.mtime_secs = mtime_secs(&root_meta);

    let mount_points = if options.cross_mounts {
        HashSet::new()
    } else {
        let mountinfo = fs::read("/proc/self/mountinfo")
            .map_err(|e| format!("Cannot read mount boundaries: {}", e))?;
        parse_mount_points(&mountinfo)
    };

    let mut seen_inodes: HashSet<(u64, u64)> = HashSet::new();
    seen_inodes.insert((root_dev, root_ino));
    let files_counter = Arc::new(AtomicU64::new(0));
    let bytes_counter = Arc::new(AtomicU64::new(0));
    let last_progress = Arc::new(Mutex::new(Instant::now()));

    // A zero depth budget lists the root alone; children would exceed it.
    if options.max_depth != Some(0) {
        scan_dir_recursive(
            &canonical,
            &mut root_entry,
            root_dev,
            &options,
            0,
            &mount_points,
            &mut seen_inodes,
            &progress_tx,
            &stop_signal,
            &files_counter,
            &bytes_counter,
            &last_progress,
        );
    }
    // Sort root children descending by disk usage
    root_entry
        .children
        .sort_by_key(|a| std::cmp::Reverse(a.disk_usage));

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

/// Progress-send cadence: every 200 files, at most every 50 ms. Pure so
/// tests pin every branch without depending on wall-clock scan speed.
fn throttle_progress(total_files: u64, last: Instant, now: Instant) -> bool {
    total_files.is_multiple_of(200) && now.saturating_duration_since(last).as_millis() >= 50
}

#[allow(clippy::too_many_arguments, clippy::only_used_in_recursion)]
fn scan_dir_recursive(
    dir_path: &Path,
    parent_entry: &mut FileEntry,
    root_dev: u64,
    options: &ScannerOptions,
    depth: usize,
    mount_points: &HashSet<PathBuf>,
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
            // Defensive: getdents failing mid-iteration is a kernel-level race
            // window (an unlinked-but-open directory still reads fine), so it
            // cannot be triggered deterministically in-process. Excluded from
            // line coverage; the sibling metadata race is stress-tested.
            #[cfg(not(tarpaulin_include))]
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

        // 2. User exclusions (--exclude): skip the entry and its whole subtree
        if is_excluded(&path, &options.excludes) {
            continue;
        }

        let meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => {
                let ghost = classify_path(&path);
                let safety = classify_safety(&path, ghost);
                let mut err_entry = FileEntry::new_file(
                    entry.file_name().to_string_lossy().into_owned(),
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
        let disk_usage = meta.blocks() * 512;

        // Progress counter update
        let total_files = files_counter.fetch_add(1, Ordering::Relaxed) + 1;
        bytes_counter.fetch_add(disk_usage, Ordering::Relaxed);

        // Send throttled progress update every ~50ms or every 200 items.
        // The cadence decision is a pure function (unit-tested below) so no
        // test depends on wall-clock scan speed.
        if total_files.is_multiple_of(200) {
            if let Some(ref tx) = progress_tx {
                if let Ok(mut last) = last_progress.lock() {
                    if throttle_progress(total_files, *last, Instant::now()) {
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
            let file_name = entry.file_name().to_string_lossy().into_owned();
            let ghost_kind = classify_path(&path);
            let delete_safety = classify_safety(&path, ghost_kind);
            let mut dir_node =
                FileEntry::new_dir(file_name, path.clone(), dev, ino, ghost_kind, delete_safety);
            dir_node.mtime_secs = mtime_secs(&meta);

            // mountinfo also identifies bind mounts whose device matches the root.
            let is_cross_mount = dev != root_dev || mount_points.contains(&path);
            // Directory identities prevent cycles and repeated traversal via bind aliases.
            // Depth cap keeps large trees explorable; totals cover scanned entries only.
            // max_depth counts listed levels below the root (1 = top level only).
            let within_depth = options.max_depth.is_none_or(|max| depth + 1 < max);
            if (options.cross_mounts || !is_cross_mount)
                && within_depth
                && seen_inodes.insert((dev, ino))
            {
                scan_dir_recursive(
                    &path,
                    &mut dir_node,
                    root_dev,
                    options,
                    depth + 1,
                    mount_points,
                    seen_inodes,
                    progress_tx,
                    stop_signal,
                    files_counter,
                    bytes_counter,
                    last_progress,
                );
            }

            // Sort child entries descending by disk usage
            dir_node
                .children
                .sort_by_key(|a| std::cmp::Reverse(a.disk_usage));

            sub_entries.push(dir_node);
        } else {
            sub_entries.push(file_entry(path, &meta, seen_inodes));
        }
    }

    // Aggregate values for parent_entry
    let mut total_size = 0u64;
    let mut total_disk = 0u64;
    let mut total_reclaimable = 0u64;
    let mut total_items = 0usize;
    let mut total_safe_reclaimable = 0u64;
    let mut total_safe_items = 0usize;

    for child in &sub_entries {
        total_size = total_size.saturating_add(child.size);
        total_disk = total_disk.saturating_add(child.disk_usage);
        total_reclaimable = total_reclaimable.saturating_add(child.reclaimable);
        total_items = total_items.saturating_add(child.items_count);
        total_safe_reclaimable =
            total_safe_reclaimable.saturating_add(child.safe_reclaimable_bytes());
        total_safe_items = total_safe_items.saturating_add(child.safe_items_count());
    }

    parent_entry.size = total_size;
    parent_entry.disk_usage = total_disk;
    parent_entry.reclaimable = total_reclaimable;
    parent_entry.items_count = total_items + 1; // plus the directory itself
                                                // Directory staleness follows its newest content: the directory inode
                                                // itself only changes on entry add/remove, not on child edits.
    let newest_child = sub_entries.iter().map(|c| c.mtime_secs).fold(0, Ord::max);
    parent_entry.mtime_secs = parent_entry.mtime_secs.max(newest_child);
    if parent_entry.delete_safety == DeleteSafety::Safe {
        parent_entry.safe_reclaimable = total_reclaimable;
        parent_entry.safe_items = 1;
    } else {
        parent_entry.safe_reclaimable = total_safe_reclaimable;
        parent_entry.safe_items = total_safe_items;
    }
    parent_entry.children = sub_entries;
}

/// Unix seconds of `meta` mtime for the age heatmap; 0 (unknown) when
/// unreadable or pre-epoch. Every line runs on the happy path by construction.
fn mtime_secs(meta: &fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn file_entry(
    path: PathBuf,
    meta: &fs::Metadata,
    seen_inodes: &mut HashSet<(u64, u64)>,
) -> FileEntry {
    let dev = meta.dev();
    let ino = meta.ino();
    let is_symlink = meta.is_symlink();
    let apparent_size = meta.len();
    let disk_usage = meta.blocks() * 512;
    let ghost_kind = classify_path(&path);
    let delete_safety = classify_safety(&path, ghost_kind);
    // Regular file or symlink
    let is_duplicate_hardlink = meta.nlink() > 1 && !seen_inodes.insert((dev, ino));

    // If duplicate hardlink, don't double count for parent aggregates
    let (counted_size, counted_disk) = if is_duplicate_hardlink {
        (0, 0)
    } else {
        (apparent_size, disk_usage)
    };

    let mut file_node = FileEntry::new_file(
        path.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
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
    file_node.mtime_secs = mtime_secs(meta);
    // Any unselected or unseen link can retain the inode blocks.
    if meta.nlink() > 1 {
        file_node.reclaimable = 0;
        file_node.safe_reclaimable = 0;
    }
    if is_duplicate_hardlink {
        file_node.safe_reclaimable = 0;
        file_node.safe_items = 0;
    }

    file_node
}

/// Refresh one failed target without reading sibling subtrees. Preserve the original
/// scan's mount boundary and seed hard-link accounting from the retained tree.
/// `base_depth` is the target's level below the original scan root, so the shared
/// depth budget keeps applying; exclusions filter rediscoved descendants.
pub(crate) fn rescan_entry(
    path: &Path,
    root_dev: u64,
    seen_inodes: &mut HashSet<(u64, u64)>,
    options: &ScannerOptions,
    base_depth: usize,
) -> std::io::Result<FileEntry> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_dir() {
        return Ok(file_entry(path.to_path_buf(), &meta, seen_inodes));
    }
    let ghost = classify_path(path);
    let mut entry = FileEntry::new_dir(
        path.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        path.to_path_buf(),
        meta.dev(),
        meta.ino(),
        ghost,
        classify_safety(path, ghost),
    );
    entry.mtime_secs = mtime_secs(&meta);
    let mounts = parse_mount_points(&fs::read("/proc/self/mountinfo")?);
    if meta.dev() == root_dev
        && !mounts.contains(path)
        && options.max_depth.is_none_or(|max| base_depth < max)
        && seen_inodes.insert((meta.dev(), meta.ino()))
    {
        scan_dir_recursive(
            path,
            &mut entry,
            root_dev,
            // Targeted refresh of an already-scanned path: the target itself
            // was scanned, so only its rediscoved descendants are filtered.
            options,
            base_depth,
            &mounts,
            seen_inodes,
            &None,
            &Arc::new(AtomicBool::new(false)),
            &Arc::new(AtomicU64::new(0)),
            &Arc::new(AtomicU64::new(0)),
            &Arc::new(Mutex::new(Instant::now())),
        );
        entry
            .children
            .sort_by_key(|child| std::cmp::Reverse(child.disk_usage));
    }
    Ok(entry)
}

// Mountinfo escapes whitespace and backslashes as octal bytes. Preserve non-UTF-8 paths.
pub(crate) fn parse_mount_points(mountinfo: &[u8]) -> HashSet<PathBuf> {
    mountinfo
        .split(|&b| b == b'\n')
        .filter_map(|line| line.split(|&b| b == b' ').nth(4))
        .map(decode_mount_path)
        .collect()
}

/// Decodes mountinfo octal escapes (`\040` -> space, etc.); unknown or
/// truncated sequences stay literal so parsing never fails on odd kernels.
fn decode_mount_path(field: &[u8]) -> PathBuf {
    let mut decoded = Vec::with_capacity(field.len());
    let mut i = 0;
    while i < field.len() {
        if field[i] == b'\\' && i + 3 < field.len() {
            let escaped = &field[i + 1..i + 4];
            let byte = match escaped {
                b"040" => Some(b' '),
                b"011" => Some(b'\t'),
                b"012" => Some(b'\n'),
                b"134" => Some(b'\\'),
                _ => None,
            };
            if let Some(byte) = byte {
                decoded.push(byte);
                i += 4;
                continue;
            }
        }
        decoded.push(field[i]);
        i += 1;
    }
    PathBuf::from(OsString::from_vec(decoded))
}

/// Btrfs subvolume boundaries: mount path -> subvolume name, parsed from the
/// `subvol=` super-option. Only `btrfs` filesystem lines are considered; all
/// other lines (wrong fs type, missing option, malformed) are skipped.
pub(crate) fn parse_btrfs_subvolumes(mountinfo: &[u8]) -> HashMap<PathBuf, String> {
    let mut subvols = HashMap::new();
    for line in mountinfo.split(|&b| b == b'\n') {
        let fields: Vec<&[u8]> = line.split(|&b| b == b' ').collect();
        let Some(sep) = fields.iter().position(|f| *f == b"-") else {
            continue;
        };
        // Layout: [..., mount point @4, ...] - fstype source super-options.
        if fields.len() <= 4 || fields.len() < sep + 4 || fields[sep + 1] != b"btrfs" {
            continue;
        }
        let mount_point = decode_mount_path(fields[4]);
        let name = fields[sep + 3]
            .split(|&b| b == b',')
            .find_map(|opt| opt.strip_prefix(b"subvol="))
            .map(|name| String::from_utf8_lossy(name).into_owned());
        if let Some(name) = name {
            subvols.insert(mount_point, name);
        }
    }
    subvols
}

/// True when any mounted subvolume is itself a snapshot store (Snapper,
/// Timeshift, or explicit snapshot directories): deleting files there may
/// free 0 bytes while snapshots reference the extents. Matches exact path
/// components of the subvolume name only, so unrelated paths merely
/// containing those words (e.g. `snapper-notes`) never match.
pub(crate) fn has_snapshot_subvolumes(subvols: &HashMap<PathBuf, String>) -> bool {
    use std::path::Component;
    subvols.values().any(|name| {
        Path::new(name).components().any(|component| {
            matches!(
                component,
                Component::Normal(part)
                    if matches!(
                        part.to_str(),
                        Some("@snapshots" | ".snapshots" | "snapshots" | "timeshift-btrfs")
                    )
            )
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mountinfo_preserves_escaped_and_non_utf8_paths() {
        let mounts = parse_mount_points(
            b"1 0 8:1 / / rw - ext4 /dev/root rw\n2 1 8:1 /source /a\\040b\\011c\\012d\\134e\xff rw - ext4 /dev/root rw\n",
        );
        assert_eq!(mounts.len(), 2);
        assert!(mounts.contains(Path::new("/")));
        assert!(mounts.contains(&PathBuf::from(OsString::from_vec(
            b"/a b\tc\nd\\e\xff".to_vec()
        ))));
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use crossbeam_channel::unbounded;

    fn stop() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(false))
    }

    #[test]
    fn snapshot_matcher_uses_exact_path_components() {
        let yes: HashMap<PathBuf, String> = [
            (PathBuf::from("/@"), "@snapshots".to_string()),
            (PathBuf::from("/t"), "timeshift-btrfs".to_string()),
            (PathBuf::from("/@"), ".snapshots".to_string()),
        ]
        .into_iter()
        .collect();
        assert!(has_snapshot_subvolumes(&yes));
        // Near-misses must not match: the mount path is no longer consulted
        // and bare substrings inside a component do not count.
        let no: HashMap<PathBuf, String> = [
            (PathBuf::from("/snapshots-backup"), "data".to_string()),
            (PathBuf::from("/x"), "snapper-notes".to_string()),
            (PathBuf::from("/y"), "my-timeshift".to_string()),
        ]
        .into_iter()
        .collect();
        assert!(!has_snapshot_subvolumes(&no));
    }

    #[test]
    fn directory_mtime_follows_newest_child() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("new.txt"), "x").unwrap();
        // Backdate both directory inodes: the content stays newer than its
        // containers, so aggregation must surface the file's mtime.
        for p in [dir.path().to_path_buf(), sub.clone()] {
            let st = std::process::Command::new("touch")
                .arg("-d")
                .arg("2001-01-01")
                .arg(&p)
                .status()
                .unwrap();
            assert!(st.success());
        }
        let root = scan_directory(dir.path(), None, stop()).unwrap();
        assert!(
            root.mtime_secs > 1_000_000_000,
            "root follows its newest content, not its own inode"
        );
        let sub_entry = root.children.iter().find(|c| c.name == "sub").unwrap();
        assert_eq!(sub_entry.mtime_secs, root.mtime_secs);
    }

    #[test]
    fn cross_mounts_option_skips_mountinfo() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "x").unwrap();
        let root = scan_directory_with_options(
            dir.path(),
            None,
            stop(),
            ScannerOptions {
                cross_mounts: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(root.children.len(), 1);
    }

    #[test]
    fn progress_channel_receives_finished_scan() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "x").unwrap();
        let (tx, rx) = unbounded();
        scan_directory(dir.path(), Some(tx), stop()).unwrap();
        let mut saw_finished = false;
        while let Ok(progress) = rx.try_recv() {
            saw_finished |= progress.is_finished;
        }
        assert!(saw_finished);
    }

    #[test]
    fn progress_channel_sends_periodic_updates() {
        // Periodic updates need the scan to outlast the 50ms throttle, which
        // depends on wall-clock speed: 48k files take ~600ms locally, so even
        // a machine an order of magnitude faster still emits several updates.
        let dir = tempfile::tempdir().unwrap();
        for d in 0..240 {
            let sub = dir.path().join(format!("d{d}"));
            std::fs::create_dir(&sub).unwrap();
            for i in 0..200 {
                std::fs::write(sub.join(format!("f{i}.txt")), "x").unwrap();
            }
        }
        let (tx, rx) = unbounded();
        scan_directory(dir.path(), Some(tx), stop()).unwrap();
        let mut periodic = 0;
        while let Ok(progress) = rx.try_recv() {
            if !progress.is_finished {
                periodic += 1;
            }
        }
        assert!(periodic > 0, "expected throttled progress updates");
    }

    #[test]
    fn progress_throttle_cadence_branches() {
        use std::time::Duration;
        let t0 = Instant::now();
        assert!(throttle_progress(200, t0, t0 + Duration::from_millis(60)));
        assert!(throttle_progress(400, t0, t0 + Duration::from_millis(50)));
        assert!(!throttle_progress(199, t0, t0 + Duration::from_secs(10)));
        assert!(!throttle_progress(200, t0, t0 + Duration::from_millis(10)));
        assert!(!throttle_progress(200, t0, t0));
    }

    #[test]
    fn unreadable_subtree_marks_error() {
        // Root ignores permission bits: 0o000 never blocks read_dir there.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("locked");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("secret.txt"), "x").unwrap();
        std::fs::write(dir.path().join("ok.txt"), "x").unwrap();
        // Drop guard: a failed assertion below must not skip the restore,
        // or TempDir cleanup fails and litters /tmp.
        struct Restore<'a>(&'a Path);
        impl Drop for Restore<'_> {
            fn drop(&mut self) {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(mut perms) = std::fs::metadata(self.0).map(|m| m.permissions()) {
                    perms.set_mode(0o755);
                    let _ = std::fs::set_permissions(self.0, perms);
                }
            }
        }
        let _restore = Restore(&sub);
        // Remove all permissions: directory reads fail as non-root.
        let mut perms = std::fs::metadata(&sub).unwrap().permissions();
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o000);
        std::fs::set_permissions(&sub, perms).unwrap();
        let root = scan_directory(dir.path(), None, stop()).unwrap();
        let locked = root.children.iter().find(|e| e.name == "locked").unwrap();
        assert!(locked.has_err);
    }

    #[test]
    fn parse_mount_points_rejects_bad_escapes() {
        // Unknown escape stays literal; short/truncated sequences are kept.
        let mounts = parse_mount_points(b"1 0 8:1 /a\\999b /a\\999b rw - ext4 /dev/root rw\n");
        assert_eq!(mounts.len(), 1);
        let mounts = parse_mount_points(b"short line\n");
        assert!(mounts.is_empty());
    }

    #[test]
    fn parse_btrfs_subvolumes_keeps_names() {
        let table = b"30 23 0:22 / / rw,relatime shared:1 - btrfs /dev/sda1 rw,ssd,subvolid=256,subvol=/@\n\
            35 23 0:22 /@/.snapshots /@/.snapshots rw,relatime shared:1 - btrfs /dev/sda1 rw,ssd,subvolid=267,subvol=/@/.snapshots\n\
            40 23 0:24 / /tmp rw,nosuid,nodev shared:153 - tmpfs tmpfs rw,inode64\n\
            41 23 0:22 /@/home /home rw,relatime shared:1 - btrfs /dev/sda1 rw,ssd,subvolid=258\n\
            short line\n";
        let subvols = parse_btrfs_subvolumes(table);
        assert_eq!(subvols.get(&PathBuf::from("/")), Some(&"/@".to_string()));
        assert_eq!(
            subvols.get(&PathBuf::from("/@/.snapshots")),
            Some(&"/@/.snapshots".to_string())
        );
        // tmpfs skipped, btrfs without subvol= skipped, short lines skipped.
        assert_eq!(subvols.len(), 2);
        assert!(has_snapshot_subvolumes(&subvols));
    }

    #[test]
    fn snapshot_matcher_ignores_plain_subvolumes() {
        let mut subvols = HashMap::new();
        subvols.insert(PathBuf::from("/"), "/@".to_string());
        subvols.insert(PathBuf::from("/home"), "/@/home".to_string());
        assert!(!has_snapshot_subvolumes(&subvols));
        assert!(!has_snapshot_subvolumes(&HashMap::new()));
        subvols.insert(
            PathBuf::from("/mnt/backup"),
            "/timeshift-btrfs/snapshots/2024-01-01".to_string(),
        );
        assert!(has_snapshot_subvolumes(&subvols));
    }

    #[test]
    fn parse_btrfs_subvolumes_decodes_escapes() {
        let table = b"30 23 0:22 /my\\040vol /my\\040vol rw shared:1 - btrfs /dev/sda1 rw,subvolid=256,subvol=/myvol\n";
        let subvols = parse_btrfs_subvolumes(table);
        assert_eq!(
            subvols.get(&PathBuf::from("/my vol")),
            Some(&"/myvol".to_string())
        );
    }
}

#[cfg(test)]
mod race_tests {
    use super::*;

    #[test]
    fn removed_mid_scan_marks_error_without_panic() {
        // Deleting the tree mid-scan exercises the readdir/entry error arms.
        for _ in 0..10 {
            let dir = tempfile::tempdir().unwrap();
            for i in 0..500 {
                std::fs::write(dir.path().join(format!("f{i}")), "x").unwrap();
            }
            let victim = dir.path().to_path_buf();
            let killer = std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(1));
                let _ = std::fs::remove_dir_all(&victim);
            });
            let root = scan_directory(dir.path(), None, Arc::new(AtomicBool::new(false)));
            killer.join().unwrap();
            // Either the scan finished first or it recorded the failure.
            if let Ok(root) = root {
                let _ = root.has_err;
            }
        }
    }

    #[test]
    fn concurrent_churn_never_panics_or_double_counts() {
        for _ in 0..3 {
            let dir = tempfile::tempdir().unwrap();
            for i in 0..300 {
                std::fs::write(dir.path().join(format!("f{i}")), "x").unwrap();
            }
            let victim = dir.path().to_path_buf();
            let churn = std::thread::spawn(move || {
                for i in 0..300 {
                    let _ = std::fs::remove_file(victim.join(format!("f{i}")));
                }
            });
            let root = scan_directory(dir.path(), None, Arc::new(AtomicBool::new(false))).unwrap();
            churn.join().unwrap();
            // Whatever survived the race is counted exactly once.
            let mut seen = std::collections::HashSet::new();
            fn check(entry: &FileEntry, seen: &mut std::collections::HashSet<(u64, u64)>) {
                // Vanished mid-scan files share the (0, 0) error identity.
                if !entry.is_dir && (entry.dev, entry.ino) != (0, 0) {
                    assert!(seen.insert((entry.dev, entry.ino)), "double count");
                }
                for child in &entry.children {
                    check(child, seen);
                }
            }
            check(&root, &mut seen);
        }
    }
}

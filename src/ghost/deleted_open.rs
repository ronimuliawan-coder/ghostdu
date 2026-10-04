use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::path::PathBuf;

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct DeletedOpenFile {
    pub pid: u32,
    pub process_name: String,
    pub original_path: String,
    pub size: u64,
    pub fd: String,
    /// Instance identity recorded at scan time. `None` when unreadable; such
    /// rows can never arm a kill confirmation.
    pub start_time: Option<u64>,
}

/// Process start time (field 22 of /proc/<pid>/stat) as a stable instance
/// identity. `None` when the process is gone or unreadable.
pub(crate) fn proc_start_time(pid: u32) -> Option<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // comm may contain spaces or ')'; fields after the last ')' start at field 3.
    stat.rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()
}

/// The validated cleaned path and allocated size for one ghost-file FD.
/// Returns `None` unless the FD passes every table-row filter, so callers
/// reuse this single read instead of rereading the FD to build the row.
/// Only the final ` (deleted)` marker is stripped: filenames that themselves
/// end in that text keep it.
fn deleted_fd_ghost(fd_path: &Path) -> Option<(String, u64)> {
    let target_link = fs::read_link(fd_path).ok()?;
    let target_str = target_link.to_string_lossy();
    let clean_path = target_str.strip_suffix(" (deleted)")?;
    // Skip in-memory or pseudo objects
    if clean_path.starts_with("/memfd:")
        || clean_path.starts_with("/dev/")
        || clean_path.starts_with("pipe:[")
        || clean_path.starts_with("socket:[")
        || clean_path.starts_with("anon_inode:[")
    {
        return None;
    }
    // Query actual size held on disk via stat on the /proc/<pid>/fd/<fd> link
    let size = match fs::metadata(fd_path) {
        Ok(meta) => meta.blocks() * 512,
        Err(_) => 0,
    };
    if size == 0 {
        // If blocks is 0, check file apparent size
        let apparent = fs::metadata(fd_path).map(|m| m.len()).unwrap_or(0);
        if apparent == 0 {
            return None;
        }
    }
    Some((clean_path.to_string(), size))
}

/// Targeted liveness check for one PID: its name and start time iff it
/// currently holds a ghost file. Walks only /proc/<pid>/fd — no global scan,
/// so the kill key path stays off the synchronous full-table walk.
pub(crate) fn describe_pid_ghost(pid: u32) -> Option<(String, u64)> {
    let fd_dir_path = PathBuf::from(format!("/proc/{}/fd", pid));
    let fd_entries = fs::read_dir(&fd_dir_path).ok()?;
    if !fd_entries
        .flatten()
        .any(|fd_entry| deleted_fd_ghost(&fd_entry.path()).is_some())
    {
        return None;
    }
    let name = fs::read_to_string(format!("/proc/{}/comm", pid))
        .ok()
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    Some((name, proc_start_time(pid)?))
}

pub fn scan_deleted_open_files() -> Vec<DeletedOpenFile> {
    let mut deleted_files = Vec::new();
    let proc_dir = match fs::read_dir("/proc") {
        Ok(d) => d,
        Err(_) => return deleted_files,
    };

    for entry in proc_dir.flatten() {
        let file_name = entry.file_name();
        let name_str = file_name.to_string_lossy();
        let pid: u32 = match name_str.parse() {
            Ok(p) => p,
            Err(_) => continue, // Not a numeric PID directory
        };

        let fd_dir_path = PathBuf::from(format!("/proc/{}/fd", pid));
        let fd_entries = match fs::read_dir(&fd_dir_path) {
            Ok(entries) => entries,
            Err(_) => continue, // Permission denied or process exited
        };

        // Validate one PID instance across the complete FD qualification:
        // capture identity first, publish rows only if it still matches after.
        // Otherwise an exit plus PID reuse could pair old file evidence with
        // the replacement's identity.
        let start_time = proc_start_time(pid);
        let mut comm_name: Option<String> = None;
        let mut pid_deleted_files = Vec::new();

        for fd_entry in fd_entries.flatten() {
            let fd_path = fd_entry.path();
            // Single validated read per FD: path and size come from the same
            // check that applied the row filters.
            let Some((clean_path, size)) = deleted_fd_ghost(&fd_path) else {
                continue;
            };

            if comm_name.is_none() {
                let comm_path = format!("/proc/{}/comm", pid);
                comm_name = fs::read_to_string(comm_path)
                    .ok()
                    .map(|s| s.trim().to_string());
            }

            pid_deleted_files.push(DeletedOpenFile {
                pid,
                process_name: comm_name.clone().unwrap_or_else(|| "unknown".to_string()),
                original_path: clean_path,
                size,
                fd: fd_entry.file_name().to_string_lossy().to_string(),
                start_time,
            });
        }

        if proc_start_time(pid) == start_time {
            deleted_files.extend(pid_deleted_files);
        }
    }

    // Sort descending by size
    deleted_files.sort_by_key(|a| std::cmp::Reverse(a.size));
    deleted_files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(unix)]
    fn targeted_check_tracks_own_deleted_file() {
        // NOTE: no "absent" assertion on our own PID — sibling tests in this
        // process may legitimately hold deleted files concurrently.
        let held = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(held.path(), vec![0u8; 100]).unwrap();
        std::fs::remove_file(held.path()).unwrap();
        let own = std::process::id();
        let (name, start) = describe_pid_ghost(own).expect("own deleted file");
        assert!(!name.is_empty());
        assert_eq!(Some(start), proc_start_time(own));
        drop(held);
        assert!(describe_pid_ghost(2_147_000_000).is_none());
    }

    #[test]
    #[cfg(unix)]
    fn scan_publishes_rows_for_stable_instance() {
        // The scanner's own deleted file must appear with this instance's
        // start time: evidence and identity captured as one.
        let held = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(held.path(), vec![0u8; 100]).unwrap();
        std::fs::remove_file(held.path()).unwrap();
        let own = std::process::id();
        let expected = proc_start_time(own);
        let rows = scan_deleted_open_files();
        let row = rows
            .iter()
            .find(|r| r.pid == own)
            .expect("own deleted file row");
        assert_eq!(row.start_time, expected);
        drop(held);
    }

    #[test]
    #[cfg(unix)]
    fn deleted_suffix_strips_once_from_row_paths() {
        // A deleted file whose own name ends in " (deleted)" keeps it: only
        // the kernel's final marker is stripped.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("note (deleted)");
        std::fs::write(&path, vec![0u8; 100]).unwrap();
        let held = std::fs::File::open(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        let own = std::process::id();
        let rows = scan_deleted_open_files();
        let row = rows
            .iter()
            .find(|r| r.pid == own && r.original_path.ends_with("note (deleted)"))
            .expect("row keeps the filename suffix");
        assert!(row.original_path.ends_with("note (deleted)"));
        assert!(!row.original_path.ends_with("note"));
        drop(held);
    }
}

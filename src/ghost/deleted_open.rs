use std::collections::HashMap;
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

/// Whether one /proc/<pid>/fd entry counts as a ghost file. Mirrors the table
/// row filters exactly (deleted suffix, pseudo-object exclusions, nonzero size).
fn deleted_fd_holds_ghost(fd_path: &Path) -> bool {
    let target_link = match fs::read_link(fd_path) {
        Ok(link) => link,
        Err(_) => return false,
    };
    let target_str = target_link.to_string_lossy();
    let Some(clean_path) = target_str.strip_suffix(" (deleted)") else {
        return false;
    };
    // Skip in-memory or pseudo objects
    if clean_path.starts_with("/memfd:")
        || clean_path.starts_with("/dev/")
        || clean_path.starts_with("pipe:[")
        || clean_path.starts_with("socket:[")
        || clean_path.starts_with("anon_inode:[")
    {
        return false;
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
            return false;
        }
    }
    true
}

/// Targeted liveness check for one PID: its name and start time iff it
/// currently holds a ghost file. Walks only /proc/<pid>/fd — no global scan,
/// so the kill key path stays off the synchronous full-table walk.
pub(crate) fn describe_pid_ghost(pid: u32) -> Option<(String, u64)> {
    let fd_dir_path = PathBuf::from(format!("/proc/{}/fd", pid));
    let fd_entries = fs::read_dir(&fd_dir_path).ok()?;
    if !fd_entries
        .flatten()
        .any(|fd_entry| deleted_fd_holds_ghost(&fd_entry.path()))
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
    // One stat read per PID, not per FD row.
    let mut start_times: HashMap<u32, Option<u64>> = HashMap::new();
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

        let mut comm_name: Option<String> = None;

        for fd_entry in fd_entries.flatten() {
            let fd_path = fd_entry.path();
            if !deleted_fd_holds_ghost(&fd_path) {
                continue;
            }
            let target_str = fs::read_link(&fd_path)
                .map(|link| link.to_string_lossy().into_owned())
                .unwrap_or_default();
            let clean_path = target_str.trim_end_matches(" (deleted)").to_string();

            // Query actual size held on disk via stat on the /proc/<pid>/fd/<fd> link
            let size = match fs::metadata(&fd_path) {
                Ok(meta) => meta.blocks() * 512,
                Err(_) => 0,
            };

            if size == 0 {
                // If blocks is 0, check file apparent size
                let apparent = fs::metadata(&fd_path).map(|m| m.len()).unwrap_or(0);
                if apparent == 0 {
                    continue;
                }
            }

            if comm_name.is_none() {
                let comm_path = format!("/proc/{}/comm", pid);
                comm_name = fs::read_to_string(comm_path)
                    .ok()
                    .map(|s| s.trim().to_string());
            }

            deleted_files.push(DeletedOpenFile {
                pid,
                process_name: comm_name.clone().unwrap_or_else(|| "unknown".to_string()),
                original_path: clean_path,
                size,
                fd: fd_entry.file_name().to_string_lossy().to_string(),
                start_time: *start_times
                    .entry(pid)
                    .or_insert_with(|| proc_start_time(pid)),
            });
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
}

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct DeletedOpenFile {
    pub pid: u32,
    pub process_name: String,
    pub original_path: String,
    pub size: u64,
    pub fd: String,
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

        let mut comm_name: Option<String> = None;

        for fd_entry in fd_entries.flatten() {
            let fd_path = fd_entry.path();
            let target_link = match fs::read_link(&fd_path) {
                Ok(link) => link,
                Err(_) => continue,
            };

            let target_str = target_link.to_string_lossy();
            if target_str.ends_with(" (deleted)") {
                let clean_path = target_str.trim_end_matches(" (deleted)").to_string();

                // Skip in-memory or pseudo objects
                if clean_path.starts_with("/memfd:")
                    || clean_path.starts_with("/dev/")
                    || clean_path.starts_with("pipe:[")
                    || clean_path.starts_with("socket:[")
                    || clean_path.starts_with("anon_inode:[")
                {
                    continue;
                }

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
                });
            }
        }
    }

    // Sort descending by size
    deleted_files.sort_by(|a, b| b.size.cmp(&a.size));
    deleted_files
}

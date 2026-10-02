use crate::fs::entry::DeleteSafety;
use crate::ghost::{classify_path, classify_safety};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Default)]
pub struct DeleteResult {
    pub succeeded: Vec<PathBuf>,
    pub failed: Vec<(PathBuf, String)>,
}

pub fn permanently_delete<P: AsRef<Path>>(paths: &[P]) -> DeleteResult {
    let mut result = DeleteResult::default();

    for path_ref in paths {
        let path = path_ref.as_ref();

        // Safety gate: reject critical system directories
        let ghost = classify_path(path);
        if classify_safety(path, ghost) == DeleteSafety::System {
            result.failed.push((
                path.to_path_buf(),
                "Blocked: Protected system file/directory cannot be deleted".to_string(),
            ));
            continue;
        }

        // Query symlink metadata without following the link
        let meta = match fs::symlink_metadata(path) {
            Ok(m) => m,
            Err(e) => {
                result.failed.push((path.to_path_buf(), e.to_string()));
                continue;
            }
        };

        let is_symlink = meta.file_type().is_symlink();

        // For non-symlink paths, check canonicalized target to guard against paths
        // (like relative paths or paths with ..) that resolve into system roots.
        // For symlinks, we only unlink the link itself (never touching the target),
        // so a symlink pointing to a system path (e.g. ~/etc-link -> /etc) can be safely unlinked.
        if !is_symlink {
            let canonical = match path.canonicalize() {
                Ok(canonical) => canonical,
                Err(error) => {
                    result.failed.push((
                        path.to_path_buf(),
                        format!("Cannot verify target: {}", error),
                    ));
                    continue;
                }
            };
            let canon_ghost = classify_path(&canonical);
            if classify_safety(&canonical, canon_ghost) == DeleteSafety::System {
                result.failed.push((
                    path.to_path_buf(),
                    "Blocked: Protected system target cannot be deleted".to_string(),
                ));
                continue;
            }
        }

        // If the path is a symlink (even to a directory), we must ONLY unlink it with remove_file
        // Calling remove_dir_all would follow the symlink and destroy the target's contents!
        let remove_res = if meta.file_type().is_symlink() {
            fs::remove_file(path)
        } else if meta.is_dir() {
            fs::remove_dir_all(path)
        } else {
            fs::remove_file(path)
        };

        match remove_res {
            Ok(_) => {
                result.succeeded.push(path.to_path_buf());
            }
            Err(e) => {
                result.failed.push((path.to_path_buf(), e.to_string()));
            }
        }
    }

    result
}

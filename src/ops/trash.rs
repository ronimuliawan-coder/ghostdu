use crate::fs::entry::DeleteSafety;
use crate::ghost::{classify_path, classify_safety};
use std::path::{Path, PathBuf};

#[derive(Debug, Default)]
pub struct TrashResult {
    pub succeeded: Vec<PathBuf>,
    pub failed: Vec<(PathBuf, String)>,
}

pub fn move_to_trash<P: AsRef<Path>>(paths: &[P]) -> TrashResult {
    let mut result = TrashResult::default();

    for path_ref in paths {
        let path = path_ref.as_ref();

        // Safety gate: reject critical system directories
        let ghost = classify_path(path);
        if classify_safety(path, ghost) == DeleteSafety::System {
            result.failed.push((
                path.to_path_buf(),
                "Blocked: Protected system file/directory cannot be moved to trash".to_string(),
            ));
            continue;
        }

        // Also check canonicalized target
        if let Ok(canonical) = path.canonicalize() {
            let canon_ghost = classify_path(&canonical);
            if classify_safety(&canonical, canon_ghost) == DeleteSafety::System {
                result.failed.push((
                    path.to_path_buf(),
                    "Blocked: Protected system target cannot be moved to trash".to_string(),
                ));
                continue;
            }
        }

        match trash::delete(path) {
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

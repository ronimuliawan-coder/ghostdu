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

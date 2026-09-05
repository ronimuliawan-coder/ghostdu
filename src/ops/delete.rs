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
        let remove_res = if path.is_dir() {
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

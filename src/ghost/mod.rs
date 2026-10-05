pub mod deleted_open;
pub mod detector;
pub mod docker;

pub(crate) use deleted_open::{describe_pid_ghost, proc_start_time};
pub use deleted_open::{scan_deleted_open_files, DeletedOpenFile};
pub use detector::{classify_path, classify_safety, is_virtual_fs_path};
/// Live-daemon prune entry point; excluded from coverage builds (needs a
/// real daemon), so the re-export is gated with the definition.
#[cfg(not(tarpaulin_include))]
pub use docker::prune_docker_dangling;
pub use docker::{fetch_docker_disk_info, parse_docker_df_json, DockerDiskInfo};

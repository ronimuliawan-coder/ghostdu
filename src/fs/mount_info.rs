use std::ffi::CString;
use std::fs;
use std::mem::MaybeUninit;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default)]
pub struct FsMountInfo {
    pub device: String,
    pub mount_point: PathBuf,
    pub fs_type: String,
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub free_bytes: u64,
    pub avail_bytes: u64,
    pub use_percent: f64,
}

pub fn query_fs_info(path: &Path) -> Option<FsMountInfo> {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

    // 1. Call statvfs to query block statistics
    let c_path = CString::new(canonical.as_os_str().as_bytes()).ok()?;
    let mut stat: MaybeUninit<libc::statvfs> = MaybeUninit::uninit();

    let res = unsafe { libc::statvfs(c_path.as_ptr(), stat.as_mut_ptr()) };
    if res != 0 {
        return None;
    }

    let stat = unsafe { stat.assume_init() };

    let frsize = stat.f_frsize as u64;
    let total_bytes = stat.f_blocks as u64 * frsize;
    let free_bytes = stat.f_bfree as u64 * frsize;
    let avail_bytes = stat.f_bavail as u64 * frsize;
    let used_bytes = total_bytes.saturating_sub(free_bytes);
    let use_percent = if total_bytes > 0 {
        ((used_bytes as f64 / total_bytes as f64) * 100.0).clamp(0.0, 100.0)
    } else {
        0.0
    };

    // 2. Parse /proc/mounts to match mount point and filesystem type
    let (device, mount_point, fs_type) = resolve_mount_point(&canonical);

    Some(FsMountInfo {
        device,
        mount_point,
        fs_type,
        total_bytes,
        used_bytes,
        free_bytes,
        avail_bytes,
        use_percent,
    })
}

fn resolve_mount_point(target_path: &Path) -> (String, PathBuf, String) {
    let mut best_device = "unknown".to_string();
    let mut best_mount = PathBuf::from("/");
    let mut best_fstype = "unknown".to_string();
    let mut best_len = 0usize;

    if let Ok(content) = fs::read_to_string("/proc/mounts") {
        for line in content.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 3 {
                let dev = parts[0];
                let mount = Path::new(parts[1]);
                let fstype = parts[2];

                if target_path.starts_with(mount) {
                    let mount_len = mount.as_os_str().len();
                    if mount_len >= best_len {
                        best_len = mount_len;
                        best_device = dev.to_string();
                        best_mount = mount.to_path_buf();
                        best_fstype = fstype.to_string();
                    }
                }
            }
        }
    }

    (best_device, best_mount, best_fstype)
}

#[derive(Debug, Clone)]
pub struct DetailedItemInfo {
    pub name: String,
    pub full_path: PathBuf,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub disk_usage: u64,
    pub apparent_size: u64,
    pub items_count: usize,
    pub blocks_512: u64,
    pub dev: u64,
    pub ino: u64,
    pub mode_octal: u32,
    pub mode_str: String,
    pub uid: u32,
    pub gid: u32,
    pub modified_str: String,
    pub fs_info: Option<FsMountInfo>,
}

pub fn get_detailed_item_info(path: &Path, items_count: usize) -> Option<DetailedItemInfo> {
    let meta = fs::symlink_metadata(path).ok()?;
    let disk_usage = meta.blocks() * 512;
    let apparent_size = meta.len();
    let is_symlink = meta.is_symlink();
    let is_dir = meta.is_dir() && !is_symlink;
    let dev = meta.dev();
    let ino = meta.ino();
    let mode = meta.mode();
    let uid = meta.uid();
    let gid = meta.gid();

    let mode_str = format_mode(mode, is_dir, is_symlink);
    let mode_octal = mode & 0o7777;

    let modified_str = match meta.modified() {
        Ok(time) => {
            let epoch = time
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as libc::time_t)
                .unwrap_or(0);
            let mut tm: libc::tm = unsafe { std::mem::zeroed() };
            let tm_ptr = unsafe { libc::localtime_r(&epoch, &mut tm) };
            if !tm_ptr.is_null() {
                let mut buf = [0u8; 64];
                if let Ok(c_fmt) = CString::new("%Y-%m-%d %H:%M:%S") {
                    let len = unsafe {
                        libc::strftime(
                            buf.as_mut_ptr() as *mut libc::c_char,
                            buf.len(),
                            c_fmt.as_ptr(),
                            &tm,
                        )
                    };
                    if len > 0 {
                        String::from_utf8_lossy(&buf[..len]).to_string()
                    } else {
                        "Unknown".to_string()
                    }
                } else {
                    "Unknown".to_string()
                }
            } else {
                "Unknown".to_string()
            }
        }
        Err(_) => "Unknown".to_string(),
    };

    let fs_info = query_fs_info(path);

    Some(DetailedItemInfo {
        name: path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "/".to_string()),
        full_path: path.to_path_buf(),
        is_dir,
        is_symlink,
        disk_usage,
        apparent_size,
        items_count,
        blocks_512: meta.blocks(),
        dev,
        ino,
        mode_octal,
        mode_str,
        uid,
        gid,
        modified_str,
        fs_info,
    })
}

fn format_mode(mode: u32, is_dir: bool, is_symlink: bool) -> String {
    let type_char = if is_symlink {
        'l'
    } else if is_dir {
        'd'
    } else {
        '-'
    };

    let rwx = [
        (0o400, 'r'), (0o200, 'w'), (0o100, 'x'),
        (0o040, 'r'), (0o020, 'w'), (0o010, 'x'),
        (0o004, 'r'), (0o002, 'w'), (0o001, 'x'),
    ];

    let mut s = String::with_capacity(10);
    s.push(type_char);
    for (bit, ch) in rwx {
        if mode & bit != 0 {
            s.push(ch);
        } else {
            s.push('-');
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_format_mode() {
        assert_eq!(format_mode(0o755, true, false), "drwxr-xr-x");
        assert_eq!(format_mode(0o644, false, false), "-rw-r--r--");
        assert_eq!(format_mode(0o777, false, true), "lrwxrwxrwx");
    }

    #[test]
    fn test_query_fs_info_root() {
        let root = Path::new("/");
        let info = query_fs_info(root);
        assert!(info.is_some(), "query_fs_info(/) should succeed");
        let info = info.unwrap();
        assert!(info.total_bytes > 0, "Root filesystem total_bytes should be > 0");
        assert!(info.avail_bytes > 0, "Root filesystem avail_bytes should be > 0");
        assert!(info.use_percent >= 0.0 && info.use_percent <= 100.0);
    }

    #[test]
    fn test_get_detailed_item_info() {
        let root = Path::new("/");
        let info = get_detailed_item_info(root, 42);
        assert!(info.is_some(), "get_detailed_item_info(/) should succeed");
        let info = info.unwrap();
        assert!(info.is_dir);
        assert_eq!(info.items_count, 42);
        assert!(info.fs_info.is_some());
    }
}

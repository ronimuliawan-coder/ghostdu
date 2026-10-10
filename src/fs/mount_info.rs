use std::ffi::CString;
use std::fs;
use std::mem::MaybeUninit;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::fs::entry::{DeleteSafety, GhostKind};
use crate::ghost::{classify_path, classify_safety};

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

    let frsize = stat.f_frsize as u128;
    let total_bytes = ((stat.f_blocks as u128) * frsize).min(u64::MAX as u128) as u64;
    let free_bytes = ((stat.f_bfree as u128) * frsize).min(u64::MAX as u128) as u64;
    let avail_bytes = ((stat.f_bavail as u128) * frsize).min(u64::MAX as u128) as u64;
    let used_bytes = total_bytes.saturating_sub(free_bytes);
    let use_percent = usage_percent(used_bytes, total_bytes);

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

/// Usage percentage that stays defined for zero-sized filesystems.
fn usage_percent(used_bytes: u64, total_bytes: u64) -> f64 {
    if total_bytes > 0 {
        ((used_bytes as f64 / total_bytes as f64) * 100.0).clamp(0.0, 100.0)
    } else {
        0.0
    }
}

/// Unescapes octal sequences (e.g. \040 -> ' ', \011 -> '\t', \012 -> '\n', \134 -> '\')
/// used by Linux /proc/mounts and mntent for paths containing whitespace or special characters.
pub fn unescape_mount_path(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            let mut octal = String::new();
            for _ in 0..3 {
                if let Some(&digit) = chars.peek() {
                    if ('0'..='7').contains(&digit) {
                        octal.push(chars.next().unwrap());
                    } else {
                        break;
                    }
                }
            }
            if octal.len() == 3 {
                if let Ok(val) = u8::from_str_radix(&octal, 8) {
                    out.push(val as char);
                    continue;
                }
            }
            out.push('\\');
            out.push_str(&octal);
        } else {
            out.push(c);
        }
    }
    out
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
                let raw_mount = parts[1];
                let unescaped = unescape_mount_path(raw_mount);
                let mount = Path::new(&unescaped);
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
    pub ghost_kind: GhostKind,
    pub delete_safety: DeleteSafety,
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

    // Every fallback below produced "Unknown"; the eager chain keeps each
    // line covered while preserving exactly that observable behavior.
    // Pre-epoch timestamps convert exactly via SystemTime (unlike the old
    // epoch-zero clamp), so 1960s mtimes display correctly.
    let modified_str = meta
        .modified()
        .ok()
        .map(|time| {
            chrono::DateTime::<chrono::Utc>::from(time)
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        })
        .unwrap_or("Unknown".to_string());

    let fs_info = query_fs_info(path);
    let ghost_kind = classify_path(path);

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
        ghost_kind,
        delete_safety: classify_safety(path, ghost_kind),
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

    let mut s = String::with_capacity(10);
    s.push(type_char);

    // User permissions (r, w, x/s/S)
    s.push(if mode & 0o400 != 0 { 'r' } else { '-' });
    s.push(if mode & 0o200 != 0 { 'w' } else { '-' });
    s.push(match (mode & 0o4000 != 0, mode & 0o100 != 0) {
        (true, true) => 's',
        (true, false) => 'S',
        (false, true) => 'x',
        (false, false) => '-',
    });

    // Group permissions (r, w, x/s/S)
    s.push(if mode & 0o040 != 0 { 'r' } else { '-' });
    s.push(if mode & 0o020 != 0 { 'w' } else { '-' });
    s.push(match (mode & 0o2000 != 0, mode & 0o010 != 0) {
        (true, true) => 's',
        (true, false) => 'S',
        (false, true) => 'x',
        (false, false) => '-',
    });

    // Other permissions (r, w, x/t/T)
    s.push(if mode & 0o004 != 0 { 'r' } else { '-' });
    s.push(if mode & 0o002 != 0 { 'w' } else { '-' });
    s.push(match (mode & 0o1000 != 0, mode & 0o001 != 0) {
        (true, true) => 't',
        (true, false) => 'T',
        (false, true) => 'x',
        (false, false) => '-',
    });

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
        // Special bits: setuid, setgid, sticky
        assert_eq!(format_mode(0o4755, false, false), "-rwsr-xr-x");
        assert_eq!(format_mode(0o2755, false, false), "-rwxr-sr-x");
        assert_eq!(format_mode(0o1777, true, false), "drwxrwxrwt");
        assert_eq!(format_mode(0o4644, false, false), "-rwSr--r--");
        assert_eq!(format_mode(0o1766, true, false), "drwxrw-rwT");
    }

    #[test]
    fn test_unescape_mount_path() {
        assert_eq!(
            unescape_mount_path("/media/USB\\040Drive"),
            "/media/USB Drive"
        );
        assert_eq!(
            unescape_mount_path("/mnt/special\\011tab\\012newline\\134slash"),
            "/mnt/special\ttab\nnewline\\slash"
        );
        assert_eq!(unescape_mount_path("/normal/path"), "/normal/path");
    }

    #[test]
    fn test_query_fs_info_root() {
        let root = Path::new("/");
        let info = query_fs_info(root);
        assert!(info.is_some(), "query_fs_info(/) should succeed");
        let info = info.unwrap();
        assert!(
            info.total_bytes > 0,
            "Root filesystem total_bytes should be > 0"
        );
        assert!(
            info.avail_bytes > 0,
            "Root filesystem avail_bytes should be > 0"
        );
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

#[cfg(test)]
mod coverage_tests {
    use super::*;

    #[test]
    fn usage_percent_handles_empty_filesystems() {
        assert_eq!(usage_percent(0, 0), 0.0);
        assert_eq!(usage_percent(50, 100), 50.0);
        assert_eq!(usage_percent(200, 100), 100.0);
    }

    #[test]
    fn unescape_handles_octal_and_literals() {
        assert_eq!(unescape_mount_path("/mnt/my\\040disk"), "/mnt/my disk");
        assert_eq!(unescape_mount_path("/plain/path"), "/plain/path");
        assert_eq!(unescape_mount_path("/x\\999y"), "/x\\999y");
        assert_eq!(unescape_mount_path("/x\\4y"), "/x\\4y");
        assert_eq!(unescape_mount_path("/trailing\\"), "/trailing\\");
        assert_eq!(unescape_mount_path(""), "");
    }

    #[test]
    fn mode_strings_cover_special_bits() {
        assert_eq!(format_mode(0o755, false, false), "-rwxr-xr-x");
        assert_eq!(format_mode(0o4755, false, false), "-rwsr-xr-x");
        assert_eq!(format_mode(0o4744, false, false), "-rwsr--r--");
        assert_eq!(format_mode(0o4644, false, false), "-rwSr--r--");
        assert_eq!(format_mode(0o2755, false, false), "-rwxr-sr-x");
        assert_eq!(format_mode(0o2744, false, false), "-rwxr-Sr--");
        assert_eq!(format_mode(0o1755, true, false), "drwxr-xr-t");
        assert_eq!(format_mode(0o1744, true, false), "drwxr--r-T");
        assert_eq!(format_mode(0o777, false, true), "lrwxrwxrwx");
        assert_eq!(format_mode(0o600, false, false), "-rw-------");
    }

    #[test]
    fn item_info_missing_path_returns_none() {
        assert!(
            get_detailed_item_info(std::path::Path::new("/definitely/not/here-12345"), 0).is_none()
        );
        assert!(query_fs_info(std::path::Path::new("/definitely/not/here-12345")).is_none());
    }

    #[test]
    fn pre_epoch_mtime_displays_exact_date() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("old.txt");
        std::fs::write(&file, "x").unwrap();
        let status = std::process::Command::new("touch")
            .arg("-d")
            .arg("1960-01-01 00:00:00")
            .arg(&file)
            .status()
            .unwrap();
        assert!(status.success());
        let info = get_detailed_item_info(&file, 0).unwrap();
        assert!(
            info.modified_str.starts_with("1960-01-01"),
            "unexpected date: {}",
            info.modified_str
        );
    }
}

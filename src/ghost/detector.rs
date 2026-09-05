use crate::fs::entry::GhostKind;
use std::path::Path;

pub fn classify_path(path: &Path) -> GhostKind {
    let path_str = path.to_string_lossy();

    // Docker system locations
    if path_str.contains("/var/lib/docker/overlay2") {
        return GhostKind::DockerOverlay;
    }
    if path_str.contains("/var/lib/docker/volumes") {
        return GhostKind::DockerVolume;
    }
    if path_str.contains("/var/lib/docker/containers") {
        return GhostKind::DockerContainer;
    }
    if path_str.contains("/var/lib/docker/buildkit") {
        return GhostKind::DockerBuildkit;
    }
    if path_str.contains("/var/lib/docker") || path_str.contains("/var/run/docker") {
        return GhostKind::DockerUser;
    }

    // User-space Docker & Podman
    if path_str.contains(".local/share/docker") || path_str.contains(".docker") {
        return GhostKind::DockerUser;
    }
    if path_str.contains(".local/share/containers") || path_str.contains(".config/containers") {
        return GhostKind::PodmanUser;
    }

    // Package manager caches (Arch pacman, etc.)
    if path_str.contains("/var/cache/pacman/pkg") {
        return GhostKind::PackageCache;
    }

    // Common heavy build and transient caches
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    match file_name {
        "node_modules" | "target" | "__pycache__" | ".venv" | "venv"
        | ".pytest_cache" | ".next" | ".nuxt" | ".svelte-kit" | ".turbo"
        | ".gradle" | ".cargo/registry" | ".cargo/git" => {
            return GhostKind::BuildCache;
        }
        _ => {}
    }

    if path_str.contains("/.cache/") || file_name == ".cache" {
        return GhostKind::BuildCache;
    }

    GhostKind::None
}

/// Returns true if a given path is an inherently virtual/pseudo Linux filesystem
/// that should never be traversed when analyzing disk usage.
pub fn is_virtual_fs_path(path: &Path) -> bool {
    let path_str = path.to_string_lossy();
    let p = path_str.as_ref();

    // Skip root virtual mounts
    if p == "/proc" || p.starts_with("/proc/")
        || p == "/sys" || p.starts_with("/sys/")
        || p == "/dev" || p.starts_with("/dev/")
        || p == "/run" || p.starts_with("/run/")
        || p == "/sys/firmware" || p.starts_with("/sys/firmware/")
        || p == "/sys/kernel" || p.starts_with("/sys/kernel/")
        || p == "/sys/fs/cgroup" || p.starts_with("/sys/fs/cgroup/")
        || p == "/dev/shm" || p.starts_with("/dev/shm/")
        || p == "/dev/pts" || p.starts_with("/dev/pts/")
    {
        return true;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_virtual_fs_skipping() {
        assert!(is_virtual_fs_path(&PathBuf::from("/proc")));
        assert!(is_virtual_fs_path(&PathBuf::from("/proc/1/stat")));
        assert!(is_virtual_fs_path(&PathBuf::from("/sys")));
        assert!(is_virtual_fs_path(&PathBuf::from("/sys/kernel/debug")));
        assert!(is_virtual_fs_path(&PathBuf::from("/dev")));
        assert!(is_virtual_fs_path(&PathBuf::from("/dev/shm")));
        assert!(is_virtual_fs_path(&PathBuf::from("/run")));
        assert!(is_virtual_fs_path(&PathBuf::from("/run/user/1000")));

        // Normal filesystems must NOT be skipped
        assert!(!is_virtual_fs_path(&PathBuf::from("/home")));
        assert!(!is_virtual_fs_path(&PathBuf::from("/home/ron")));
        assert!(!is_virtual_fs_path(&PathBuf::from("/var")));
        assert!(!is_virtual_fs_path(&PathBuf::from("/usr")));
        assert!(!is_virtual_fs_path(&PathBuf::from("/etc")));
    }

    #[test]
    fn test_ghost_classification() {
        assert_eq!(
            classify_path(&PathBuf::from("/var/lib/docker/overlay2/abc")),
            GhostKind::DockerOverlay
        );
        assert_eq!(
            classify_path(&PathBuf::from("/var/lib/docker/volumes/my-vol")),
            GhostKind::DockerVolume
        );
        assert_eq!(
            classify_path(&PathBuf::from("/var/lib/docker/containers/c123")),
            GhostKind::DockerContainer
        );
        assert_eq!(
            classify_path(&PathBuf::from("/var/lib/docker/buildkit/cache")),
            GhostKind::DockerBuildkit
        );
        assert_eq!(
            classify_path(&PathBuf::from("/home/ron/.local/share/docker")),
            GhostKind::DockerUser
        );
        assert_eq!(
            classify_path(&PathBuf::from("/home/ron/.local/share/containers")),
            GhostKind::PodmanUser
        );
        assert_eq!(
            classify_path(&PathBuf::from("/home/ron/project/node_modules")),
            GhostKind::BuildCache
        );
        assert_eq!(
            classify_path(&PathBuf::from("/home/ron/project/target")),
            GhostKind::BuildCache
        );
        assert_eq!(
            classify_path(&PathBuf::from("/var/cache/pacman/pkg")),
            GhostKind::PackageCache
        );
        assert_eq!(
            classify_path(&PathBuf::from("/home/ron/documents/photo.jpg")),
            GhostKind::None
        );
    }
}

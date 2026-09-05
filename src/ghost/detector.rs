use crate::fs::entry::GhostKind;
use std::path::Path;

pub fn classify_path(path: &Path) -> GhostKind {
    let path_str = path.to_string_lossy();
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

    // 1. Wastebin / Trash locations (FreeDesktop & mount-point wastebins)
    if path_str.contains("/.local/share/Trash")
        || path_str.contains("/.Trash-")
        || path_str.contains("/.Trash/")
        || file_name == ".Trash"
        || file_name.starts_with(".Trash-")
        || file_name == ".Trashes"
    {
        return GhostKind::Trash;
    }

    // 2. Crash Dumps & Coredumps
    if path_str.contains("/var/lib/systemd/coredump")
        || path_str.contains("/var/crash")
        || file_name.ends_with(".coredump")
        || (file_name.starts_with("core.") && path_str.contains("/var/"))
    {
        return GhostKind::CoreDump;
    }

    // 3. System Snapshots (Timeshift, Snapper)
    if path_str.contains("/run/timeshift/backup")
        || path_str.contains("/timeshift/snapshots")
        || path_str.starts_with("/timeshift")
        || path_str.contains("/.snapshots/")
        || file_name == ".snapshots"
    {
        return GhostKind::SystemSnapshot;
    }

    // 4. Docker system locations
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

    // 5. User-space Docker & Podman
    if path_str.contains(".local/share/docker") || path_str.contains(".docker") {
        return GhostKind::DockerUser;
    }
    if path_str.contains(".local/share/containers") || path_str.contains(".config/containers") {
        return GhostKind::PodmanUser;
    }

    // 6. Flatpak & Snap runtimes and app data
    if path_str.contains("/var/lib/flatpak")
        || path_str.contains("/.var/app/")
        || file_name == ".var"
        || path_str.contains("/.local/share/flatpak")
    {
        return GhostKind::Flatpak;
    }
    if path_str.contains("/var/lib/snapd")
        || path_str.contains("/snap/")
        || path_str.ends_with("/snap")
        || (path_str.contains("/home/") && path_str.contains("/snap/"))
        || (file_name == "snap" && path_str.contains("/home/"))
    {
        return GhostKind::SnapPackage;
    }

    // 7. Virtualization disks and ISO images
    if path_str.contains("/var/lib/libvirt/images")
        || path_str.contains("/VirtualBox VMs/")
        || path_str.contains("/.vagrant.d/boxes")
    {
        return GhostKind::VmOrIso;
    }
    if file_name.ends_with(".iso")
        || file_name.ends_with(".qcow2")
        || file_name.ends_with(".vdi")
        || file_name.ends_with(".vmdk")
        || file_name.ends_with(".ova")
        || file_name.ends_with(".qcow")
    {
        return GhostKind::VmOrIso;
    }

    // 8. AI & Machine Learning Models / Weights
    if path_str.contains("/.ollama/models")
        || path_str.contains("/ollama/.ollama/models")
        || path_str.contains("/huggingface/hub")
        || path_str.contains("/.cache/torch/hub")
        || path_str.contains("/.cache/torch/checkpoints")
    {
        return GhostKind::AiModel;
    }
    if file_name.ends_with(".gguf") || file_name.ends_with(".safetensors") {
        return GhostKind::AiModel;
    }

    // 9. Gaming compat, Steam shader caches, Proton & Wine
    if path_str.contains("/steamapps/shadercache")
        || path_str.contains("/steamapps/compatdata")
        || path_str.contains("/.wine/")
        || file_name == ".wine"
        || file_name == ".wine32"
        || file_name == ".wine64"
        || path_str.contains("/.local/share/lutris/runners")
        || path_str.contains("/.local/share/lutris/pfx")
        || path_str.contains("/heroic/tools")
        || path_str.contains("/heroic/prefixes")
    {
        return GhostKind::GamingCompat;
    }

    // 10. Web Browser & Electron app caches (MUST be evaluated before general .cache)
    if path_str.contains("/.cache/google-chrome")
        || path_str.contains("/.cache/chromium")
        || path_str.contains("/.cache/BraveSoftware")
        || path_str.contains("/.cache/mozilla/firefox")
        || path_str.contains("/.cache/microsoft-edge")
        || path_str.contains("/.cache/opera")
        || path_str.contains("/.cache/vivaldi")
        || path_str.contains("/discord/Cache")
        || path_str.contains("/discord/Code Cache")
        || path_str.contains("/Slack/Cache")
        || path_str.contains("/Slack/Code Cache")
        || path_str.contains("/Code/Cache")
        || path_str.contains("/Code/CachedData")
    {
        return GhostKind::BrowserCache;
    }

    // 11. System & App Logs
    if path_str.contains("/var/log")
        || path_str.contains("/.npm/_logs")
        || file_name == "journal"
        || file_name == "log"
        || file_name == "logs"
        || file_name.ends_with(".log")
        || file_name.ends_with(".log.gz")
        || file_name.ends_with(".log.1")
        || file_name.ends_with(".log.old")
    {
        return GhostKind::LogFiles;
    }

    // 12. Project dependencies (separated from transient build caches)
    match file_name {
        "node_modules" | "vendor" | ".venv" | "venv" | "site-packages" | "dist-packages" => {
            return GhostKind::DependencyTree;
        }
        _ => {}
    }

    // 13. Package manager caches (Arch pacman, apt, dnf, AUR helpers)
    if path_str.contains("/var/cache/pacman/pkg")
        || path_str.contains("/var/cache/apt/archives")
        || path_str.contains("/var/cache/dnf")
        || path_str.contains("/.cache/yay")
        || path_str.contains("/.cache/paru")
    {
        return GhostKind::PackageCache;
    }

    // 14. Common heavy build and compiler caches
    match file_name {
        "target" | "__pycache__" | ".pytest_cache" | ".next" | ".nuxt" | ".svelte-kit" | ".turbo"
        | ".gradle" | ".cargo/registry" | ".cargo/git" | "go-build" | ".mypy_cache"
        | ".ruff_cache" | "ccache" => {
            return GhostKind::BuildCache;
        }
        _ => {}
    }

    // 15. Fallback for general ~/.cache folders
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
            classify_path(&PathBuf::from("/home/ron/.local/share/Trash/files/doc.pdf")),
            GhostKind::Trash
        );
        assert_eq!(
            classify_path(&PathBuf::from("/mnt/storage/.Trash-1000/old.zip")),
            GhostKind::Trash
        );
        assert_eq!(
            classify_path(&PathBuf::from("/var/lib/systemd/coredump/core.bash.1000")),
            GhostKind::CoreDump
        );
        assert_eq!(
            classify_path(&PathBuf::from("/run/timeshift/backup/2026-09-01")),
            GhostKind::SystemSnapshot
        );
        assert_eq!(
            classify_path(&PathBuf::from("/.snapshots/42/snapshot")),
            GhostKind::SystemSnapshot
        );
        assert_eq!(
            classify_path(&PathBuf::from("/var/lib/flatpak/app/org.videolan.VLC")),
            GhostKind::Flatpak
        );
        assert_eq!(
            classify_path(&PathBuf::from("/home/ron/.var/app/com.discordapp.Discord")),
            GhostKind::Flatpak
        );
        assert_eq!(
            classify_path(&PathBuf::from("/home/ron/snap/spotify/current")),
            GhostKind::SnapPackage
        );
        assert_eq!(
            classify_path(&PathBuf::from("/var/lib/libvirt/images/arch.qcow2")),
            GhostKind::VmOrIso
        );
        assert_eq!(
            classify_path(&PathBuf::from("/home/ron/Downloads/archlinux.iso")),
            GhostKind::VmOrIso
        );
        assert_eq!(
            classify_path(&PathBuf::from("/home/ron/.ollama/models/blobs/sha256-abc")),
            GhostKind::AiModel
        );
        assert_eq!(
            classify_path(&PathBuf::from("/home/ron/.cache/huggingface/hub/models--meta--llama")),
            GhostKind::AiModel
        );
        assert_eq!(
            classify_path(&PathBuf::from("/home/ron/models/mistral.safetensors")),
            GhostKind::AiModel
        );
        assert_eq!(
            classify_path(&PathBuf::from("/home/ron/.local/share/Steam/steamapps/shadercache/12345")),
            GhostKind::GamingCompat
        );
        assert_eq!(
            classify_path(&PathBuf::from("/home/ron/.wine/drive_c")),
            GhostKind::GamingCompat
        );
        assert_eq!(
            classify_path(&PathBuf::from("/home/ron/.cache/google-chrome/Default/Cache")),
            GhostKind::BrowserCache
        );
        assert_eq!(
            classify_path(&PathBuf::from("/home/ron/.cache/mozilla/firefox/profile/cache2")),
            GhostKind::BrowserCache
        );
        assert_eq!(
            classify_path(&PathBuf::from("/var/log/pacman.log")),
            GhostKind::LogFiles
        );
        assert_eq!(
            classify_path(&PathBuf::from("/home/ron/project/app.log")),
            GhostKind::LogFiles
        );
        assert_eq!(
            classify_path(&PathBuf::from("/home/ron/project/node_modules")),
            GhostKind::DependencyTree
        );
        assert_eq!(
            classify_path(&PathBuf::from("/home/ron/project/.venv")),
            GhostKind::DependencyTree
        );
        assert_eq!(
            classify_path(&PathBuf::from("/var/cache/pacman/pkg")),
            GhostKind::PackageCache
        );
        assert_eq!(
            classify_path(&PathBuf::from("/home/ron/.cache/yay/google-chrome")),
            GhostKind::PackageCache
        );
        assert_eq!(
            classify_path(&PathBuf::from("/home/ron/project/target")),
            GhostKind::BuildCache
        );
        assert_eq!(
            classify_path(&PathBuf::from("/home/ron/.cache/thumbnails")),
            GhostKind::BuildCache
        );
        assert_eq!(
            classify_path(&PathBuf::from("/home/ron/documents/photo.jpg")),
            GhostKind::None
        );
    }
}

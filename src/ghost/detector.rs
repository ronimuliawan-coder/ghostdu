use crate::fs::entry::{DeleteSafety, GhostKind};
use std::path::{Component, Path};

/// Case-insensitive ASCII suffix match without allocating a lowercased copy.
/// `classify_path` runs per scanned entry, so a `to_lowercase()` here adds one
/// heap allocation per file. Byte comparison is safe (no char-boundary panic).
// ponytail: ASCII-only folding; these ASCII suffixes can't match non-ASCII names either way
fn has_suffix_ignore_ascii_case(name: &str, suffix: &str) -> bool {
    name.len() >= suffix.len()
        && name.as_bytes()[name.len() - suffix.len()..].eq_ignore_ascii_case(suffix.as_bytes())
}

/// True when a component slice begins at the filesystem root with exactly
/// these components (e.g. `["var", "log"]` matches `/var/log` and
/// `/var/log/journal`, never `/home/u/var/log`). Component-exact, so a user
/// directory named `var` cannot borrow a system location's classification
/// (audit M4). Takes the caller's component slice: `classify_path` builds one
/// per entry instead of one per rule (review round 1, Qodo finding 5).
fn starts_with_segments(components: &[Component<'_>], segments: &[&str]) -> bool {
    let mut iter = components.iter();
    match iter.next() {
        Some(Component::RootDir) => {}
        _ => return false,
    }
    for segment in segments {
        match iter.next() {
            Some(component) if component.as_os_str().as_encoded_bytes() == segment.as_bytes() => {}
            _ => return false,
        }
    }
    true
}

/// True when exactly these components appear contiguously anywhere in the
/// component slice (e.g. `[".local", "share", "Trash"]` matches any user's
/// trash directory). Segment-exact, so `myvar/lib/docker` or `.docker-backup`
/// cannot match a dot-directory rule they merely spell (audit M4).
fn contains_segments(components: &[Component<'_>], segments: &[&str]) -> bool {
    if segments.is_empty() {
        return false;
    }
    let body = match components {
        [Component::RootDir, rest @ ..] => rest,
        all => all,
    };
    if segments.len() > body.len() {
        return false;
    }
    body.windows(segments.len()).any(|window| {
        window.iter().zip(segments).all(|(component, segment)| {
            component.as_os_str().as_encoded_bytes() == segment.as_bytes()
        })
    })
}

/// True when any path component starts with `prefix` — for FreeDesktop
/// `.Trash-<uid>` wastebins, whose name varies by uid and whose contents must
/// classify as trash even though their own names differ.
fn any_component_starts_with(path: &Path, prefix: &str) -> bool {
    path.components().any(|component| {
        component
            .as_os_str()
            .as_encoded_bytes()
            .starts_with(prefix.as_bytes())
    })
}

pub fn classify_path(path: &Path) -> GhostKind {
    let path_str = path.to_string_lossy();
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    // One component slice per entry feeds every rule; the segment helpers take
    // it so classification allocates once, not per rule (review round 1).
    let components: Vec<Component<'_>> = path.components().collect();

    // 1. Wastebin / Trash locations (FreeDesktop & mount-point wastebins)
    if contains_segments(&components, &[".local", "share", "Trash"])
        || contains_segments(&components, &[".Trash"])
        || any_component_starts_with(path, ".Trash-")
        || file_name == ".Trash"
        || file_name == ".Trashes"
    {
        return GhostKind::Trash;
    }

    // 2. Crash Dumps & Coredumps
    if starts_with_segments(&components, &["var", "lib", "systemd", "coredump"])
        || starts_with_segments(&components, &["var", "crash"])
        || file_name.ends_with(".coredump")
        || (file_name.starts_with("core.") && starts_with_segments(&components, &["var"]))
    {
        return GhostKind::CoreDump;
    }

    // 3. System Snapshots (Timeshift, Snapper)
    if starts_with_segments(&components, &["run", "timeshift", "backup"])
        || starts_with_segments(&components, &["timeshift", "snapshots"])
        || starts_with_segments(&components, &["timeshift"])
        || contains_segments(&components, &[".snapshots"])
        || file_name == ".snapshots"
    {
        return GhostKind::SystemSnapshot;
    }

    // 4. Docker system locations
    if starts_with_segments(&components, &["var", "lib", "docker", "overlay2"]) {
        return GhostKind::DockerOverlay;
    }
    if starts_with_segments(&components, &["var", "lib", "docker", "volumes"]) {
        return GhostKind::DockerVolume;
    }
    if starts_with_segments(&components, &["var", "lib", "docker", "containers"]) {
        return GhostKind::DockerContainer;
    }
    if starts_with_segments(&components, &["var", "lib", "docker", "buildkit"]) {
        return GhostKind::DockerBuildkit;
    }
    // The daemon socket is a file named `docker.sock` directly under /var/run,
    // so the component directly beneath /var/run matches by prefix — searching
    // the rest of the path for "/docker" would sweep unrelated nested names
    // like /var/run/work/docker-notes (review round 2, Qodo finding b8a632a2).
    if starts_with_segments(&components, &["var", "lib", "docker"])
        || (starts_with_segments(&components, &["var", "run"])
            && components.len() > 3
            && components[3]
                .as_os_str()
                .as_encoded_bytes()
                .starts_with(b"docker"))
    {
        return GhostKind::DockerUser;
    }

    // 5. User-space Docker & Podman
    if contains_segments(&components, &[".local", "share", "docker"])
        || contains_segments(&components, &[".docker"])
    {
        return GhostKind::DockerUser;
    }
    if contains_segments(&components, &[".local", "share", "containers"])
        || contains_segments(&components, &[".config", "containers"])
    {
        return GhostKind::PodmanUser;
    }

    // 6. Flatpak & Snap runtimes and app data
    if starts_with_segments(&components, &["var", "lib", "flatpak"])
        || contains_segments(&components, &[".var", "app"])
        || file_name == ".var"
        || contains_segments(&components, &[".local", "share", "flatpak"])
    {
        return GhostKind::Flatpak;
    }
    if starts_with_segments(&components, &["var", "lib", "snapd"])
        || contains_segments(&components, &["snap"])
        || path_str.ends_with("/snap")
        || (file_name == "snap" && path_str.contains("/home/"))
    {
        return GhostKind::SnapPackage;
    }

    // 7. Virtualization disks and ISO images
    if starts_with_segments(&components, &["var", "lib", "libvirt", "images"])
        || contains_segments(&components, &["VirtualBox VMs"])
        || contains_segments(&components, &[".vagrant.d", "boxes"])
    {
        return GhostKind::VmOrIso;
    }
    if has_suffix_ignore_ascii_case(file_name, ".iso")
        || has_suffix_ignore_ascii_case(file_name, ".qcow2")
        || has_suffix_ignore_ascii_case(file_name, ".vdi")
        || has_suffix_ignore_ascii_case(file_name, ".vmdk")
        || has_suffix_ignore_ascii_case(file_name, ".ova")
        || has_suffix_ignore_ascii_case(file_name, ".qcow")
    {
        return GhostKind::VmOrIso;
    }

    // 8. AI & Machine Learning Models / Weights
    if contains_segments(&components, &[".ollama", "models"])
        || contains_segments(&components, &["ollama", ".ollama", "models"])
        || contains_segments(&components, &["huggingface", "hub"])
        || contains_segments(&components, &[".cache", "torch", "hub"])
        || contains_segments(&components, &[".cache", "torch", "checkpoints"])
    {
        return GhostKind::AiModel;
    }
    if has_suffix_ignore_ascii_case(file_name, ".gguf")
        || has_suffix_ignore_ascii_case(file_name, ".safetensors")
    {
        return GhostKind::AiModel;
    }

    // 9. Gaming compat, Steam shader caches, Proton & Wine
    if contains_segments(&components, &["steamapps", "shadercache"])
        || contains_segments(&components, &["steamapps", "compatdata"])
        || contains_segments(&components, &[".wine"])
        || file_name == ".wine"
        || file_name == ".wine32"
        || file_name == ".wine64"
        || contains_segments(&components, &[".local", "share", "lutris", "runners"])
        || contains_segments(&components, &[".local", "share", "lutris", "pfx"])
        || contains_segments(&components, &["heroic", "tools"])
        || contains_segments(&components, &["heroic", "prefixes"])
    {
        return GhostKind::GamingCompat;
    }

    // 10. Web Browser & Electron app caches (MUST be evaluated before general .cache)
    if contains_segments(&components, &[".cache", "google-chrome"])
        || contains_segments(&components, &[".cache", "chromium"])
        || contains_segments(&components, &[".cache", "BraveSoftware"])
        || contains_segments(&components, &[".cache", "mozilla", "firefox"])
        || contains_segments(&components, &[".cache", "microsoft-edge"])
        || contains_segments(&components, &[".cache", "opera"])
        || contains_segments(&components, &[".cache", "vivaldi"])
        || contains_segments(&components, &["discord", "Cache"])
        || contains_segments(&components, &["discord", "Code Cache"])
        || contains_segments(&components, &["Slack", "Cache"])
        || contains_segments(&components, &["Slack", "Code Cache"])
        || contains_segments(&components, &["Code", "Cache"])
        || contains_segments(&components, &["Code", "CachedData"])
    {
        return GhostKind::BrowserCache;
    }

    // 11. System & App Logs (require directory context or recognizable log extensions)
    if starts_with_segments(&components, &["var", "log"])
        || contains_segments(&components, &[".npm", "_logs"])
        || (contains_segments(&components, &[".local", "state"])
            && (file_name.eq_ignore_ascii_case("log") || file_name.eq_ignore_ascii_case("logs")))
        || (file_name.eq_ignore_ascii_case("journal") && contains_segments(&components, &["log"]))
        || has_suffix_ignore_ascii_case(file_name, ".log")
        || has_suffix_ignore_ascii_case(file_name, ".log.gz")
        || has_suffix_ignore_ascii_case(file_name, ".log.1")
        || has_suffix_ignore_ascii_case(file_name, ".log.old")
    {
        return GhostKind::LogFiles;
    }

    // 12. Project dependencies (separated from transient build caches)
    // Matches dependency root as well as nested files inside dependencies
    if matches!(
        file_name,
        "node_modules" | "vendor" | ".venv" | "venv" | "site-packages" | "dist-packages"
    ) || contains_segments(&components, &["node_modules"])
        || contains_segments(&components, &[".venv"])
        || contains_segments(&components, &["venv"])
        || contains_segments(&components, &["vendor"])
        || contains_segments(&components, &["site-packages"])
        || contains_segments(&components, &["dist-packages"])
    {
        return GhostKind::DependencyTree;
    }

    // 13. Package manager caches (Arch pacman, apt, dnf, AUR helpers)
    if starts_with_segments(&components, &["var", "cache", "pacman", "pkg"])
        || starts_with_segments(&components, &["var", "cache", "apt", "archives"])
        || starts_with_segments(&components, &["var", "cache", "dnf"])
        || contains_segments(&components, &[".cache", "yay"])
        || contains_segments(&components, &[".cache", "paru"])
    {
        return GhostKind::PackageCache;
    }

    // 14. Common heavy build and compiler caches
    // Matches build cache root as well as nested files inside
    if matches!(
        file_name,
        "target"
            | "__pycache__"
            | ".pytest_cache"
            | ".next"
            | ".nuxt"
            | ".svelte-kit"
            | ".turbo"
            | ".gradle"
            | "go-build"
            | ".mypy_cache"
            | ".ruff_cache"
            | "ccache"
    ) || path_str.ends_with("/.cargo/registry")
        || path_str.ends_with("/.cargo/git")
        || contains_segments(&components, &["__pycache__"])
        || contains_segments(&components, &[".pytest_cache"])
        || contains_segments(&components, &[".next"])
        || contains_segments(&components, &[".nuxt"])
        || contains_segments(&components, &[".turbo"])
        || contains_segments(&components, &[".gradle"])
        || contains_segments(&components, &[".cargo", "registry"])
        || contains_segments(&components, &[".cargo", "git"])
        || contains_segments(&components, &["go-build"])
        || contains_segments(&components, &[".mypy_cache"])
        || contains_segments(&components, &[".ruff_cache"])
    {
        return GhostKind::BuildCache;
    }

    // 15. Recognised cache units inside generic containers. An ancestor `.cache`
    // alone must not mark arbitrary contents as cache: only well-known units
    // (plus the specific rules above) classify, so personal files under an
    // innocent-looking cache path stay UserData.
    if is_recognized_cache_unit(path) {
        return GhostKind::BuildCache;
    }

    GhostKind::None
}

/// Well-known cache directory names. Compared per path segment (byte-exact, so
/// non-UTF-8 segments never match) rather than by substring.
const RECOGNIZED_CACHE_UNITS: [&str; 10] = [
    "thumbnails",
    "fontconfig",
    "mesa_shader_cache",
    "pip",
    "uv",
    "npm",
    "yarn",
    "pnpm",
    "cargo",
    "mozilla",
];

fn is_recognized_cache_unit(path: &Path) -> bool {
    // Units count only directly beneath `.cache`: deeper nesting such as
    // `.cache/personal/pip` is a personal path that happens to contain a
    // cache-like name, not a cache. The XDG cache root itself is a cache unit
    // too (user-found-issues #1 — the tightened unit matching had
    // declassified the root), scoped to a `.cache` directly inside a home
    // directory: a project-local `.cache` is personal data unless proven
    // otherwise (review round 3, Qodo finding 42ccfb96).
    let mut under_cache = false;
    let mut components = path.components().peekable();
    while let Some(comp) = components.next() {
        let bytes = comp.as_os_str().as_encoded_bytes();
        if under_cache {
            return RECOGNIZED_CACHE_UNITS
                .iter()
                .any(|unit| bytes == unit.as_bytes());
        }
        if bytes == b".cache" {
            if components.peek().is_none() {
                return is_home_cache_root(path);
            }
            under_cache = true;
        }
    }
    false
}

/// True when `path`'s parent is a home directory: a user directory directly
/// under `/home`, or `/root`. Exotic `XDG_CACHE_HOME` overrides fall through
/// to UserData — the conservative direction.
fn is_home_cache_root(path: &Path) -> bool {
    // A path ending in `.cache` always has a parent; the empty fallback only
    // serves the type and classifies as false below.
    let parent = path.parent().unwrap_or(Path::new(""));
    let mut parent_components = parent.components();
    match parent_components.next() {
        Some(Component::RootDir) => {}
        _ => return false,
    }
    let mut second_last: Option<Component<'_>> = None;
    let mut last: Option<Component<'_>> = None;
    let mut depth = 0;
    for component in parent_components {
        second_last = last.take();
        last = Some(component);
        depth += 1;
    }
    if depth == 1 {
        return last.is_some_and(|c| c.as_os_str().as_encoded_bytes() == b"root");
    }
    depth >= 2 && second_last.is_some_and(|c| c.as_os_str().as_encoded_bytes() == b"home")
}

/// Classifies a path and its ghost kind into a Deletion Safety Tier:
/// - Safe: Caches, trash, crash dumps, rotated logs, build artifacts (can be recreated safely)
/// - Recheck: Dependencies, models, ISOs, VM disks, snapshots (reproducible with cost/bandwidth)
/// - UserData: Documents, source code, personal files, configuration (permanent loss)
/// - System: Critical OS roots and directories (/usr, /etc, /boot, /bin, etc. — deletion blocked)
pub fn classify_safety(path: &Path, ghost: GhostKind) -> DeleteSafety {
    let path_str = path.to_string_lossy();
    let p = path_str.as_ref();

    // 1. Critical System Directories (Deleting these will brick or severely impair Linux)
    if p == "/"
        || p == "/bin"
        || p == "/sbin"
        || p == "/boot"
        || p == "/etc"
        || p == "/lib"
        || p == "/lib64"
        || p == "/lib32"
        || p == "/usr"
        || p == "/usr/bin"
        || p == "/usr/sbin"
        || p == "/usr/lib"
        || p == "/usr/lib64"
        || p == "/usr/include"
        || p == "/sys"
        || p == "/proc"
        || p == "/dev"
        || p == "/run"
        || p == "/root"
        || p == "/var"
        || p == "/var/lib"
        || p == "/var/lib/systemd"
    {
        return DeleteSafety::System;
    }

    // Direct children under critical system roots. The prefix set must cover
    // every root whose contents classify as deletable User tier otherwise
    // (audit H1: /bin, /sbin, /lib32 contents on layouts where those are real
    // directories). No substring carve-outs: a `/usr` path merely spelling
    // "cache"/"flatpak"/"snap" stayed deletable under the previous carve-out
    // (audit M5), the wrong direction for a destructive-path check. /run and
    // /dev are deliberately absent: the scanner skips those virtual filesystems,
    // deleted-open ghost cleanup legitimately targets live /dev/shm files, and
    // a blanket prefix would block that feature for imported paths too.
    if p.starts_with("/etc/")
        || p.starts_with("/boot/")
        || p.starts_with("/lib/")
        || p.starts_with("/lib64/")
        || p.starts_with("/lib32/")
        || p.starts_with("/bin/")
        || p.starts_with("/sbin/")
        || p.starts_with("/usr/")
    {
        return DeleteSafety::System;
    }

    // 2. Safe to remove (Caches, Trashes, Crash Dumps, Rotated Logs, Ephemeral Caches)
    match ghost {
        GhostKind::Trash
        | GhostKind::BuildCache
        | GhostKind::PackageCache
        | GhostKind::BrowserCache
        | GhostKind::CoreDump
        | GhostKind::DeletedOpen => {
            return DeleteSafety::Safe;
        }
        // Steam shadercache is 100% safe to remove; compatdata / wine prefixes contain prefixes/saves so recheck
        GhostKind::GamingCompat => {
            if p.contains("/shadercache") {
                return DeleteSafety::Safe;
            } else {
                return DeleteSafety::Recheck;
            }
        }
        // Rotated or old logs or journal logs are safe to clean, but /var/log container itself is Recheck!
        GhostKind::LogFiles => {
            if p == "/var/log" || p == "/var/log/" {
                return DeleteSafety::Recheck;
            }
            return DeleteSafety::Safe;
        }
        // Dependencies, AI weights, ISOs, Snapshots, Flatpak/Snap app data, Docker volumes require recheck
        // Docker overlay & buildkit MUST be pruned via daemon prune, never raw rm -rf
        GhostKind::DockerOverlay
        | GhostKind::DockerBuildkit
        | GhostKind::DependencyTree
        | GhostKind::AiModel
        | GhostKind::VmOrIso
        | GhostKind::SystemSnapshot
        | GhostKind::Flatpak
        | GhostKind::SnapPackage
        | GhostKind::DockerVolume
        | GhostKind::DockerContainer
        | GhostKind::DockerUser
        | GhostKind::PodmanUser => {
            return DeleteSafety::Recheck;
        }
        GhostKind::None => {}
    }

    // Root temp mount points are protected system directories
    if p == "/tmp" || p == "/var/tmp" {
        return DeleteSafety::System;
    }

    // Ephemeral temporary files, editor backup/swap files
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if file_name.ends_with(".tmp")
        || file_name.ends_with(".temp")
        || file_name.ends_with(".bak")
        || file_name.ends_with(".swp")
        || file_name.ends_with("~")
        || file_name.starts_with(".#")
    {
        return DeleteSafety::Safe;
    }

    // Default to UserData
    DeleteSafety::UserData
}

/// Returns true if a given path is an inherently virtual/pseudo Linux filesystem
/// that should never be traversed when analyzing disk usage.
pub fn is_virtual_fs_path(path: &Path) -> bool {
    let path_str = path.to_string_lossy();
    let p = path_str.as_ref();

    // Skip root virtual mounts
    p == "/proc"
        || p.starts_with("/proc/")
        || p == "/sys"
        || p.starts_with("/sys/")
        || p == "/dev"
        || p.starts_with("/dev/")
        || p == "/run"
        || p.starts_with("/run/")
        || p == "/sys/firmware"
        || p.starts_with("/sys/firmware/")
        || p == "/sys/kernel"
        || p.starts_with("/sys/kernel/")
        || p == "/sys/fs/cgroup"
        || p.starts_with("/sys/fs/cgroup/")
        || p == "/dev/shm"
        || p.starts_with("/dev/shm/")
        || p == "/dev/pts"
        || p.starts_with("/dev/pts/")
        || p.starts_with("/var/lib/snapd/mnt/")
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
            classify_path(&PathBuf::from("/var/lib/docker/network/files/local-kv.db")),
            GhostKind::DockerUser
        );
        assert_eq!(
            classify_path(&PathBuf::from("/var/run/docker.sock")),
            GhostKind::DockerUser
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
            classify_path(&PathBuf::from("/home/ron/Downloads/archlinux.ISO")),
            GhostKind::VmOrIso
        );
        assert_eq!(
            classify_path(&PathBuf::from("/home/ron/models/mistral.GGUF")),
            GhostKind::AiModel
        );
        assert_eq!(
            classify_path(&PathBuf::from("/home/ron/project/APP.LOG")),
            GhostKind::LogFiles
        );
        assert_eq!(
            classify_path(&PathBuf::from("/home/ron/.ollama/models/blobs/sha256-abc")),
            GhostKind::AiModel
        );
        assert_eq!(
            classify_path(&PathBuf::from(
                "/home/ron/.cache/huggingface/hub/models--meta--llama"
            )),
            GhostKind::AiModel
        );
        assert_eq!(
            classify_path(&PathBuf::from("/home/ron/models/mistral.safetensors")),
            GhostKind::AiModel
        );
        assert_eq!(
            classify_path(&PathBuf::from(
                "/home/ron/.local/share/Steam/steamapps/shadercache/12345"
            )),
            GhostKind::GamingCompat
        );
        assert_eq!(
            classify_path(&PathBuf::from("/home/ron/.wine/drive_c")),
            GhostKind::GamingCompat
        );
        assert_eq!(
            classify_path(&PathBuf::from(
                "/home/ron/.cache/google-chrome/Default/Cache"
            )),
            GhostKind::BrowserCache
        );
        assert_eq!(
            classify_path(&PathBuf::from(
                "/home/ron/.cache/mozilla/firefox/profile/cache2"
            )),
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
        // An ancestor `.cache` alone must not bless arbitrary contents.
        assert_eq!(
            classify_path(&PathBuf::from("/home/alice/.cache/personal")),
            GhostKind::None
        );
        assert_eq!(
            classify_path(&PathBuf::from("/home/alice/.cache/personal/notes.txt")),
            GhostKind::None
        );
        // Recognised units outside `.cache` are personal paths, not caches.
        assert_eq!(
            classify_path(&PathBuf::from("/home/alice/pip/notes.txt")),
            GhostKind::None
        );
        assert_eq!(
            classify_path(&PathBuf::from("/home/alice/projects/cargo/report.txt")),
            GhostKind::None
        );
        assert_eq!(
            classify_path(&PathBuf::from("/home/ron/.cache/pip/cache.dat")),
            GhostKind::BuildCache
        );
        // Units nested below an unrecognized directory are not caches.
        assert_eq!(
            classify_path(&PathBuf::from("/home/alice/.cache/personal/pip/notes.txt")),
            GhostKind::None
        );
        // The XDG cache root itself is a cache unit (user-found-issues #1).
        assert_eq!(
            classify_path(&PathBuf::from("/home/ron/.cache")),
            GhostKind::BuildCache
        );
        assert_eq!(
            classify_path(&PathBuf::from("/home/ron/documents/photo.jpg")),
            GhostKind::None
        );
        assert_eq!(
            classify_path(&PathBuf::from("/home/ron/.docker/config.json")),
            GhostKind::DockerUser
        );
        assert_eq!(
            classify_path(&PathBuf::from("/home/ron/.local/state/app/log")),
            GhostKind::LogFiles
        );
        assert_eq!(
            classify_safety(&PathBuf::from("/var/log"), GhostKind::LogFiles),
            DeleteSafety::Recheck
        );
    }

    #[test]
    fn test_safety_classification() {
        // System protected paths
        assert_eq!(
            classify_safety(&PathBuf::from("/"), GhostKind::None),
            DeleteSafety::System
        );
        assert_eq!(
            classify_safety(&PathBuf::from("/usr"), GhostKind::None),
            DeleteSafety::System
        );
        assert_eq!(
            classify_safety(&PathBuf::from("/usr/bin/python"), GhostKind::None),
            DeleteSafety::System
        );
        assert_eq!(
            classify_safety(&PathBuf::from("/etc/fstab"), GhostKind::None),
            DeleteSafety::System
        );
        assert_eq!(
            classify_safety(&PathBuf::from("/boot"), GhostKind::None),
            DeleteSafety::System
        );
        assert_eq!(
            classify_safety(&PathBuf::from("/tmp"), GhostKind::None),
            DeleteSafety::System
        );

        // Safe to remove (caches, trash, logs, coredumps)
        assert_eq!(
            classify_safety(
                &PathBuf::from("/home/ron/.local/share/Trash"),
                GhostKind::Trash
            ),
            DeleteSafety::Safe
        );
        assert_eq!(
            classify_safety(
                &PathBuf::from("/home/ron/project/target"),
                GhostKind::BuildCache
            ),
            DeleteSafety::Safe
        );
        assert_eq!(
            classify_safety(
                &PathBuf::from("/home/ron/.cache/google-chrome"),
                GhostKind::BrowserCache
            ),
            DeleteSafety::Safe
        );
        assert_eq!(
            classify_safety(
                &PathBuf::from("/var/lib/systemd/coredump"),
                GhostKind::CoreDump
            ),
            DeleteSafety::Safe
        );
        assert_eq!(
            classify_safety(&PathBuf::from("/var/log/pacman.log"), GhostKind::LogFiles),
            DeleteSafety::Safe
        );
        assert_eq!(
            classify_safety(&PathBuf::from("/tmp/scratch.tmp"), GhostKind::None),
            DeleteSafety::Safe
        );
        assert_eq!(
            classify_safety(&PathBuf::from("/tmp/scratch.txt"), GhostKind::None),
            DeleteSafety::UserData
        );
        assert_eq!(
            classify_safety(
                &PathBuf::from("/home/ron/.local/share/Steam/steamapps/shadercache"),
                GhostKind::GamingCompat
            ),
            DeleteSafety::Safe
        );

        // Recheck items (deps, models, ISOs, snapshots)
        assert_eq!(
            classify_safety(
                &PathBuf::from("/home/ron/project/node_modules"),
                GhostKind::DependencyTree
            ),
            DeleteSafety::Recheck
        );
        assert_eq!(
            classify_safety(
                &PathBuf::from("/home/ron/ai/llama.safetensors"),
                GhostKind::AiModel
            ),
            DeleteSafety::Recheck
        );
        assert_eq!(
            classify_safety(
                &PathBuf::from("/home/ron/Downloads/arch.iso"),
                GhostKind::VmOrIso
            ),
            DeleteSafety::Recheck
        );
        assert_eq!(
            classify_safety(
                &PathBuf::from("/.snapshots/1/snapshot"),
                GhostKind::SystemSnapshot
            ),
            DeleteSafety::Recheck
        );
        assert_eq!(
            classify_safety(&PathBuf::from("/home/ron/.wine"), GhostKind::GamingCompat),
            DeleteSafety::Recheck
        );

        // User data
        assert_eq!(
            classify_safety(
                &PathBuf::from("/home/ron/Documents/notes.txt"),
                GhostKind::None
            ),
            DeleteSafety::UserData
        );
        assert_eq!(
            classify_safety(
                &PathBuf::from("/home/ron/Projects/src/main.rs"),
                GhostKind::None
            ),
            DeleteSafety::UserData
        );
    }

    #[test]
    fn segment_helpers_are_component_exact() {
        fn components_of(path: &Path) -> Vec<Component<'_>> {
            path.components().collect()
        }
        assert!(starts_with_segments(
            &components_of(Path::new("/var/log/journal")),
            &["var", "log"]
        ));
        assert!(!starts_with_segments(
            &components_of(Path::new("/home/u/var/log")),
            &["var", "log"]
        ));
        assert!(!starts_with_segments(
            &components_of(Path::new("var/log")),
            &["var", "log"]
        ));
        assert!(contains_segments(
            &components_of(Path::new("/home/u/.local/share/Trash/files")),
            &[".local", "share", "Trash"]
        ));
        assert!(!contains_segments(
            &components_of(Path::new("/home/u/myvar/lib/docker")),
            &["var", "lib", "docker"]
        ));
        assert!(!contains_segments(
            &components_of(Path::new("/home/u/.docker-backup")),
            &[".docker"]
        ));
        assert!(!contains_segments(
            &components_of(Path::new("/var/log")),
            &[]
        ));
        // Relative paths (snapshot imports) classify on their components too.
        assert!(contains_segments(
            &components_of(Path::new("u/.cache/pip")),
            &[".cache", "pip"]
        ));
        assert!(any_component_starts_with(
            Path::new("/mnt/storage/.Trash-1000/x"),
            ".Trash-"
        ));
        assert!(!any_component_starts_with(
            Path::new("/mnt/storage/trash-1000"),
            ".Trash-"
        ));
    }

    /// Audit H1: on layouts where /bin, /sbin and /lib32 are real directories,
    /// their contents classified as deletable User tier. usr-merge layouts were
    /// already saved by `prepare_target`'s canonicalization to /usr/...; this
    /// closes the classification itself.
    #[test]
    fn system_dir_contents_are_blocked_regardless_of_usr_merge() {
        for path in [
            "/bin",
            "/sbin",
            "/lib32",
            "/bin/ls",
            "/sbin/fsck.ext4",
            "/lib32/libc.so.6",
        ] {
            assert_eq!(
                classify_safety(Path::new(path), GhostKind::None),
                DeleteSafety::System,
                "{path} must be System"
            );
        }
    }

    /// Audit H1, classification-level defense: /run and /dev are
    /// scanner-skipped virtual filesystems, so a hand-built or imported path
    /// under them cannot present as User tier. /run/timeshift backup content is
    /// deliberately System: a live mounted snapshot must not be rm'd.
    #[test]
    fn virtual_fs_subtrees_keep_the_classifier_in_charge() {
        // /dev and /run are deliberately not blanket System prefixes: deleted-open
        // ghost cleanup legitimately targets live /dev/shm files, and the scanner
        // already skips virtual filesystems, so imported paths there keep the
        // classifier's judgment instead of a blanket block (audit H1 scoped the
        // prefix additions to /bin, /sbin, /lib32).
        assert_eq!(
            classify_safety(
                &PathBuf::from("/dev/shm/ghostdu-orphan"),
                GhostKind::DeletedOpen
            ),
            DeleteSafety::Safe
        );
        assert_eq!(
            classify_safety(&PathBuf::from("/run/user/1000/leftover"), GhostKind::None),
            DeleteSafety::UserData
        );
    }

    /// Audit M4: a user directory literally named `var` cannot borrow system
    /// location classifications. Previously `contains("/var/log")`,
    /// `contains("/var/crash")`, `contains("/var/lib/docker")` and
    /// `contains(".docker")` matched these innocent paths — some into Safe tier.
    #[test]
    fn user_paths_cannot_borrow_system_location_names() {
        assert_eq!(
            classify_path(&PathBuf::from("/home/u/var/logging/notes.txt")),
            GhostKind::None
        );
        assert_eq!(
            classify_path(&PathBuf::from("/home/u/var/lib/docker/x")),
            GhostKind::None
        );
        assert_eq!(
            classify_path(&PathBuf::from("/home/u/var/crash-reports/x.txt")),
            GhostKind::None
        );
        assert_eq!(
            classify_path(&PathBuf::from("/home/u/.docker-backup/data")),
            GhostKind::None
        );
        assert_eq!(
            classify_path(&PathBuf::from("/home/u/var/core.dump.txt")),
            GhostKind::None
        );
        // Root-anchored system locations still classify.
        assert_eq!(
            classify_path(&PathBuf::from("/var/log/journal")),
            GhostKind::LogFiles
        );
        assert_eq!(
            classify_path(&PathBuf::from("/home/u/.docker/config")),
            GhostKind::DockerUser
        );
        assert_eq!(
            classify_path(&PathBuf::from("/home/u/.local/share/Trash/files/x")),
            GhostKind::Trash
        );
        // The /var/run socket rule matches only the component directly beneath
        // it (review round 2): the daemon socket and docker directories stay,
        // unrelated nested names do not borrow the badge.
        assert_eq!(
            classify_path(&PathBuf::from("/var/run/docker.sock")),
            GhostKind::DockerUser
        );
        assert_eq!(
            classify_path(&PathBuf::from("/var/run/docker/network/files/local-kv.db")),
            GhostKind::DockerUser
        );
        assert_eq!(
            classify_path(&PathBuf::from("/var/run/work/docker-notes")),
            GhostKind::None
        );
    }

    /// Audit M5: /usr contents are System unconditionally. The removed carve-out
    /// let a /usr path merely spelling "cache"/"flatpak"/"snap" fall through to
    /// User tier — the wrong direction for a destructive-path check.
    #[test]
    fn usr_contents_are_system_without_substring_carve_outs() {
        for path in [
            "/usr/share/doc/cache-notes",
            "/usr/lib/snapd/squashfs",
            "/usr/local/flatpak-doc",
        ] {
            assert_eq!(
                classify_safety(Path::new(path), GhostKind::None),
                DeleteSafety::System,
                "{path} must be System"
            );
        }
    }

    /// User-found-issues #1: the XDG cache root itself classifies as BuildCache
    /// so `~/.cache` regains its CACHE badge; the tightened unit matching had
    /// declassified the root. Deeper personal nesting stays UserData.
    #[test]
    fn cache_root_classifies_as_build_cache() {
        assert_eq!(
            classify_path(&PathBuf::from("/home/ron/.cache")),
            GhostKind::BuildCache
        );
        // Non-home `.cache` roots are not the XDG cache root (review round 3);
        // relative snapshot paths classify the same way.
        assert_eq!(classify_path(&PathBuf::from("/.cache")), GhostKind::None);
        assert_eq!(classify_path(&PathBuf::from("u/.cache")), GhostKind::None);
        assert_eq!(
            classify_path(&PathBuf::from("/root/.cache")),
            GhostKind::BuildCache
        );
        assert_eq!(
            classify_path(&PathBuf::from("/home/u/proj/.cache")),
            GhostKind::None
        );
        assert_eq!(
            classify_path(&PathBuf::from("/home/ron/.cache/personal/notes.txt")),
            GhostKind::None
        );
        assert_eq!(
            classify_safety(&PathBuf::from("/home/ron/.cache"), GhostKind::BuildCache),
            DeleteSafety::Safe
        );
    }
}

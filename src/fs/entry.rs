use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum GhostKind {
    None,
    DockerOverlay,
    DockerVolume,
    DockerContainer,
    DockerBuildkit,
    DockerUser,
    PodmanUser,
    BuildCache,
    PackageCache,
    DeletedOpen,
    Trash,
    LogFiles,
    Flatpak,
    SnapPackage,
    DependencyTree,
    GamingCompat,
    AiModel,
    VmOrIso,
    BrowserCache,
    CoreDump,
    SystemSnapshot,
}

#[allow(dead_code)]
impl GhostKind {
    pub fn is_ghost(&self) -> bool {
        !matches!(self, GhostKind::None)
    }

    pub fn is_docker(&self) -> bool {
        matches!(
            self,
            GhostKind::DockerOverlay
                | GhostKind::DockerVolume
                | GhostKind::DockerContainer
                | GhostKind::DockerBuildkit
                | GhostKind::DockerUser
                | GhostKind::PodmanUser
        )
    }

    pub fn label(&self) -> &'static str {
        match self {
            GhostKind::None => "",
            GhostKind::DockerOverlay => "🐳 Docker-Overlay",
            GhostKind::DockerVolume => "🐳 Docker-Volume",
            GhostKind::DockerContainer => "🐳 Docker-Container",
            GhostKind::DockerBuildkit => "🐳 Docker-Buildkit",
            GhostKind::DockerUser => "🐳 Docker-User",
            GhostKind::PodmanUser => "🦭 Podman",
            GhostKind::BuildCache => "👻 Build-Cache",
            GhostKind::PackageCache => "📦 Pkg-Cache",
            GhostKind::DeletedOpen => "👻 Deleted-Open",
            GhostKind::Trash => "🗑️ Wastebin/Trash",
            GhostKind::LogFiles => "📜 System/App Logs",
            GhostKind::Flatpak => "📦 Flatpak",
            GhostKind::SnapPackage => "📦 Snap Package",
            GhostKind::DependencyTree => "📦 Dependencies",
            GhostKind::GamingCompat => "🎮 Game/Shaders",
            GhostKind::AiModel => "🤖 AI Model",
            GhostKind::VmOrIso => "💿 VM Disk / ISO",
            GhostKind::BrowserCache => "🌐 Browser Cache",
            GhostKind::CoreDump => "💥 Crash Dump",
            GhostKind::SystemSnapshot => "🔒 Snapshot",
        }
    }

    pub fn badge(&self) -> &'static str {
        match self {
            GhostKind::None => "",
            GhostKind::DockerOverlay
            | GhostKind::DockerVolume
            | GhostKind::DockerContainer
            | GhostKind::DockerBuildkit
            | GhostKind::DockerUser => "🐳 DOCKER",
            GhostKind::PodmanUser => "🦭 PODMAN",
            GhostKind::BuildCache => "👻 CACHE",
            GhostKind::PackageCache => "📦 PKG",
            GhostKind::DeletedOpen => "👻 GHOST",
            GhostKind::Trash => "🗑️ TRASH",
            GhostKind::LogFiles => "📜 LOGS",
            GhostKind::Flatpak => "📦 FLATPAK",
            GhostKind::SnapPackage => "📦 SNAP",
            GhostKind::DependencyTree => "📦 DEPS",
            GhostKind::GamingCompat => "🎮 GAME",
            GhostKind::AiModel => "🤖 AI",
            GhostKind::VmOrIso => "💿 VM/ISO",
            GhostKind::BrowserCache => "🌐 BROWSER",
            GhostKind::CoreDump => "💥 CRASH",
            GhostKind::SystemSnapshot => "🔒 SNAP",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(dead_code)]
pub enum DeleteSafety {
    Safe,    // 🟢 Safe to remove, transient/ephemeral/cache
    Recheck, // 🟡 Recheck/reproducible with cost (deps, models, isos)
    #[default]
    UserData, // ⚪ User personal files or source code
    System,  // 🔴 Critical system directory/file - deletion blocked or dangerous
}

#[allow(dead_code)]
impl DeleteSafety {
    pub fn glyph(&self) -> &'static str {
        match self {
            DeleteSafety::Safe => "🟢",
            DeleteSafety::Recheck => "🟡",
            DeleteSafety::UserData => "⚪",
            DeleteSafety::System => "🔴",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            DeleteSafety::Safe => "Safe to Remove",
            DeleteSafety::Recheck => "Caution / Reproducible",
            DeleteSafety::UserData => "User Data",
            DeleteSafety::System => "System Protected",
        }
    }

    pub fn badge(&self) -> &'static str {
        match self {
            DeleteSafety::Safe => "SAFE",
            DeleteSafety::Recheck => "RECHECK",
            DeleteSafety::UserData => "USER",
            DeleteSafety::System => "SYSTEM",
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            DeleteSafety::Safe => "Temporary cache, trash, or build artifact — safe to delete; automatically recreated if needed.",
            DeleteSafety::Recheck => "Dependency tree, downloaded model, or VM disk — safe to purge, but requires network or time to restore.",
            DeleteSafety::UserData => "Personal file, source code, or configuration — permanent loss if deleted.",
            DeleteSafety::System => "Critical operating system directory or binary — deletion is blocked to prevent breaking your OS.",
        }
    }

    pub fn is_safe(&self) -> bool {
        matches!(self, DeleteSafety::Safe)
    }

    pub fn is_system(&self) -> bool {
        matches!(self, DeleteSafety::System)
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct FileEntry {
    pub name: String,
    pub path: PathBuf,
    pub size: u64,          // Apparent file size in bytes
    pub disk_usage: u64,    // Allocated disk space (blocks * 512)
    pub items_count: usize, // Total recursive items count
    pub is_dir: bool,
    pub is_symlink: bool,
    pub dev: u64,
    pub ino: u64,
    pub ghost_kind: GhostKind,
    pub delete_safety: DeleteSafety,
    pub has_err: bool, // e.g. permission denied
    pub children: Vec<FileEntry>,
}

impl FileEntry {
    #[allow(clippy::too_many_arguments)]
    pub fn new_file(
        name: String,
        path: PathBuf,
        size: u64,
        disk_usage: u64,
        is_symlink: bool,
        dev: u64,
        ino: u64,
        ghost_kind: GhostKind,
        delete_safety: DeleteSafety,
    ) -> Self {
        Self {
            name,
            path,
            size,
            disk_usage,
            items_count: 1,
            is_dir: false,
            is_symlink,
            dev,
            ino,
            ghost_kind,
            delete_safety,
            has_err: false,
            children: Vec::new(),
        }
    }

    pub fn new_dir(
        name: String,
        path: PathBuf,
        dev: u64,
        ino: u64,
        ghost_kind: GhostKind,
        delete_safety: DeleteSafety,
    ) -> Self {
        Self {
            name,
            path,
            size: 0,
            disk_usage: 0,
            items_count: 1,
            is_dir: true,
            is_symlink: false,
            dev,
            ino,
            ghost_kind,
            delete_safety,
            has_err: false,
            children: Vec::new(),
        }
    }

    pub fn display_size(&self, apparent: bool) -> u64 {
        if apparent {
            self.size
        } else {
            self.disk_usage
        }
    }

    pub fn safe_reclaimable_bytes(&self) -> u64 {
        self.children
            .iter()
            .filter(|c| c.delete_safety == DeleteSafety::Safe)
            .map(|c| c.disk_usage)
            .sum()
    }

    pub fn safe_items_count(&self) -> usize {
        self.children
            .iter()
            .filter(|c| c.delete_safety == DeleteSafety::Safe)
            .count()
    }
}

pub fn format_size(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;
    const TIB: u64 = GIB * 1024;

    if bytes >= TIB {
        format!("{:.2} TiB", bytes as f64 / TIB as f64)
    } else if bytes >= GIB {
        format!("{:.2} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{} B", bytes)
    }
}

pub fn format_count_short(count: usize) -> String {
    if count >= 1_000_000_000 {
        format!("{:.1}B", count as f64 / 1_000_000_000.0)
    } else if count >= 1_000_000 {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    } else if count >= 1_000 {
        format!("{:.1}k", count as f64 / 1_000.0)
    } else {
        format!("{}", count)
    }
}

pub fn format_count(count: usize) -> String {
    let s = format_count_short(count);
    if count == 1 {
        format!("{} item", s)
    } else {
        format!("{} items", s)
    }
}

use super::delete::{prepare_target, verify_identity};
use rustix::fs::{
    mkdirat, openat2, renameat_with, unlinkat, AtFlags, Mode, OFlags, RenameFlags, ResolveFlags,
    CWD,
};
use std::collections::HashMap;
use std::ffi::{CStr, OsStr};
use std::fs::{self, File};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Default)]
pub struct TrashResult {
    pub succeeded: Vec<PathBuf>,
    pub failed: Vec<(PathBuf, String)>,
}

pub fn move_to_trash<P: AsRef<Path>>(paths: &[P]) -> TrashResult {
    trash_with_verification(paths, |_, _| Ok(()))
}

pub(crate) fn move_to_trash_confirmed(
    paths: &[PathBuf],
    identities: &super::TargetIdentities,
) -> TrashResult {
    trash_with_verification(paths, |path, target| {
        super::verify_confirmed(identities, path, target)
    })
}

fn trash_with_verification<P: AsRef<Path>>(
    paths: &[P],
    verify: impl Fn(&Path, &File) -> io::Result<()>,
) -> TrashResult {
    trash_with_destination(paths, verify, trash_directory)
}

fn trash_with_destination<P: AsRef<Path>>(
    paths: &[P],
    verify: impl Fn(&Path, &File) -> io::Result<()>,
    mut resolve: impl FnMut(&File) -> io::Result<(File, Option<PathBuf>)>,
) -> TrashResult {
    let mut result = TrashResult::default();
    let mut destinations = HashMap::new();
    let topology = MountTopology::load();
    for path in paths {
        let path = path.as_ref();
        let moved: io::Result<()> = (|| {
            let (parent, name, target) = prepare_target(path)?;
            verify(path, &target)?;
            topology.check_target(&target)?;
            let original = fd_path(&parent)?.join(OsStr::from_bytes(name.to_bytes()));
            let destination = match destinations.entry(mount_id(&parent)?) {
                std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                std::collections::hash_map::Entry::Vacant(entry) => {
                    let (trash, topdir) = resolve(&parent)?;
                    entry.insert(TrashDestination::open(&parent, trash, topdir)?)
                }
            };
            let restore_path = match &destination.topdir {
                Some(topdir) => original.strip_prefix(topdir).map_err(io::Error::other)?,
                None => &original,
            };
            let noop = |_: MovePhase, _: &File, _: &str| {};
            move_prepared_with_hook(&parent, &name, &target, destination, restore_path, noop)
        })();
        match moved {
            Ok(()) => result.succeeded.push(path.to_path_buf()),
            Err(error) => result.failed.push((path.to_path_buf(), error.to_string())),
        }
    }
    result
}

#[derive(Default)]
struct MountTrieNode {
    is_mount: bool,
    children: HashMap<std::ffi::OsString, MountTrieNode>,
}

impl MountTrieNode {
    fn insert(&mut self, path: &Path) {
        let mut curr = self;
        for comp in path.components() {
            let key = comp.as_os_str().to_os_string();
            curr = curr.children.entry(key).or_default();
        }
        curr.is_mount = true;
    }

    fn has_mount_at_or_under(&self, path: &Path) -> bool {
        let mut curr = self;
        for comp in path.components() {
            match curr.children.get(comp.as_os_str()) {
                Some(next) => curr = next,
                None => return false,
            }
        }
        curr.is_mount || !curr.children.is_empty()
    }
}

/// Shared mount topology index loaded once per trash batch.
/// Evaluates directory targets against an in-memory prefix trie in O(depth) rather
/// than repeatedly rereading and parsing /proc/self/mountinfo on every target.
struct MountTopology {
    root: io::Result<MountTrieNode>,
}

impl MountTopology {
    fn load() -> Self {
        let root = fs::read("/proc/self/mountinfo").map(|topology| {
            let mut root = MountTrieNode::default();
            for mount in crate::fs::scanner::parse_mount_points(&topology) {
                root.insert(&mount);
            }
            root
        });
        Self { root }
    }

    fn check_target(&self, target: &File) -> io::Result<()> {
        if !target.metadata()?.is_dir() {
            return Ok(());
        }
        let root = match &self.root {
            Ok(root) => root,
            Err(e) => return Err(io::Error::new(e.kind(), e.to_string())),
        };
        let path = fd_path(target)?;
        if root.has_mount_at_or_under(&path) {
            return Err(io::Error::from_raw_os_error(libc::EXDEV));
        }
        Ok(())
    }
}

struct TrashDestination {
    // Keep the source mount alive for the lifetime of its cached mount ID.
    _source: File,
    trash: File,
    files: File,
    info: File,
    topdir: Option<PathBuf>,
}

impl TrashDestination {
    fn open(source: &File, trash: File, topdir: Option<PathBuf>) -> io::Result<Self> {
        let files = private_directory(&trash, Path::new("files"))?;
        let info = private_directory(&trash, Path::new("info"))?;
        Ok(Self {
            _source: source.try_clone()?,
            trash,
            files,
            info,
            topdir,
        })
    }

    fn validate(&self) -> io::Result<()> {
        // Recheck mutable permissions without repeating pathname setup/discovery.
        for directory in [&self.trash, &self.files, &self.info] {
            validate_private(directory)?;
        }
        Ok(())
    }
}

fn mount_id(directory: &File) -> io::Result<u64> {
    use rustix::fs::{statx, StatxFlags};
    let stat = statx(directory, "", AtFlags::EMPTY_PATH, StatxFlags::MNT_ID)?;
    // Eager error: built on every call so the line is always covered; it only
    // materializes on kernels without mount IDs. There is no device-ID
    // fallback: device IDs cannot distinguish bind mounts, and mount IDs
    // exist on all supported kernels.
    (stat.stx_mask & StatxFlags::MNT_ID.bits() != 0)
        .then_some(stat.stx_mnt_id)
        .ok_or(io::Error::other("kernel lacks statx mount IDs"))
}

fn fd_path(directory: &File) -> io::Result<PathBuf> {
    fs::read_link(format!("/proc/self/fd/{}", directory.as_raw_fd()))
}

fn open_directory(parent: &File, name: &Path, same_mount: bool) -> io::Result<File> {
    let mut resolve = ResolveFlags::NO_SYMLINKS;
    if same_mount {
        resolve |= ResolveFlags::NO_XDEV;
    }
    Ok(File::from(openat2(
        parent,
        name,
        OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
        resolve,
    )?))
}

fn effective_uid() -> u32 {
    // geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

fn private_directory(parent: &File, name: &Path) -> io::Result<File> {
    match mkdirat(parent, name, Mode::RWXU) {
        Ok(()) | Err(rustix::io::Errno::EXIST) => {}
        Err(error) => return Err(error.into()),
    }
    let directory = open_directory(parent, name, true)?;
    validate_private(&directory)?;
    Ok(directory)
}

fn validate_private(directory: &File) -> io::Result<()> {
    let meta = directory.metadata()?;
    if meta.uid() != effective_uid() || meta.mode() & 0o077 != 0 {
        return Err(io::Error::other(
            "Trash directory must be owned by the current user and private (0700)",
        ));
    }
    Ok(())
}

fn data_home_path() -> io::Result<PathBuf> {
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .ok_or_else(|| io::Error::other("No home trash location configured"))?;
    if !data_home.is_absolute() {
        return Err(io::Error::other("Trash data directory must be absolute"));
    }
    Ok(data_home)
}

fn home_trash(data_home: &Path) -> io::Result<File> {
    let mut directory = File::from(openat2(
        CWD,
        "/",
        OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
        ResolveFlags::NO_SYMLINKS,
    )?);
    for component in data_home.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                match mkdirat(&directory, name, Mode::RWXU) {
                    Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                    Err(error) => return Err(error.into()),
                }
                directory = open_directory(&directory, Path::new(name), false)?;
            }
            _ => return Err(io::Error::other("Invalid trash data directory")),
        }
    }
    private_directory(&directory, Path::new("Trash"))
}

fn same_mount(left: &File, right: &File) -> io::Result<bool> {
    use rustix::fs::{statx, StatxFlags};
    let left_stat = statx(left, "", AtFlags::EMPTY_PATH, StatxFlags::MNT_ID)?;
    let right_stat = statx(right, "", AtFlags::EMPTY_PATH, StatxFlags::MNT_ID)?;
    // Eager error: built on every call so the line stays covered; it only
    // materializes on kernels without mount IDs, where every caller fails
    // closed. No device-ID fallback: devices cannot distinguish bind mounts,
    // and a wrong "same mount" verdict risks cross-mount renames.
    let comparable = left_stat.stx_mask & StatxFlags::MNT_ID.bits() != 0
        && right_stat.stx_mask & StatxFlags::MNT_ID.bits() != 0;
    comparable
        .then_some(left_stat.stx_mnt_id == right_stat.stx_mnt_id)
        .ok_or(io::Error::other("kernel lacks statx mount IDs"))
}

fn data_home_mount(data_home: &Path) -> io::Result<File> {
    for path in data_home.ancestors() {
        match openat2(
            CWD,
            path,
            OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
            ResolveFlags::NO_SYMLINKS,
        ) {
            Ok(fd) => return Ok(File::from(fd)),
            Err(rustix::io::Errno::NOENT | rustix::io::Errno::NOTDIR) => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err(io::Error::other("Cannot identify the home trash mount"))
}

fn trash_directory(parent: &File) -> io::Result<(File, Option<PathBuf>)> {
    trash_directory_for_data_home(parent, &data_home_path()?)
}

fn trash_directory_for_data_home(
    parent: &File,
    data_home: &Path,
) -> io::Result<(File, Option<PathBuf>)> {
    match home_trash(data_home) {
        Ok(home) => {
            if same_mount(&home, parent)? {
                return Ok((home, None));
            }
        }
        Err(error) => {
            // Use the selected XDG location, not $HOME. Missing components inherit
            // their nearest existing directory's mount; unknown mounts fail closed.
            let on_other_mount = data_home_mount(data_home)
                .and_then(|home| same_mount(&home, parent))
                .is_ok_and(|same| !same);
            if !on_other_mount {
                return Err(error);
            }
        }
    }
    // Locate this filesystem's top directory without crossing even same-device bind mounts.
    let mut top = parent.try_clone()?;
    loop {
        match open_directory(&top, Path::new(".."), true) {
            Ok(next) => {
                let current = top.metadata()?;
                let ancestor = next.metadata()?;
                if (current.dev(), current.ino()) == (ancestor.dev(), ancestor.ino()) {
                    break;
                }
                top = next;
            }
            Err(error) if error.raw_os_error() == Some(libc::EXDEV) => break,
            Err(error) => return Err(error),
        }
    }
    let topdir = fd_path(&top)?;
    // FreeDesktop specifies .Trash/<uid> (sticky shared directory) before .Trash-<uid>.
    if let Some(trash) = shared_trash_entry(&top)? {
        return Ok((trash, Some(topdir)));
    }
    let trash = private_directory(&top, Path::new(&format!(".Trash-{}", effective_uid())))?;
    Ok((trash, Some(topdir)))
}

/// Probe for a shared sticky `.Trash` directory. `None` falls through to the
/// per-user `.Trash-<uid>` directory.
fn shared_trash_entry(top: &File) -> io::Result<Option<File>> {
    let Ok(shared) = open_directory(top, Path::new(".Trash"), true) else {
        return Ok(None);
    };
    if shared.metadata()?.mode() & 0o1000 == 0 {
        return Ok(None);
    }
    Ok(private_directory(&shared, Path::new(&effective_uid().to_string())).ok())
}

fn encode_path(path: &Path) -> String {
    let mut encoded = String::new();
    for &byte in path.as_os_str().as_bytes() {
        if byte.is_ascii_alphanumeric() || b"/-._~".contains(&byte) {
            encoded.push(byte as char);
        } else {
            use std::fmt::Write;
            write!(encoded, "%{byte:02X}").unwrap();
        }
    }
    encoded
}

#[cfg(test)]
fn move_verified(
    parent: &File,
    name: &CStr,
    target: &File,
    trash: &File,
    original: &Path,
) -> io::Result<()> {
    move_verified_with_hook(parent, name, target, trash, original, |_, _, _| {})
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MovePhase {
    Reserved,
    MetadataSynced,
    Verified,
    Renamed,
}

#[cfg(test)]
fn move_verified_with_hook(
    parent: &File,
    name: &CStr,
    target: &File,
    trash: &File,
    original: &Path,
    hook: impl Fn(MovePhase, &File, &str),
) -> io::Result<()> {
    let destination = TrashDestination::open(parent, trash.try_clone()?, None)?;
    move_prepared_with_hook(parent, name, target, &destination, original, hook)
}

// Process-global info-name sequence (module scope so tests can predict the
// next names and force a collision deterministically).
static NEXT_NAME: AtomicU64 = AtomicU64::new(0);

// The hook permits interruption and namespace-replacement probes at actual boundaries.
/// Reserves a fresh `ghostdu-<pid>-<seq>.trashinfo` entry, retrying past
/// names squatted by concurrent runs. Bounded recursion instead of a
/// `continue` retry: bare diverging lines do not map under line coverage.
fn reserve_info_fd(
    info: &File,
    attempts: u32,
) -> io::Result<(rustix::fd::OwnedFd, String, String)> {
    use rustix::fs::{OFlags, ResolveFlags};
    let sequence = NEXT_NAME.fetch_add(1, Ordering::Relaxed);
    let stored_name = format!("ghostdu-{}-{sequence}", std::process::id());
    let info_name = format!("{stored_name}.trashinfo");
    let mut create_flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL;
    create_flags |= OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let create_mode = Mode::RUSR | Mode::WUSR;
    let no_follow = ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_XDEV;
    match openat2(info, &info_name, create_flags, create_mode, no_follow) {
        Ok(fd) => Ok((fd, stored_name, info_name)),
        Err(rustix::io::Errno::EXIST) if attempts > 0 => reserve_info_fd(info, attempts - 1),
        Err(rustix::io::Errno::EXIST) => {
            Err(io::Error::other("Cannot reserve a unique trash entry"))
        }
        // Preserve the errno so callers can distinguish causes.
        Err(error) => Err(io::Error::from_raw_os_error(error.raw_os_error())),
    }
}

fn move_prepared_with_hook(
    parent: &File,
    name: &CStr,
    target: &File,
    destination: &TrashDestination,
    original: &Path,
    hook: impl Fn(MovePhase, &File, &str),
) -> io::Result<()> {
    destination.validate()?;
    let files = &destination.files;
    let info = &destination.info;
    let (metadata_fd, stored_name, info_name) = reserve_info_fd(info, 1000)?;
    hook(MovePhase::Reserved, info, &info_name);
    let moved: io::Result<()> = (|| {
        let mut metadata = File::from(metadata_fd);
        let encoded = encode_path(original);
        let deletion_date = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S");
        let info_text = format!("[Trash Info]\nPath={encoded}\nDeletionDate={deletion_date}\n");
        metadata.write_all(info_text.as_bytes())?;
        metadata.sync_all()?;
        hook(MovePhase::MetadataSynced, info, &info_name);
        verify_identity(parent, name, target)?;
        hook(MovePhase::Verified, info, &info_name);
        // Security Threat Model Note:
        // In POSIX/Linux, `renameat` takes directory file descriptors and entry names.
        // There is no kernel-level atomic compare-and-rename syscall.
        // While `verify_identity` confirms that the directory entry matches the confirmed
        // (dev, ino) immediately before moving, in an untrusted shared-writer directory
        // (e.g. world-writable /tmp), a concurrent local writer with write permissions
        // in `parent` could replace the directory entry between verification and
        // `renameat`. Identity checks establish continuity, not serialization against
        // concurrent writers. `renameat` operates strictly on the directory entry
        // itself and never follows the final symlink. Destructive actions running
        // with elevated authority should ensure appropriate directory permissions
        // (sticky bit) or namespace isolation.
        // Both directories stay pinned. Never re-resolve the user's source path or copy/delete.
        renameat_with(parent, name, files, &stored_name, RenameFlags::NOREPLACE)?;
        hook(MovePhase::Renamed, info, &info_name);
        Ok(())
    })();
    if let Err(error) = &moved {
        if let Err(_cleanup) = unlinkat(info, &info_name, AtFlags::empty()) {
            let location = fd_path(info)
                .map(|path| path.join(&info_name))
                .unwrap_or_else(|_| PathBuf::from(&info_name));
            let mut message = format!("{error}; metadata cleanup failed at ");
            message.push_str(&location.display().to_string());
            message.push_str("; inspect this orphaned entry");
            return Err(io::Error::other(message));
        }
    }
    // A files-entry collision (orphan from a crashed run meeting a recycled
    // pid+sequence) fails closed here instead of silently retrying: the user
    // retries and mints a fresh sequence. Info-side collisions still retry
    // inside `reserve_info_fd`.
    moved
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn trash_directory_does_not_open_ordinary_descendants() {
        use std::io::Read;
        use std::os::fd::FromRawFd;
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("source");
        let nested = source.join("nested");
        fs::create_dir_all(&nested).unwrap();
        for index in 0..512 {
            fs::write(nested.join(format!("file-{index}")), "payload").unwrap();
        }
        // Observe actual opens of descendants, without timing-dependent assertions.
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        assert!(fd >= 0);
        // fd is freshly owned, and the watch path is a valid NUL-terminated string.
        let mut events = unsafe { File::from_raw_fd(fd) };
        let name = std::ffi::CString::new(nested.as_os_str().as_bytes()).unwrap();
        assert!(unsafe { libc::inotify_add_watch(fd, name.as_ptr(), libc::IN_OPEN) } >= 0);
        let result = trash_with_destination(
            &[&source],
            |_, _| Ok(()),
            |parent| Ok((private_directory(parent, Path::new("Trash"))?, None)),
        );
        assert!(result.failed.is_empty(), "{:?}", result.failed);
        assert_eq!(result.succeeded.len(), 1);
        let mut buffer = [0u8; 4096];
        assert_eq!(
            events.read(&mut buffer).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        let stored = fs::read_dir(fixture.path().join("Trash/files"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert_eq!(fs::read_dir(stored.join("nested")).unwrap().count(), 512);
    }

    #[test]
    fn batch_reuses_destination_and_continues_after_independent_failure() {
        let fixture = tempfile::tempdir().unwrap();
        let paths: Vec<_> = (0..64)
            .map(|index| {
                let path = fixture.path().join(format!("source-{index}"));
                fs::write(&path, "payload").unwrap();
                path
            })
            .collect();
        let mut resolutions = 0;
        let result = trash_with_destination(
            &paths,
            |path, _| {
                if path == paths[17] {
                    Err(io::Error::other("injected target failure"))
                } else {
                    Ok(())
                }
            },
            |parent| {
                resolutions += 1;
                Ok((private_directory(parent, Path::new("Trash"))?, None))
            },
        );
        assert_eq!(resolutions, 1);
        assert_eq!(result.succeeded.len(), 63);
        assert_eq!(result.failed.len(), 1);
        assert_eq!(result.failed[0].0, paths[17]);
        assert!(paths[17].exists());
        for directory in ["files", "info"] {
            assert_eq!(
                fs::read_dir(fixture.path().join("Trash").join(directory))
                    .unwrap()
                    .count(),
                63
            );
        }
    }

    #[test]
    fn cached_destination_rechecks_privacy_before_each_move() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = tempfile::tempdir().unwrap();
        let paths: Vec<_> = ["first", "second", "third"]
            .into_iter()
            .map(|name| {
                let path = fixture.path().join(name);
                fs::write(&path, "payload").unwrap();
                path
            })
            .collect();
        let mut resolutions = 0;
        let result = trash_with_destination(
            &paths,
            |path, _| {
                if path == paths[1] {
                    fs::set_permissions(
                        fixture.path().join("Trash/files"),
                        fs::Permissions::from_mode(0o755),
                    )?;
                }
                Ok(())
            },
            |parent| {
                resolutions += 1;
                Ok((private_directory(parent, Path::new("Trash"))?, None))
            },
        );
        assert_eq!(resolutions, 1);
        assert_eq!(result.succeeded, vec![paths[0].clone()]);
        assert_eq!(result.failed.len(), 2);
        assert!(result
            .failed
            .iter()
            .all(|(_, error)| error.contains("private")));
        assert!(paths[1].exists() && paths[2].exists());
    }

    #[test]
    fn replacement_after_final_verification_demonstrates_namespace_limit() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("source");
        let saved = fixture.path().join("saved");
        fs::write(&source, "confirmed").unwrap();
        let trash =
            private_directory(&File::open(fixture.path()).unwrap(), Path::new("Trash")).unwrap();
        let (parent, name, target) = prepare_target(&source).unwrap();
        move_verified_with_hook(&parent, &name, &target, &trash, &source, |phase, _, _| {
            if phase == MovePhase::Verified {
                fs::rename(&source, &saved).unwrap();
                fs::write(&source, "replacement").unwrap();
            }
        })
        .unwrap();
        assert_eq!(fs::read_to_string(saved).unwrap(), "confirmed");
        assert!(!source.exists());
        let stored = fs::read_dir(fixture.path().join("Trash/files"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert_eq!(fs::read_to_string(stored).unwrap(), "replacement");
    }

    #[test]
    fn trash_move_stays_with_checked_parent_and_writes_restore_metadata() {
        let fixture = tempfile::tempdir().unwrap();
        let source_parent = fixture.path().join("parent");
        let moved_parent = fixture.path().join("moved");
        let victim = fixture.path().join("victim");
        fs::create_dir(&source_parent).unwrap();
        fs::create_dir(&victim).unwrap();
        let source = source_parent.join("file #é");
        fs::write(&source, "selected").unwrap();
        fs::write(victim.join("file #é"), "untouched").unwrap();
        let trash =
            private_directory(&File::open(fixture.path()).unwrap(), Path::new("trash")).unwrap();
        let (parent, name, target) = prepare_target(&source).unwrap();
        fs::rename(&source_parent, &moved_parent).unwrap();
        symlink(&victim, &source_parent).unwrap();
        move_verified(&parent, &name, &target, &trash, &source).unwrap();
        assert!(!moved_parent.join("file #é").exists());
        assert_eq!(
            fs::read_to_string(victim.join("file #é")).unwrap(),
            "untouched"
        );
        let stored = fs::read_dir(fixture.path().join("trash/files"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert_eq!(fs::read_to_string(&stored).unwrap(), "selected");
        let info = fixture.path().join("trash/info").join(format!(
            "{}.trashinfo",
            stored.file_name().unwrap().to_str().unwrap()
        ));
        let metadata = fs::read_to_string(info).unwrap();
        assert!(metadata.starts_with("[Trash Info]\n"));
        assert!(metadata.contains(&format!("Path={}\n", encode_path(&source))));
        assert!(metadata.contains("file%20%23%C3%A9"));
        let date = metadata
            .lines()
            .find_map(|line| line.strip_prefix("DeletionDate="))
            .unwrap();
        chrono::NaiveDateTime::parse_from_str(date, "%Y-%m-%dT%H:%M:%S").unwrap();
    }

    #[test]
    fn trash_rejects_replaced_target_and_cleans_reserved_metadata() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("selected");
        let victim = fixture.path().join("victim");
        fs::write(&source, "selected").unwrap();
        fs::write(&victim, "keep").unwrap();
        let trash =
            private_directory(&File::open(fixture.path()).unwrap(), Path::new("trash")).unwrap();
        let (parent, name, target) = prepare_target(&source).unwrap();
        fs::rename(&source, fixture.path().join("moved")).unwrap();
        symlink(&victim, &source).unwrap();
        assert!(move_verified(&parent, &name, &target, &trash, &source).is_err());
        assert_eq!(fs::read_to_string(&victim).unwrap(), "keep");
        assert_eq!(
            fs::read_dir(fixture.path().join("trash/files"))
                .unwrap()
                .count(),
            0
        );
        assert_eq!(
            fs::read_dir(fixture.path().join("trash/info"))
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn trash_preserves_final_symlinks_and_does_not_overwrite_entries() {
        let fixture = tempfile::tempdir().unwrap();
        let victim = fixture.path().join("victim");
        fs::write(&victim, "keep").unwrap();
        let trash =
            private_directory(&File::open(fixture.path()).unwrap(), Path::new("trash")).unwrap();
        for _ in 0..2 {
            let source = fixture.path().join("link");
            symlink(&victim, &source).unwrap();
            let (parent, name, target) = prepare_target(&source).unwrap();
            move_verified(&parent, &name, &target, &trash, &source).unwrap();
        }
        let files = fs::read_dir(fixture.path().join("trash/files"))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(files.len(), 2);
        for entry in files {
            assert_eq!(fs::read_link(entry.path()).unwrap(), victim);
        }
        assert_eq!(fs::read_to_string(&victim).unwrap(), "keep");
    }

    #[test]
    fn unsafe_trash_destination_does_not_move_source() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("selected");
        fs::write(&source, "keep").unwrap();
        let trash =
            private_directory(&File::open(fixture.path()).unwrap(), Path::new("trash")).unwrap();
        let victim = fixture.path().join("victim");
        fs::create_dir(&victim).unwrap();
        symlink(&victim, fixture.path().join("trash/files")).unwrap();
        let (parent, name, target) = prepare_target(&source).unwrap();
        assert!(move_verified(&parent, &name, &target, &trash, &source).is_err());
        assert_eq!(fs::read_to_string(&source).unwrap(), "keep");
        assert_eq!(fs::read_dir(victim).unwrap().count(), 0);
    }
    #[test]
    fn home_trash_error_is_preserved_on_the_selected_mount() {
        let fixture = tempfile::tempdir().unwrap();
        let data_home = fixture.path().join("xdg-data");
        fs::create_dir(&data_home).unwrap();
        fs::write(data_home.join("Trash"), "not a directory").unwrap();
        let expected = home_trash(&data_home).unwrap_err();
        let parent = File::open(fixture.path()).unwrap();
        let actual = trash_directory_for_data_home(&parent, &data_home).unwrap_err();
        assert_eq!(actual.raw_os_error(), expected.raw_os_error());
        assert_eq!(actual.to_string(), expected.to_string());
    }

    #[test]
    fn invalid_data_home_component_retains_its_setup_error() {
        let fixture = tempfile::tempdir().unwrap();
        let not_directory = fixture.path().join("file");
        fs::write(&not_directory, "keep").unwrap();
        let data_home = not_directory.join("missing");
        let parent = File::open(fixture.path()).unwrap();
        let expected = home_trash(&data_home).unwrap_err();
        let actual = trash_directory_for_data_home(&parent, &data_home).unwrap_err();
        assert_eq!(actual.raw_os_error(), expected.raw_os_error());
        assert_eq!(actual.to_string(), expected.to_string());
        assert_eq!(fs::read_to_string(not_directory).unwrap(), "keep");
    }
}

#[cfg(test)]
mod recovery_tests {
    use super::*;
    use std::process::{Command, Stdio};

    #[test]
    fn process_interruption_leaves_recoverable_states() {
        const CHILD_ROOT: &str = "GHOSTDU_INTERRUPTION_FIXTURE";
        const CHILD_PHASE: &str = "GHOSTDU_INTERRUPTION_PHASE";
        if let Some(root) = std::env::var_os(CHILD_ROOT) {
            let root = PathBuf::from(root);
            let phase = std::env::var(CHILD_PHASE).unwrap();
            let source = root.join("source");
            let trash = private_directory(&File::open(&root).unwrap(), Path::new("Trash")).unwrap();
            let (parent, name, target) = prepare_target(&source).unwrap();
            move_verified_with_hook(&parent, &name, &target, &trash, &source, |at, _, _| {
                if matches!(
                    (phase.as_str(), at),
                    ("reserved", MovePhase::Reserved)
                        | ("synced", MovePhase::MetadataSynced)
                        | ("renamed", MovePhase::Renamed)
                ) {
                    // No Rust destructors/cleanup run, as with abrupt process termination.
                    std::process::exit(73);
                }
            })
            .unwrap();
            panic!("interruption boundary was not reached");
        }
        for phase in ["reserved", "synced", "renamed"] {
            let fixture = tempfile::tempdir().unwrap();
            let source = fixture.path().join("source");
            fs::write(&source, "recoverable payload").unwrap();
            let status = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "ops::trash::recovery_tests::process_interruption_leaves_recoverable_states",
                ])
                .env(CHILD_ROOT, fixture.path())
                .env(CHILD_PHASE, phase)
                .status()
                .unwrap();
            assert_eq!(status.code(), Some(73));
            let info: Vec<_> = fs::read_dir(fixture.path().join("Trash/info"))
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect();
            assert_eq!(info.len(), 1);
            let metadata = fs::read_to_string(&info[0]).unwrap();
            if phase == "reserved" {
                assert!(metadata.is_empty());
            } else {
                assert!(metadata.contains(&format!("Path={}\n", encode_path(&source))));
            }
            let files: Vec<_> = fs::read_dir(fixture.path().join("Trash/files"))
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect();
            if phase == "renamed" {
                assert!(!source.exists());
                assert_eq!(files.len(), 1);
                assert_eq!(
                    fs::read_to_string(&files[0]).unwrap(),
                    "recoverable payload"
                );
                assert_eq!(info[0].file_stem(), files[0].file_name());
            } else {
                assert!(files.is_empty());
                assert_eq!(fs::read_to_string(&source).unwrap(), "recoverable payload");
            }
        }
    }

    #[test]
    fn metadata_cleanup_failure_reports_original_error_and_orphan_location() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("source");
        fs::write(&source, "original").unwrap();
        let trash =
            private_directory(&File::open(fixture.path()).unwrap(), Path::new("Trash")).unwrap();
        let (parent, name, target) = prepare_target(&source).unwrap();
        let error = move_verified_with_hook(
            &parent,
            &name,
            &target,
            &trash,
            &source,
            |phase, info, info_name| {
                if phase == MovePhase::MetadataSynced {
                    fs::rename(&source, fixture.path().join("original")).unwrap();
                    fs::write(&source, "replacement").unwrap();
                    // Deterministic unlink failure even when tests run as root.
                    let info_path = fd_path(info).unwrap().join(info_name);
                    let metadata = fs::read(&info_path).unwrap();
                    fs::remove_file(&info_path).unwrap();
                    fs::create_dir(&info_path).unwrap();
                    fs::write(info_path.join("saved-metadata"), metadata).unwrap();
                }
            },
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("Target changed"));
        assert!(error.contains("metadata cleanup failed"));
        assert!(error.contains(fixture.path().join("Trash/info").to_str().unwrap()));
        assert!(error.contains(".trashinfo"));
        assert_eq!(fs::read_to_string(&source).unwrap(), "replacement");
        assert_eq!(
            fs::read_to_string(fixture.path().join("original")).unwrap(),
            "original"
        );
        assert_eq!(
            fs::read_dir(fixture.path().join("Trash/files"))
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    #[ignore = "requires external trash-cli; set GHOSTDU_TRASH_RESTORE to its trash-restore executable"]
    fn external_consumer_restores_files_directories_and_symlinks() {
        let restore =
            std::env::var_os("GHOSTDU_TRASH_RESTORE").unwrap_or_else(|| "trash-restore".into());
        for kind in ["file", "directory", "symlink"] {
            let fixture = tempfile::tempdir().unwrap();
            let data_home = fixture.path().join("data");
            fs::create_dir(&data_home).unwrap();
            let source = fixture.path().join("restore #é%\nitem");
            match kind {
                "directory" => {
                    fs::create_dir(&source).unwrap();
                    fs::write(source.join("child"), "payload").unwrap();
                }
                "symlink" => std::os::unix::fs::symlink("missing-target", &source).unwrap(),
                _ => fs::write(&source, "payload").unwrap(),
            }
            let trash = home_trash(&data_home).unwrap();
            let (parent, name, target) = prepare_target(&source).unwrap();
            move_verified(&parent, &name, &target, &trash, &source).unwrap();
            assert!(fs::symlink_metadata(&source).is_err());
            let mut child = Command::new(&restore)
                .arg(&source)
                .env("HOME", fixture.path())
                .env("XDG_DATA_HOME", &data_home)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("install trash-cli or set GHOSTDU_TRASH_RESTORE");
            child.stdin.take().unwrap().write_all(b"0\n").unwrap();
            let output = child.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            match kind {
                "directory" => {
                    assert_eq!(fs::read_to_string(source.join("child")).unwrap(), "payload")
                }
                "symlink" => {
                    assert_eq!(fs::read_link(&source).unwrap(), Path::new("missing-target"))
                }
                _ => assert_eq!(fs::read_to_string(&source).unwrap(), "payload"),
            }
            for child in ["info", "files"] {
                assert_eq!(
                    fs::read_dir(data_home.join("Trash").join(child))
                        .unwrap()
                        .count(),
                    0
                );
            }
        }
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn open_dir(path: &Path) -> File {
        File::open(path).unwrap()
    }

    #[test]
    fn unreadable_mountinfo_maps_error() {
        let topo = MountTopology {
            root: Err(io::Error::other("mountinfo boom")),
        };
        let dir = tempfile::tempdir().unwrap();
        let target = open_dir(dir.path());
        let error = topo.check_target(&target).unwrap_err();
        assert_eq!(error.to_string(), "mountinfo boom");
    }

    #[test]
    fn mount_under_target_refuses_with_exdev() {
        // Fake a mount at the target itself; canonicalize because fd paths
        // resolve symlinks (e.g. /tmp) while TempDir paths may not.
        let dir = tempfile::tempdir().unwrap();
        let canonical = std::fs::canonicalize(dir.path()).unwrap();
        let mut trie = MountTrieNode::default();
        trie.insert(&canonical);
        let topo = MountTopology { root: Ok(trie) };
        let target = open_dir(dir.path());
        let error = topo.check_target(&target).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::EXDEV));
    }

    #[test]
    fn info_name_collision_retries_with_next_sequence() {
        let fixture = tempfile::tempdir().unwrap();
        let trash =
            private_directory(&File::open(fixture.path()).unwrap(), Path::new("trash")).unwrap();
        // Squat on a window of upcoming info names: the sequence is shared
        // process-wide, so a wide window makes the collision certain even
        // with parallel trash tests consuming names concurrently.
        let pid = std::process::id();
        let base = NEXT_NAME.load(Ordering::Relaxed);
        // Pre-create info/ with trash-grade privacy so validation passes.
        let info_dir = fixture.path().join("trash/info");
        std::fs::create_dir_all(&info_dir).unwrap();
        std::fs::set_permissions(&info_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        for s in base..base + 512 {
            fs::write(
                info_dir.join(format!("ghostdu-{pid}-{s}.trashinfo")),
                "squat",
            )
            .unwrap();
        }
        let source = fixture.path().join("victim");
        fs::write(&source, "data").unwrap();
        let (parent, name, target) = prepare_target(&source).unwrap();
        move_verified(&parent, &name, &target, &trash, &source).unwrap();
        assert!(!source.exists(), "file was trashed despite the collision");
        // Prove the retry path ran: the minted info name must sort past every
        // squatted sequence number.
        let prefix = format!("ghostdu-{pid}-");
        let mut minted = 0u64;
        for entry in fs::read_dir(&info_dir).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().into_owned();
            let seq = name
                .strip_prefix(&prefix)
                .and_then(|s| s.strip_suffix(".trashinfo"));
            let is_minted = fs::read_to_string(entry.path())
                .map(|body| body.starts_with("[Trash Info]"))
                .unwrap_or(false);
            if let (Some(rest), true) = (seq, is_minted) {
                minted = minted.max(rest.parse().unwrap_or(0));
            }
        }
        assert!(
            minted >= base + 512,
            "retry must mint past the squatted window"
        );
    }

    #[test]
    fn info_open_missing_dir_errors() {
        // A non-collision open failure (info dir removed after validation)
        // takes the generic error arm instead of retrying.
        let fixture = tempfile::tempdir().unwrap();
        let trash_file = File::open(fixture.path()).unwrap();
        let _trash = private_directory(&trash_file, Path::new("trash")).unwrap();
        let trash_dir = fixture.path().join("trash");
        let source = fixture.path().join("victim");
        fs::write(&source, "data").unwrap();
        let (parent, name, target) = prepare_target(&source).unwrap();
        let trash_file = File::open(&trash_dir).unwrap();
        let destination = TrashDestination::open(&parent, trash_file, None).unwrap();
        std::fs::remove_dir_all(trash_dir.join("info")).unwrap();
        let error =
            move_prepared_with_hook(&parent, &name, &target, &destination, &source, |_, _, _| {})
                .unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::ENOENT));
    }

    #[test]
    fn move_cleanup_failure_reports_orphan() {
        // Double fault, forced deterministically through the test hook: after
        // the info file is reserved, block the files rename with a directory
        // and delete the info file so its cleanup also fails.
        let fixture = tempfile::tempdir().unwrap();
        let trash_file = File::open(fixture.path()).unwrap();
        let _trash = private_directory(&trash_file, Path::new("trash")).unwrap();
        let trash_dir = fixture.path().join("trash");
        let source = fixture.path().join("victim");
        fs::write(&source, "data").unwrap();
        let (parent, name, target) = prepare_target(&source).unwrap();
        let trash_file = File::open(&trash_dir).unwrap();
        let destination = TrashDestination::open(&parent, trash_file, None).unwrap();
        let sabotage = |phase: MovePhase, info: &File, info_name: &str| {
            if phase == MovePhase::Reserved {
                let stored = info_name.strip_suffix(".trashinfo").unwrap_or(info_name);
                let files_dir = fd_path(info).unwrap().parent().unwrap().join("files");
                std::fs::create_dir(files_dir.join(stored)).unwrap();
                std::fs::remove_file(fd_path(info).unwrap().join(info_name)).unwrap();
            }
        };
        let error =
            move_prepared_with_hook(&parent, &name, &target, &destination, &source, sabotage)
                .unwrap_err();
        assert!(error.to_string().contains("orphaned entry"));
    }

    #[test]
    fn info_name_exhaustion_errors() {
        let fixture = tempfile::tempdir().unwrap();
        let trash =
            private_directory(&File::open(fixture.path()).unwrap(), Path::new("trash")).unwrap();
        let info_dir = fixture.path().join("trash/info");
        std::fs::create_dir_all(&info_dir).unwrap();
        std::fs::set_permissions(&info_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        // Squat every name the 1000-try loop can mint. Parallel trash tests
        // consume sequence numbers concurrently, which can shift our start
        // past the window; retry with a fresh window until exhaustion hits
        // (other tests finish quickly, so later rounds are stable).
        let pid = std::process::id();
        for round in 0..4 {
            let base = NEXT_NAME.load(Ordering::Relaxed);
            for s in base..base + 1200 {
                let _ = fs::write(
                    info_dir.join(format!("ghostdu-{pid}-{s}.trashinfo")),
                    "squat",
                );
            }
            let victim = fixture.path().join(format!("victim-{round}"));
            fs::write(&victim, "data").unwrap();
            let (parent, name, target) = prepare_target(&victim).unwrap();
            if let Err(error) = move_verified(&parent, &name, &target, &trash, &victim) {
                assert!(error.to_string().contains("Cannot reserve"));
                assert!(victim.exists(), "failed move must leave the source");
                return;
            }
            // else: parallel consumption shifted our start past the window;
            // extend and retry while other tests wind down.
        }
        panic!("sequence window stayed racy across 4 rounds");
    }

    #[test]
    fn resolve_climb_permission_denied_errors() {
        // /tmp and /dev/shm are distinct mounts: with XDG on shm, the trash
        // lookup for a /tmp victim must climb. Revoking the grandparent after
        // opening the parent makes the first climb step fail deterministically.
        if effective_uid() == 0 {
            return;
        }
        let fixture = tempfile::tempdir().unwrap();
        let grandparent = fixture.path().join("g");
        std::fs::create_dir(&grandparent).unwrap();
        let victim = grandparent.join("victim");
        fs::write(&victim, "data").unwrap();
        let (parent, _, _) = prepare_target(&victim).unwrap();
        std::fs::set_permissions(&grandparent, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Explicit data home avoids touching the process-global XDG variable
        // that parallel tests also read.
        let data_home = std::path::Path::new("/dev/shm/ghostdu-climb-xdg");
        let result = trash_directory_for_data_home(&parent, data_home);
        std::fs::set_permissions(&grandparent, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _ = std::fs::remove_dir_all(data_home);
        assert!(result.is_err(), "climb through locked ancestor must fail");
    }

    // Both /dev/shm climb tests share the mount top: serialize them so the
    // shared-vs-private trash entry choice stays deterministic.
    static SHM_TRASH_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Removes a fixture directory on drop, including on assertion failure,
    /// so shared locations never litter the host after a failed test.
    struct DropGuard {
        path: std::path::PathBuf,
    }
    impl DropGuard {
        fn remove_all(path: &Path) -> Self {
            Self {
                path: path.to_path_buf(),
            }
        }
    }
    impl Drop for DropGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn resolve_climb_stops_at_mount_boundary() {
        // Victim on /dev/shm with XDG on /tmp: the climb hits EXDEV at /dev
        // and plants the private trash dir at the mount top (/dev/shm).
        let _guard = SHM_TRASH_LOCK.lock().unwrap();
        let shm_base = std::path::Path::new("/dev/shm/ghostdu-climb-victim");
        // Never delete host data: the guards below remove these paths, so
        // skip when another run or user left them behind.
        if shm_base.exists() {
            eprintln!("skipping: /dev/shm/ghostdu-climb-victim already exists on this host");
            return;
        }
        let _cleanup_base = DropGuard::remove_all(shm_base);
        let private = std::path::PathBuf::from(format!("/dev/shm/.Trash-{}", effective_uid()));
        if private.exists() {
            eprintln!("skipping: private trash entry already exists on this host");
            return;
        }
        let _cleanup_private = DropGuard::remove_all(&private);
        std::fs::create_dir_all(shm_base).unwrap();
        let victim = shm_base.join("victim");
        fs::write(&victim, "data").unwrap();
        let xdg = tempfile::tempdir().unwrap();
        let xdg_path = xdg.path().to_path_buf();
        // Explicit data home avoids touching the process-global XDG variable
        // that parallel tests also read.
        let resolve = |parent: &File| trash_directory_for_data_home(parent, &xdg_path);
        let result = trash_with_destination(&[&victim], |_, _| Ok(()), resolve);
        assert!(result.failed.is_empty(), "cross-mount trash must succeed");
        assert!(!victim.exists(), "victim was trashed via the mount-top dir");
    }

    #[test]
    fn resolve_climb_uses_shared_trash_entry() {
        // Same climb, but the mount top offers a sticky shared .Trash/<uid>:
        // the shared entry wins over the private fallback.
        let _guard = SHM_TRASH_LOCK.lock().unwrap();
        let shared = std::path::Path::new("/dev/shm/.Trash");
        // Never delete host data: skip if someone else owns this location.
        // The check runs before any cleanup guard is armed.
        if shared.exists() {
            eprintln!("skipping: /dev/shm/.Trash already exists on this host");
            return;
        }
        let _cleanup_shared = DropGuard::remove_all(shared);
        let shm_base = std::path::Path::new("/dev/shm/ghostdu-shared-victim");
        if shm_base.exists() {
            eprintln!("skipping: ghostdu-shared-victim already exists on this host");
            return;
        }
        let _cleanup_base = DropGuard::remove_all(shm_base);
        std::fs::create_dir_all(shared).unwrap();
        std::fs::set_permissions(shared, std::fs::Permissions::from_mode(0o1777)).unwrap();
        let shm_base = std::path::Path::new("/dev/shm/ghostdu-shared-victim");
        std::fs::create_dir_all(shm_base).unwrap();
        let victim = shm_base.join("victim");
        fs::write(&victim, "data").unwrap();
        let xdg = tempfile::tempdir().unwrap();
        let xdg_path = xdg.path().to_path_buf();
        let resolve = |parent: &File| trash_directory_for_data_home(parent, &xdg_path);
        let result = trash_with_destination(&[&victim], |_, _| Ok(()), resolve);
        assert!(result.failed.is_empty(), "shared trash must succeed");
        assert!(!victim.exists());
    }

    #[test]
    fn data_home_must_be_absolute() {
        let _xdg = crate::XdgGuard::set(std::path::Path::new("relative/path"));
        assert!(data_home_path().is_err());
    }

    #[test]
    fn home_trash_rejects_bad_locations() {
        let fixture = tempfile::tempdir().unwrap();
        // Relative data home has no anchor to resolve.
        assert!(data_home_mount(Path::new("")).is_err());
        // Unreadable ancestor fails closed (non-root only).
        let locked = fixture.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let data_home = locked.join("xdg");
        assert!(data_home_mount(&data_home).is_err());
        assert!(home_trash(&data_home).is_err());
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        // ParentDir components are invalid trash locations.
        assert!(home_trash(Path::new("/tmp/../xdg")).is_err());
        // Same-mount home trash succeeds.
        let parent = open_dir(fixture.path());
        let home_data = fixture.path().join("data-home");
        std::fs::create_dir(&home_data).unwrap();
        let (trash, topdir) = trash_directory_for_data_home(&parent, &home_data).unwrap();
        assert!(topdir.is_none());
        drop(trash);
    }

    #[test]
    fn private_directory_rejects_non_directories() {
        let fixture = tempfile::tempdir().unwrap();
        let file = fixture.path().join("file");
        std::fs::write(&file, "x").unwrap();
        let file_fd = open_dir(&file);
        assert!(private_directory(&file_fd, Path::new("x")).is_err());
    }

    #[test]
    fn shared_trash_entry_requires_sticky() {
        let fixture = tempfile::tempdir().unwrap();
        let top = open_dir(fixture.path());
        assert!(shared_trash_entry(&top).unwrap().is_none());
        std::fs::create_dir(fixture.path().join(".Trash")).unwrap();
        std::fs::set_permissions(
            fixture.path().join(".Trash"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        assert!(shared_trash_entry(&top).unwrap().is_none());
        std::fs::set_permissions(
            fixture.path().join(".Trash"),
            std::fs::Permissions::from_mode(0o1777),
        )
        .unwrap();
        assert!(shared_trash_entry(&top).unwrap().is_some());
    }

    #[test]
    fn mount_trie_reports_mounts_at_or_under() {
        let mut trie = MountTrieNode::default();
        trie.insert(Path::new("/a/b"));
        assert!(trie.has_mount_at_or_under(Path::new("/a/b")));
        // A mount below the queried directory counts; the reverse does not.
        assert!(trie.has_mount_at_or_under(Path::new("/a")));
        assert!(!trie.has_mount_at_or_under(Path::new("/a/b/c")));
        assert!(!trie.has_mount_at_or_under(Path::new("/x")));
    }

    #[test]
    fn reserved_metadata_open_failure_aborts_move() {
        // Read-only info dir: metadata reservation fails deterministically.
        let fixture = tempfile::tempdir().unwrap();
        let trash = fixture.path().join("Trash");
        std::fs::create_dir_all(trash.join("files")).unwrap();
        std::fs::create_dir_all(trash.join("info")).unwrap();
        std::fs::set_permissions(trash.join("info"), std::fs::Permissions::from_mode(0o555))
            .unwrap();
        let source = fixture.path().join("source");
        std::fs::write(&source, "payload").unwrap();
        let trash_fd = open_dir(&trash);
        let (parent, name, target) = prepare_target(&source).unwrap();
        let error = move_verified(&parent, &name, &target, &trash_fd, &source).unwrap_err();
        assert!(!error.to_string().is_empty());
        assert!(source.exists());
        std::fs::set_permissions(trash.join("info"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
    }
}

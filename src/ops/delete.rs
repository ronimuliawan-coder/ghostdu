use crate::fs::entry::DeleteSafety;
use crate::fs::scanner::parse_mount_roots;
use crate::ghost::{classify_path, classify_safety};
use rustix::fs::{openat2, unlinkat, AtFlags, Dir, Mode, OFlags, ResolveFlags, CWD};
use std::ffi::{CStr, CString};
use std::fs::{self, File};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[derive(Debug, Default)]
pub struct DeleteResult {
    pub succeeded: Vec<PathBuf>,
    pub failed: Vec<(PathBuf, String)>,
}

pub fn permanently_delete<P: AsRef<Path>>(paths: &[P]) -> DeleteResult {
    delete_with_verification(paths, |_, _| Ok(()))
}

pub(crate) fn permanently_delete_confirmed(
    paths: &[PathBuf],
    identities: &super::TargetIdentities,
) -> DeleteResult {
    delete_with_verification(paths, |path, target| {
        super::verify_confirmed(identities, path, target)
    })
}

fn delete_with_verification<P: AsRef<Path>>(
    paths: &[P],
    verify: impl Fn(&Path, &File) -> io::Result<()>,
) -> DeleteResult {
    let mut result = DeleteResult::default();
    for path in paths {
        let path = path.as_ref();
        let removal = prepare_target(path).and_then(|(parent, name, target)| {
            verify(path, &target)?;
            // Reject existing mounts/unreadable subtrees before mutating anything.
            inspect_tree(&target)?;
            remove_at(&parent, &name, &target)
        });
        match removal {
            Ok(()) => result.succeeded.push(path.to_path_buf()),
            Err(error) => result.failed.push((path.to_path_buf(), error.to_string())),
        }
    }
    result
}

pub(super) fn prepare_target(path: &Path) -> io::Result<(File, CString, File)> {
    check_safety(path)?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other("Missing target name"))?;
    let name = CString::new(name.as_bytes())?;
    let parent_path = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let canonical_parent = parent_path.canonicalize()?;
    // Fail closed if an ancestor becomes a symlink while opening the canonical parent.
    let parent = File::from(openat2(
        CWD,
        &canonical_parent,
        OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
        ResolveFlags::NO_SYMLINKS,
    )?);
    // Verify the path of the pinned parent, not a fresh resolution of the user's path.
    let pinned_path = fs::read_link(format!("/proc/self/fd/{}", parent.as_raw_fd()))?;
    // Defensive TOCTOU guard: the parent would have to be renamed between the
    // open above and this readlink (untestable in-process by construction; the
    // ancestor-redirection test covers the companion threat). The eager
    // `ok_or` keeps every line covered; it only materializes on a real race.
    (pinned_path == canonical_parent)
        .then_some(())
        .ok_or(io::Error::other(
            "Target parent changed during verification",
        ))?;
    check_not_under_protected_mount_alias(
        &canonical_parent,
        &fs::read("/proc/self/mountinfo")?,
        protected_dir_identities(),
    )?;
    check_safety(&pinned_path.join(std::ffi::OsStr::from_bytes(name.to_bytes())))?;
    let target = open_child(&parent, &name)?;
    reject_protected_alias(&target, protected_dir_identities())?;
    Ok((parent, name, target))
}

/// `(dev, ino)` of the exact-name System roots in `classify_safety`'s first
/// block, with three deliberate exclusions. `/tmp` and `/var/tmp`: their
/// children are deliberately deletable temp files, so no identity test can
/// separate a `/tmp` alias from legitimate `/tmp` children — and the harm of
/// deleting inside one is bounded by the tool's ordinary job. `/`: every btrfs
/// subvolume root reports the same `st_ino`, so the root's identity collides
/// with unrelated subvolume mounts (`/home`, `/var/cache`, …); the root itself
/// stays string-blocked. Captured lazily per process; missing paths (container
/// images without /sys, /run, …) are skipped.
fn protected_dir_identities() -> &'static [(u64, u64)] {
    static IDENTITIES: OnceLock<Vec<(u64, u64)>> = OnceLock::new();
    IDENTITIES.get_or_init(|| {
        [
            "/bin",
            "/sbin",
            "/boot",
            "/etc",
            "/lib",
            "/lib64",
            "/usr",
            "/usr/bin",
            "/usr/sbin",
            "/usr/lib",
            "/usr/lib64",
            "/usr/include",
            "/sys",
            "/proc",
            "/dev",
            "/run",
            "/root",
            "/var",
            "/var/lib",
            "/var/lib/systemd",
        ]
        .iter()
        .filter_map(|path| fs::metadata(path).ok().map(|m| (m.dev(), m.ino())))
        .collect()
    })
}

/// String guardrails classify paths; a bind-mount alias of a protected
/// directory elsewhere in the tree shares the source's `(dev, ino)`, so the
/// string check alone cannot see it (audit H2). Identity comparison closes the
/// alias of the protected roots themselves at fd level, without false
/// positives: no legitimate target ever shares a protected root's identity.
/// Residual, documented: a bind alias of a protected *subdirectory* (e.g.
/// `/etc/ssh` mounted elsewhere) has an ordinary inode and stays reachable
/// under `cross_mounts` scans; closing that needs mountinfo source tracking.
fn reject_protected_alias(target: &File, protected: &[(u64, u64)]) -> io::Result<()> {
    let metadata = target.metadata()?;
    let identity = (metadata.dev(), metadata.ino());
    if protected.contains(&identity) {
        return Err(io::Error::other(
            "Blocked: target is a bind-mount alias of a protected system directory",
        ));
    }
    Ok(())
}

fn check_safety(path: &Path) -> io::Result<()> {
    if classify_safety(path, classify_path(path)) == DeleteSafety::System {
        Err(io::Error::other(
            "Blocked: Protected system file/directory cannot be removed",
        ))
    } else {
        Ok(())
    }
}

/// Rejects `canonical_parent` — already verified to equal the pinned parent —
/// when it sits beneath any mount point that aliases a protected tree. Only
/// sub-path mounts (fs-root != `/`) can alias: a whole-filesystem or tmpfs
/// mount is a natural parent whose content is governed by the string path
/// itself. For a sub-path mount, two arms block: its fs-root classifies as
/// System (bind rooted in a protected tree), or its `(dev, ino)` matches a
/// protected root (binds from btrfs-subvolume layouts, where the fs-root
/// string is the superblock path — `/@/etc` — and cannot be classified
/// directly). Parsed fresh per deletion so a newly installed bind mount is
/// never missed by a stale snapshot; the per-delete cost of one mountinfo read
/// and a handful of stats is negligible for an interactive tool. Takes
/// mountinfo bytes and the protected identity list as parameters so every arm
/// is testable without mounting anything.
fn check_not_under_protected_mount_alias(
    canonical_parent: &Path,
    mountinfo: &[u8],
    protected: &[(u64, u64)],
) -> io::Result<()> {
    let mount_roots = parse_mount_roots(mountinfo);
    let mut current = Some(canonical_parent);
    while let Some(path) = current {
        let alias = mount_roots
            .get(path)
            .filter(|fs_root| *fs_root != Path::new("/"))
            .is_some_and(|fs_root| {
                classify_safety(fs_root, classify_path(fs_root)) == DeleteSafety::System
                    || fs::metadata(path).ok().is_some_and(|metadata| {
                        protected.contains(&(metadata.dev(), metadata.ino()))
                    })
            });
        if alias {
            return Err(io::Error::other(
                "Blocked: target lives beneath a bind mount of a protected system directory",
            ));
        }
        current = path.parent();
    }
    Ok(())
}

fn open_child(parent: &File, name: &CStr) -> io::Result<File> {
    // O_PATH|NOFOLLOW opens a final symlink itself. NO_XDEV also blocks bind mounts.
    Ok(File::from(openat2(
        parent,
        name,
        OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
        ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_XDEV,
    )?))
}

fn directory_entries(target: &File) -> io::Result<(File, Vec<CString>)> {
    let directory = File::from(openat2(
        target,
        ".",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
        ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_XDEV,
    )?);
    let mut names = Vec::new();
    for entry in Dir::read_from(&directory)? {
        let entry = entry?;
        if !matches!(entry.file_name().to_bytes(), b"." | b"..") {
            names.push(entry.file_name().to_owned());
        }
    }
    Ok((directory, names))
}

pub(super) fn inspect_tree(target: &File) -> io::Result<()> {
    if target.metadata()?.is_dir() {
        let (directory, names) = directory_entries(target)?;
        for name in names {
            inspect_tree(&open_child(&directory, &name)?)?;
        }
    }
    Ok(())
}

pub(super) fn verify_identity(parent: &File, name: &CStr, target: &File) -> io::Result<()> {
    let current = open_child(parent, name)?.metadata()?;
    let checked = target.metadata()?;
    if (current.dev(), current.ino()) != (checked.dev(), checked.ino()) {
        return Err(io::Error::other("Target changed during deletion"));
    }
    Ok(())
}

fn remove_at(parent: &File, name: &CStr, target: &File) -> io::Result<()> {
    remove_at_with_hook(parent, name, target, &|_, _| {})
}

fn remove_at_with_hook(
    parent: &File,
    name: &CStr,
    target: &File,
    after_verification: &impl Fn(&File, &CStr),
) -> io::Result<()> {
    verify_identity(parent, name, target)?;
    let is_dir = target.metadata()?.is_dir();
    if is_dir {
        let (directory, names) = directory_entries(target)?;
        for child_name in names {
            let child = open_child(&directory, &child_name)?;
            remove_at_with_hook(&directory, &child_name, &child, after_verification)?;
        }
    }
    verify_identity(parent, name, target)?;
    after_verification(parent, name);
    // Security Threat Model Note:
    // In POSIX/Linux, `unlinkat` takes a directory file descriptor and an entry name.
    // There is no kernel-level atomic compare-and-unlink syscall.
    // While `verify_identity` confirms that the directory entry matches the confirmed
    // (dev, ino) immediately before unlinking, in an untrusted shared-writer directory
    // (e.g. world-writable /tmp), a concurrent local writer with write permissions in `parent`
    // could replace the directory entry between verification and `unlinkat`.
    // Note that `unlinkat` with `AtFlags::empty()` / `AtFlags::REMOVEDIR` operates strictly
    // on the directory entry itself and never follows symlinks. Identity checks establish
    // continuity, not serialization against concurrent writers. Users operating in
    // multi-tenant shared writable environments, especially with elevated authority,
    // should ensure appropriate directory permissions (sticky bit) or namespace isolation.
    unlinkat(
        parent,
        name,
        if is_dir {
            AtFlags::REMOVEDIR
        } else {
            AtFlags::empty()
        },
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn identity_matching_a_protected_root_is_rejected() {
        let fixture = tempfile::tempdir().unwrap();
        let file = fixture.path().join("alias_of_etc");
        fs::write(&file, "x").unwrap();
        let handle = File::open(&file).unwrap();
        let metadata = handle.metadata().unwrap();
        let protected = [(metadata.dev(), metadata.ino())];
        let error = reject_protected_alias(&handle, &protected).unwrap_err();
        assert!(error.to_string().contains("bind-mount alias"));
    }

    #[test]
    fn ordinary_identity_passes_the_alias_check() {
        let fixture = tempfile::tempdir().unwrap();
        let file = fixture.path().join("ordinary");
        fs::write(&file, "x").unwrap();
        let handle = File::open(&file).unwrap();
        reject_protected_alias(&handle, protected_dir_identities()).unwrap();
        // An empty protection set exercises the same pass path.
        reject_protected_alias(&handle, &[]).unwrap();
    }

    #[test]
    fn alias_ancestors_in_mountinfo_block_deletion() {
        let mountinfo = b"23 0 253:0 / / rw,relatime - ext4 /dev/root\n42 23 253:0 /etc /mnt/alias rw - ext4 /dev/root\n36 23 0:41 / /tmp rw - tmpfs tmpfs\n";
        // The alias mount itself and anything beneath it are blocked.
        let error = check_not_under_protected_mount_alias(Path::new("/mnt/alias"), mountinfo, &[])
            .unwrap_err();
        assert!(error.to_string().contains("bind mount"));
        let error =
            check_not_under_protected_mount_alias(Path::new("/mnt/alias/deep/dir"), mountinfo, &[])
                .unwrap_err();
        assert!(error.to_string().contains("bind mount"));
        // Whole-fs and tmpfs mounts are natural parents; non-mount paths too.
        check_not_under_protected_mount_alias(Path::new("/mnt"), mountinfo, &[]).unwrap();
        check_not_under_protected_mount_alias(Path::new("/tmp/x"), mountinfo, &[]).unwrap();
    }

    /// Btrfs-style bind: the fs-root string is the superblock path (`/@/etc`),
    /// which classifies as None, so only the `(dev, ino)` arm can catch it.
    #[test]
    fn subvolume_rooted_binds_are_caught_by_identity() {
        let fixture = tempfile::tempdir().unwrap();
        let alias = fixture.path().join("alias-of-etc");
        fs::create_dir_all(&alias).unwrap();
        let mountinfo = format!(
            "23 0 253:0 /@ / rw,relatime - btrfs /dev/root\n42 23 253:0 /@/etc {} rw - btrfs /dev/root\n",
            alias.display()
        );
        let metadata = fs::metadata(&alias).unwrap();
        let protected = [(metadata.dev(), metadata.ino())];
        let error = check_not_under_protected_mount_alias(&alias, mountinfo.as_bytes(), &protected)
            .unwrap_err();
        assert!(error.to_string().contains("bind mount"));
        // Without the identity match the same layout stays deletable.
        check_not_under_protected_mount_alias(&alias, mountinfo.as_bytes(), &[]).unwrap();
    }

    #[test]
    fn deletion_beneath_real_mount_layouts_is_allowed() {
        let fixture = tempfile::tempdir().unwrap();
        // The tempdir's real mount ancestors — subvolume /@ root, tmpfs /tmp —
        // must not block ordinary deletions.
        check_not_under_protected_mount_alias(
            fixture.path(),
            &fs::read("/proc/self/mountinfo").unwrap(),
            protected_dir_identities(),
        )
        .unwrap();
    }

    #[test]
    fn replacement_after_final_verification_demonstrates_namespace_limit() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("source");
        let saved = fixture.path().join("saved");
        fs::write(&source, "confirmed").unwrap();
        let (parent, name, target) = prepare_target(&source).unwrap();
        remove_at_with_hook(&parent, &name, &target, &|_, _| {
            fs::rename(&source, &saved).unwrap();
            fs::write(&source, "replacement").unwrap();
        })
        .unwrap();
        assert_eq!(fs::read_to_string(saved).unwrap(), "confirmed");
        // Records the limitation, rather than claiming checks serialize writers.
        assert!(!source.exists());
    }

    #[test]
    fn replacing_parent_cannot_redirect_checked_deletion() {
        let temp = tempfile::tempdir().unwrap();
        let parent_path = temp.path().join("parent");
        let moved = temp.path().join("moved");
        let victim = temp.path().join("victim");
        fs::create_dir_all(parent_path.join("ssh")).unwrap();
        fs::create_dir_all(victim.join("ssh")).unwrap();
        fs::write(parent_path.join("ssh/selected"), "selected").unwrap();
        fs::write(victim.join("ssh/keep"), "protected fixture").unwrap();
        let (parent, name, target) = prepare_target(&parent_path.join("ssh")).unwrap();
        inspect_tree(&target).unwrap();
        fs::rename(&parent_path, &moved).unwrap();
        symlink(&victim, &parent_path).unwrap();
        remove_at(&parent, &name, &target).unwrap();
        assert!(!moved.join("ssh").exists());
        assert_eq!(
            fs::read_to_string(victim.join("ssh/keep")).unwrap(),
            "protected fixture"
        );
    }

    #[test]
    fn replacing_final_directory_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("selected");
        let moved = temp.path().join("moved");
        let victim = temp.path().join("victim");
        fs::create_dir(&path).unwrap();
        fs::create_dir(&victim).unwrap();
        fs::write(victim.join("keep"), "untouched").unwrap();
        let (parent, name, target) = prepare_target(&path).unwrap();
        fs::rename(&path, &moved).unwrap();
        symlink(&victim, &path).unwrap();
        assert!(remove_at(&parent, &name, &target).is_err());
        assert!(moved.is_dir());
        assert_eq!(
            fs::read_to_string(victim.join("keep")).unwrap(),
            "untouched"
        );
    }
}

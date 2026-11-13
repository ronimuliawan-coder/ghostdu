use crate::fs::entry::DeleteSafety;
use crate::fs::scanner::parse_mount_roots;
use crate::ghost::{classify_path, classify_safety};
use rustix::fs::{openat2, unlinkat, AtFlags, Dir, Mode, OFlags, ResolveFlags, CWD};
use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::fs::{self, File};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

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
    delete_with_verification_impl(
        paths,
        verify,
        read_mount_roots(),
        protected_dir_identities(),
    )
}

fn delete_with_verification_impl<P: AsRef<Path>>(
    paths: &[P],
    verify: impl Fn(&Path, &File) -> io::Result<()>,
    mount_roots: io::Result<HashMap<PathBuf, PathBuf>>,
    protected: Vec<(&'static str, u64, u64)>,
) -> DeleteResult {
    let mut result = DeleteResult::default();
    // One mountinfo read per batch (review round 1, Qodo finding 6): a bind
    // installed mid-batch is picked up by the next batch. An unreadable mount
    // table fails the whole batch closed — without it the alias guard is
    // blind and deletion must not proceed.
    let mount_roots = match mount_roots {
        Ok(roots) => roots,
        Err(error) => {
            result.failed = paths
                .iter()
                .map(|p| (p.as_ref().to_path_buf(), error.to_string()))
                .collect();
            return result;
        }
    };
    for path in paths {
        let path = path.as_ref();
        let removal = prepare_target_with_mount_roots(path, &mount_roots, &protected).and_then(
            |(parent, name, target)| {
                verify(path, &target)?;
                // Reject existing mounts/unreadable subtrees before mutating anything.
                inspect_tree(&target)?;
                remove_at(&parent, &name, &target)
            },
        );
        match removal {
            Ok(()) => result.succeeded.push(path.to_path_buf()),
            Err(error) => result.failed.push((path.to_path_buf(), error.to_string())),
        }
    }
    result
}

/// The live mount table's mount point → fs-root mapping, read fresh so newly
/// installed bind mounts are never missed by a stale snapshot.
pub(super) fn read_mount_roots() -> io::Result<HashMap<PathBuf, PathBuf>> {
    Ok(parse_mount_roots(&fs::read("/proc/self/mountinfo")?))
}

pub(super) fn prepare_target_with_mount_roots(
    path: &Path,
    mount_roots: &HashMap<PathBuf, PathBuf>,
    protected: &[(&'static str, u64, u64)],
) -> io::Result<(File, CString, File)> {
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
    check_not_under_protected_mount_alias(&canonical_parent, mount_roots, protected)?;
    check_safety(&pinned_path.join(std::ffi::OsStr::from_bytes(name.to_bytes())))?;
    let target = open_child(&parent, &name)?;
    reject_protected_alias(&target, protected)?;
    Ok((parent, name, target))
}

/// `(path, dev, ino)` of the exact-name System roots in `classify_safety`'s
/// first block, with three deliberate exclusions. `/tmp` and `/var/tmp`: their
/// children are deliberately deletable temp files, so no identity test can
/// separate a `/tmp` alias from legitimate `/tmp` children — and the harm of
/// deleting inside one is bounded by the tool's ordinary job. `/`: its
/// identity stays out because trashing the root itself is already
/// string-blocked, and on legacy `numdevs=1` btrfs layouts every subvolume
/// would otherwise share the root's `(dev, 256)` and be blocked wholesale
/// (with the default per-subvolume devices, subvolume mounts carry distinct
/// `st_dev`s and cannot collide with it — see btrfs anon-device behavior). The path travels with the identity so a protected
/// directory mounted at its normal location can be told apart from an alias
/// of it (review round 1, Qodo finding 3). Captured **per batch** alongside
/// the mount snapshot: a protected root mounted after an earlier batch —
/// `/boot` appearing mid-session — must be recognized by later ones (review
/// round 3, Qodo finding 83104618). Missing paths (container images without
/// /sys, /run, …) are skipped; the ~21 stats per batch are negligible next to
/// the mountinfo read.
pub(super) fn protected_dir_identities() -> Vec<(&'static str, u64, u64)> {
    [
        "/bin",
        "/sbin",
        "/lib32",
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
    .filter_map(|path| fs::metadata(path).ok().map(|m| (*path, m.dev(), m.ino())))
    .collect()
}

/// String guardrails classify paths; a bind-mount alias of a protected
/// directory elsewhere in the tree shares the source's `(dev, ino)`, so the
/// string check alone cannot see it (audit H2). Identity comparison closes
/// the alias of the protected roots themselves at fd level, layout-proof:
/// btrfs subvolume layouts report the mount's fs-root as the superblock path
/// (`/@/etc`), which no string check can classify. Residual, documented: a
/// bind alias of a protected *subdirectory* (e.g. `/etc/ssh` mounted
/// elsewhere) has an ordinary inode and stays reachable under `cross_mounts`
/// scans; closing that needs mountinfo source tracking.
fn reject_protected_alias(target: &File, protected: &[(&'static str, u64, u64)]) -> io::Result<()> {
    let metadata = target.metadata()?;
    let identity = (metadata.dev(), metadata.ino());
    if protected
        .iter()
        .any(|(_, dev, ino)| identity == (*dev, *ino))
    {
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
/// when it sits beneath any directory that aliases a protected tree. Every
/// ancestor is statted (review round 2, Qodo finding 695f9642): under a bind
/// of `/` mounted at `/host`, `/host/etc` is an ordinary directory carrying
/// `/etc`'s identity, and only the full walk sees it. The decision per
/// matching identity:
/// - the protected path itself — and its natural ancestors when it is
///   itself a mount, like Fedora's `/root` subvolume mounted at `/` — never
///   blocks (openSUSE's `/@/var` at `/var`, ostree's `/var` bind — review
///   round 1);
/// - an ancestor that is itself a sub-path mount blocks only when the
///   protected path is either not a mount point (its inode cannot otherwise
///   appear as a mount root) or is mounted with the *same* fs-root —
///   `/var/log` on its own openSUSE subvolume carries a different
///   `st_dev`/fs-root from `/var` and must not count as its alias;
/// - a plain directory (no mount point of its own) carrying a protected
///   root's identity blocks: btrfs gives each subvolume its own anonymous
///   device (numdevs=many default), so equal `(dev, ino)` at another path is
///   the protected subvolume itself exposed through a parent- or fs-root-tree
///   mount (review round 4). This closes the whole-fs-bind hole:
///   `/host/etc/ssh` is reached through `/host/etc`, an ordinary directory
///   under the `/host` mount.
///
/// The fs-root *string* is never classified — it is a superblock-relative
/// path whose name coincides with host paths on real layouts (Fedora's root
/// subvolume is `/root` mounted at `/`; an independent filesystem has its own
/// `/etc`) — review round 1, Qodo finding 1.
fn check_not_under_protected_mount_alias(
    canonical_parent: &Path,
    mount_roots: &HashMap<PathBuf, PathBuf>,
    protected: &[(&'static str, u64, u64)],
) -> io::Result<()> {
    let mut current = Some(canonical_parent);
    while let Some(path) = current {
        if let Ok(metadata) = fs::metadata(path) {
            if ancestor_blocks(
                path,
                (metadata.dev(), metadata.ino()),
                mount_roots,
                protected,
            ) {
                return Err(io::Error::other(
                    "Blocked: target lives beneath a bind mount of a protected system directory",
                ));
            }
        }
        current = path.parent();
    }
    Ok(())
}

/// Pure decision for one ancestor, over caller-supplied mount roots and
/// protected identities, so every arm is testable without mounting anything.
fn ancestor_blocks(
    path: &Path,
    identity: (u64, u64),
    mount_roots: &HashMap<PathBuf, PathBuf>,
    protected: &[(&'static str, u64, u64)],
) -> bool {
    protected.iter().any(|(protected_path, dev, ino)| {
        if identity != (*dev, *ino) {
            return false;
        }
        let protected = Path::new(protected_path);
        // The protected directory's own location — and its natural ancestors,
        // when it is itself a mount, like Fedora's `/root` subvolume mounted
        // at `/` — never alias it.
        if path == protected || protected.starts_with(path) {
            return false;
        }
        match mount_roots.get(path) {
            // Qodo round 3 (4c1a7365): a protected directory that is its own
            // filesystem (a separate /boot partition) reports fs-root `/`,
            // and its bind alias carries that same fs-root — an fs-root
            // != "/" exemption skipped exactly those aliases. Decide by
            // fs-root equality alone.
            Some(fs_root) => mount_roots
                .get(protected)
                .is_none_or(|protected_root| protected_root == fs_root),
            // Qodo round 4 (43bdcc3a): btrfs assigns each subvolume its own
            // anonymous st_dev (numdevs=many, the default), so an identity
            // match on a plain directory means the protected subvolume's own
            // root directory, exposed through a parent- or fs-root-tree mount
            // — deleting beneath it deletes real content. Block.
            None => true,
        }
    })
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
        let protected = [("/etc", metadata.dev(), metadata.ino())];
        let error = reject_protected_alias(&handle, &protected).unwrap_err();
        assert!(error.to_string().contains("bind-mount alias"));
    }

    #[test]
    fn ordinary_identity_passes_the_alias_check() {
        let fixture = tempfile::tempdir().unwrap();
        let file = fixture.path().join("ordinary");
        fs::write(&file, "x").unwrap();
        let handle = File::open(&file).unwrap();
        reject_protected_alias(&handle, &protected_dir_identities()).unwrap();
        // An empty protection set exercises the same pass path.
        reject_protected_alias(&handle, &[]).unwrap();
    }

    fn parse_roots(mountinfo: &str) -> HashMap<PathBuf, PathBuf> {
        parse_mount_roots(mountinfo.as_bytes())
    }

    #[test]
    fn alias_ancestors_in_mountinfo_block_deletion() {
        // The alias mount points at a real fixture directory whose identity is
        // registered as the host's `/etc`; a different path with that identity
        // is an alias.
        let fixture = tempfile::tempdir().unwrap();
        let alias = fixture.path().join("etc-alias");
        fs::create_dir_all(&alias).unwrap();
        let metadata = fs::metadata(&alias).unwrap();
        let protected = [("/etc", metadata.dev(), metadata.ino())];
        let mountinfo = format!(
            "23 0 253:0 / / rw,relatime - ext4 /dev/root\n42 23 253:0 /etc {} rw - ext4 /dev/root\n36 23 0:41 / /tmp rw - tmpfs tmpfs\n",
            alias.display()
        );
        let roots = parse_roots(&mountinfo);
        // The alias mount itself and anything beneath it are blocked.
        let error = check_not_under_protected_mount_alias(&alias, &roots, &protected).unwrap_err();
        assert!(error.to_string().contains("bind mount"));
        let deep = fixture.path().join("etc-alias/deep/dir");
        fs::create_dir_all(&deep).unwrap();
        let error = check_not_under_protected_mount_alias(&deep, &roots, &protected).unwrap_err();
        assert!(error.to_string().contains("bind mount"));
        // Whole-fs and tmpfs mounts are natural parents; non-mount paths too.
        check_not_under_protected_mount_alias(&fixture.path().join("mnt"), &roots, &protected)
            .unwrap();
        check_not_under_protected_mount_alias(&fixture.path().join("tmp/x"), &roots, &protected)
            .unwrap();
    }

    /// Btrfs-style bind: the fs-root string is the superblock path (`/@/etc`),
    /// which no string check can classify, so only the identity arm catches it.
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
        let protected = [("/etc", metadata.dev(), metadata.ino())];
        let error =
            check_not_under_protected_mount_alias(&alias, &parse_roots(&mountinfo), &protected)
                .unwrap_err();
        assert!(error.to_string().contains("bind mount"));
        // Without the identity match the same layout stays deletable.
        check_not_under_protected_mount_alias(&alias, &parse_roots(&mountinfo), &[]).unwrap();
    }

    /// Qodo round 2 (695f9642): under a whole-fs bind of `/` at `/host`,
    /// `/host/etc` is an ordinary directory carrying `/etc`'s identity — the
    /// full ancestor walk must catch it even with no sub-path mounts.
    #[test]
    fn root_bind_subtree_components_are_rejected() {
        let fixture = tempfile::tempdir().unwrap();
        let alias = fixture.path().join("rootalias/etc");
        fs::create_dir_all(&alias).unwrap();
        let metadata = fs::metadata(&alias).unwrap();
        let protected = [("/etc", metadata.dev(), metadata.ino())];
        // Whole-fs style mountinfo: every mount carries fs-root `/`, so the
        // plain-directory arm is the only one that can fire.
        let mountinfo =
            "23 0 253:0 / / rw - ext4 /dev/root\n42 23 253:0 / /host rw - ext4 /dev/root\n"
                .to_string();
        let error =
            check_not_under_protected_mount_alias(&alias, &parse_roots(&mountinfo), &protected)
                .unwrap_err();
        assert!(error.to_string().contains("bind mount"));
        // Without the identity match the same layout stays deletable.
        check_not_under_protected_mount_alias(&alias, &parse_roots(&mountinfo), &[]).unwrap();
    }

    /// Btrfs subvolume roots all report `st_ino` 256 on one filesystem: a
    /// mount point sharing a protected root's identity but mounted from a
    /// different fs-root is a different mount, not an alias (openSUSE's
    /// `/var/log` beside `/var`); the same fs-root is an alias and blocks.
    #[test]
    fn same_fs_root_decides_between_collision_and_alias() {
        let fixture = tempfile::tempdir().unwrap();
        let log_mount = fixture.path().join("log");
        fs::create_dir_all(&log_mount).unwrap();
        let metadata = fs::metadata(&fixture).unwrap();
        let protected = [("/var/cache", metadata.dev(), metadata.ino())];
        // Different fs-roots: collision, not alias — allowed.
        let distinct = format!(
            "23 0 253:0 /@log {} rw - btrfs /dev/root\n24 23 253:0 /@cache /var/cache rw - btrfs /dev/root\n",
            fixture.path().display()
        );
        check_not_under_protected_mount_alias(fixture.path(), &parse_roots(&distinct), &protected)
            .unwrap();
        // The same fs-root under a different path: alias — blocked.
        let aliased = format!(
            "23 0 253:0 /@cache {} rw - btrfs /dev/root\n24 23 253:0 /@cache /var/cache rw - btrfs /dev/root\n",
            fixture.path().display()
        );
        let error = check_not_under_protected_mount_alias(
            fixture.path(),
            &parse_roots(&aliased),
            &protected,
        )
        .unwrap_err();
        assert!(error.to_string().contains("bind mount"));
    }

    /// Qodo round 3 (4c1a7365): /boot on its own partition reports fs-root
    /// `/`, and its bind alias carries that same fs-root — the removed
    /// fs-root != "/" exemption skipped exactly this case.
    #[test]
    fn whole_fs_protected_root_alias_is_blocked() {
        let fixture = tempfile::tempdir().unwrap();
        let natural = fixture.path().join("boot");
        let alias = fixture.path().join("mnt/b");
        fs::create_dir_all(&alias).unwrap();
        // Register the alias directory's identity as the host's /boot: the
        // alias IS the same directory through the bind.
        let metadata = fs::metadata(&alias).unwrap();
        let protected = [("/boot", metadata.dev(), metadata.ino())];
        let mountinfo = format!(
            "23 0 253:0 / {} rw - ext4 /dev/root\n42 23 253:0 / {} rw - ext4 /dev/root\n",
            natural.display(),
            alias.display()
        );
        let deep = fixture.path().join("mnt/b/grub");
        fs::create_dir_all(&deep).unwrap();
        let error =
            check_not_under_protected_mount_alias(&deep, &parse_roots(&mountinfo), &protected)
                .unwrap_err();
        assert!(error.to_string().contains("bind mount"));
    }

    /// Qodo round 4 (43bdcc3a): with btrfs's default per-subvolume devices,
    /// an identity match on a plain directory means the protected subvolume
    /// itself, exposed through a parent- or fs-root-tree mount — blocking is
    /// correct; identity mismatch never blocks.
    #[test]
    fn plain_directory_identity_match_blocks() {
        let empty = HashMap::new();
        // The protected subvolume's root directory under a fs-root-tree mount.
        assert!(ancestor_blocks(
            Path::new("/mnt/btrfs-root/@var"),
            (253, 256),
            &empty,
            &[("/var", 253, 256)],
        ));
        // A regular protected directory reached through a whole-fs bind.
        assert!(ancestor_blocks(
            Path::new("/host/etc"),
            (253, 3011),
            &empty,
            &[("/etc", 253, 3011)],
        ));
        // Identity mismatch never blocks.
        assert!(!ancestor_blocks(
            Path::new("/host/etc"),
            (253, 9999),
            &empty,
            &[("/etc", 253, 3011)],
        ));
    }

    /// Qodo finding 3: a protected directory mounted at its normal location —
    /// openSUSE's `/@/var` subvolume at `/var`, ostree's `/var` bind — matches
    /// its own identity, which must not count as an alias of itself.
    #[test]
    fn protected_directory_at_its_own_mount_point_stays_deletable() {
        // /var exists on every Linux host; give its real identity to the guard
        // and mount it (per the synthetic mountinfo) at exactly that path.
        let metadata = fs::metadata("/var").unwrap();
        let protected = [("/var", metadata.dev(), metadata.ino())];
        let mountinfo = "23 0 253:0 /@ / rw,relatime - btrfs /dev/root\n42 23 253:0 /@/var /var rw,relatime - btrfs /dev/root\n44 23 253:0 /@/var/log /var/log rw,relatime - btrfs /dev/root\n";
        let roots = parse_roots(mountinfo);
        // Cleanup beneath the natural /var mount must work (log is a nested
        // subvolume on the same layout, with an unclassified fs-root).
        check_not_under_protected_mount_alias(Path::new("/var/log/journal"), &roots, &protected)
            .unwrap();
        // The same identity showing up under a DIFFERENT path is an alias.
        let fixture = tempfile::tempdir().unwrap();
        let alias = fixture.path().join("var-alias");
        fs::create_dir_all(&alias).unwrap();
        let alias_meta = fs::metadata(&alias).unwrap();
        let alias_protected = [("/var", alias_meta.dev(), alias_meta.ino())];
        let alias_mountinfo = format!(
            "23 0 253:0 /@ / rw,relatime - btrfs /dev/root\n42 23 253:0 /@/var {} rw - btrfs /dev/root\n",
            alias.display()
        );
        let error = check_not_under_protected_mount_alias(
            &alias,
            &parse_roots(&alias_mountinfo),
            &alias_protected,
        )
        .unwrap_err();
        assert!(error.to_string().contains("bind mount"));
    }

    /// Qodo finding 1: Fedora-style layouts name the root subvolume `/root`
    /// and mount it at `/`; an fs-root string must never be classified as a
    /// host path, so nothing here may block.
    #[test]
    fn fedora_style_root_subvolume_does_not_block_deletions() {
        let fixture = tempfile::tempdir().unwrap();
        let mountinfo = format!(
            "36 1 0:33 /root / rw,relatime - btrfs /dev/vda3 rw,subvol=/root\n37 36 0:33 /root/home {} rw,relatime - btrfs /dev/vda3 rw,subvol=/root\n",
            fixture.path().display()
        );
        // Even with /root's real identity registered, the root mount and the
        // subpath home mount must not block: their paths are not aliases.
        let root_meta = fs::metadata("/root");
        let protected: Vec<(&'static str, u64, u64)> = match root_meta {
            Ok(meta) => vec![("/root", meta.dev(), meta.ino())],
            Err(_) => vec![],
        };
        check_not_under_protected_mount_alias(fixture.path(), &parse_roots(&mountinfo), &protected)
            .unwrap();
    }

    #[test]
    fn deletion_beneath_real_mount_layouts_is_allowed() {
        let fixture = tempfile::tempdir().unwrap();
        // The tempdir's real mount ancestors — subvolume /@ root, tmpfs /tmp —
        // must not block ordinary deletions.
        check_not_under_protected_mount_alias(
            fixture.path(),
            &read_mount_roots().unwrap(),
            &protected_dir_identities(),
        )
        .unwrap();
    }

    /// An unreadable mount table fails the whole batch closed.
    #[test]
    fn unreadable_mountinfo_fails_the_batch() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("keep.txt");
        fs::write(&file, "x").unwrap();
        let result = delete_with_verification_impl(
            std::slice::from_ref(&file),
            |_, _| Ok(()),
            Err(io::Error::other("mountinfo boom")),
            Vec::new(),
        );
        assert!(result.succeeded.is_empty());
        assert_eq!(result.failed.len(), 1);
        assert!(result.failed[0].1.contains("mountinfo boom"));
        assert!(file.exists());
    }

    #[test]
    fn replacement_after_final_verification_demonstrates_namespace_limit() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("source");
        let saved = fixture.path().join("saved");
        fs::write(&source, "confirmed").unwrap();
        let (parent, name, target) = prepare_target_with_mount_roots(
            &source,
            &read_mount_roots().unwrap(),
            &protected_dir_identities(),
        )
        .unwrap();
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
        let (parent, name, target) = prepare_target_with_mount_roots(
            &parent_path.join("ssh"),
            &read_mount_roots().unwrap(),
            &protected_dir_identities(),
        )
        .unwrap();
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
        let (parent, name, target) = prepare_target_with_mount_roots(
            &path,
            &read_mount_roots().unwrap(),
            &protected_dir_identities(),
        )
        .unwrap();
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

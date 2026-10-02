use super::delete::{inspect_tree, prepare_target, verify_identity};
use rustix::fs::{
    mkdirat, openat2, renameat_with, unlinkat, AtFlags, Mode, OFlags, RenameFlags, ResolveFlags,
    CWD,
};
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
    let mut result = TrashResult::default();
    for path in paths {
        let path = path.as_ref();
        let moved: io::Result<()> = (|| {
            let (parent, name, target) = prepare_target(path)?;
            verify(path, &target)?;
            inspect_tree(&target)?;
            let original = fd_path(&parent)?.join(OsStr::from_bytes(name.to_bytes()));
            let (trash, topdir) = trash_directory(&parent)?;
            let restore_path = match topdir {
                Some(topdir) => original
                    .strip_prefix(topdir)
                    .map_err(io::Error::other)?
                    .to_path_buf(),
                None => original,
            };
            move_verified(&parent, &name, &target, &trash, &restore_path)
        })();
        match moved {
            Ok(()) => result.succeeded.push(path.to_path_buf()),
            Err(error) => result.failed.push((path.to_path_buf(), error.to_string())),
        }
    }
    result
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
    let meta = directory.metadata()?;
    if meta.uid() != effective_uid() || meta.mode() & 0o077 != 0 {
        return Err(io::Error::other(
            "Trash directory must be owned by the current user and private (0700)",
        ));
    }
    Ok(directory)
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
    if left_stat.stx_mask & StatxFlags::MNT_ID.bits() != 0
        && right_stat.stx_mask & StatxFlags::MNT_ID.bits() != 0
    {
        Ok(left_stat.stx_mnt_id == right_stat.stx_mnt_id)
    } else {
        // Older kernels lack mount IDs; a cross-mount rename will still fail safely.
        Ok(left.metadata()?.dev() == right.metadata()?.dev())
    }
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
    if let Ok(shared) = open_directory(&top, Path::new(".Trash"), true) {
        if shared.metadata()?.mode() & 0o1000 != 0 {
            if let Ok(trash) = private_directory(&shared, Path::new(&effective_uid().to_string())) {
                return Ok((trash, Some(topdir)));
            }
        }
    }
    let trash = private_directory(&top, Path::new(&format!(".Trash-{}", effective_uid())))?;
    Ok((trash, Some(topdir)))
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
    Renamed,
}

// The hook permits subprocess interruption tests at actual transaction boundaries.
fn move_verified_with_hook(
    parent: &File,
    name: &CStr,
    target: &File,
    trash: &File,
    original: &Path,
    hook: impl Fn(MovePhase, &File, &str),
) -> io::Result<()> {
    let files = private_directory(trash, Path::new("files"))?;
    let info = private_directory(trash, Path::new("info"))?;
    static NEXT_NAME: AtomicU64 = AtomicU64::new(0);
    for _ in 0..1000 {
        let sequence = NEXT_NAME.fetch_add(1, Ordering::Relaxed);
        let stored_name = format!("ghostdu-{}-{sequence}", std::process::id());
        let info_name = format!("{stored_name}.trashinfo");
        let metadata_fd = match openat2(
            &info,
            &info_name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
            ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_XDEV,
        ) {
            Ok(fd) => fd,
            Err(rustix::io::Errno::EXIST) => continue,
            Err(error) => return Err(error.into()),
        };
        hook(MovePhase::Reserved, &info, &info_name);
        let moved: io::Result<()> = (|| {
            let mut metadata = File::from(metadata_fd);
            writeln!(
                metadata,
                "[Trash Info]\nPath={}\nDeletionDate={}",
                encode_path(original),
                chrono::Local::now().format("%Y-%m-%dT%H:%M:%S")
            )?;
            metadata.sync_all()?;
            hook(MovePhase::MetadataSynced, &info, &info_name);
            verify_identity(parent, name, target)?;
            // Both directories stay pinned. Never re-resolve the user's source path or copy/delete.
            renameat_with(parent, name, &files, &stored_name, RenameFlags::NOREPLACE)?;
            hook(MovePhase::Renamed, &info, &info_name);
            Ok(())
        })();
        if let Err(error) = &moved {
            if let Err(cleanup) = unlinkat(&info, &info_name, AtFlags::empty()) {
                let location = fd_path(&info)
                    .map(|path| path.join(&info_name))
                    .unwrap_or_else(|_| PathBuf::from(&info_name));
                return Err(io::Error::other(format!(
                    "{error}; metadata cleanup failed at {}: {cleanup}; inspect this orphaned entry",
                    location.display()
                )));
            }
        }
        match moved {
            Err(error) if error.raw_os_error() == Some(libc::EEXIST) => continue,
            result => return result,
        }
    }
    Err(io::Error::other("Cannot reserve a unique trash entry"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

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

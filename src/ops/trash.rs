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
    let mut result = TrashResult::default();
    for path in paths {
        let path = path.as_ref();
        let moved: io::Result<()> = (|| {
            let (parent, name, target) = prepare_target(path)?;
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

fn home_trash() -> io::Result<File> {
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .ok_or_else(|| io::Error::other("No home trash location configured"))?;
    if !data_home.is_absolute() {
        return Err(io::Error::other("Trash data directory must be absolute"));
    }
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

fn trash_directory(parent: &File) -> io::Result<(File, Option<PathBuf>)> {
    if let Ok(home) = home_trash() {
        if same_mount(&home, parent)? {
            return Ok((home, None));
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
        let moved: io::Result<()> = (|| {
            let mut metadata = File::from(metadata_fd);
            writeln!(
                metadata,
                "[Trash Info]\nPath={}\nDeletionDate={}",
                encode_path(original),
                chrono::Local::now().format("%Y-%m-%dT%H:%M:%S")
            )?;
            metadata.sync_all()?;
            verify_identity(parent, name, target)?;
            // Both directories stay pinned. Never re-resolve the user's source path or copy/delete.
            renameat_with(parent, name, &files, &stored_name, RenameFlags::NOREPLACE)?;
            Ok(())
        })();
        if moved.is_err() {
            let _ = unlinkat(&info, &info_name, AtFlags::empty());
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
}

use crate::fs::entry::DeleteSafety;
use crate::ghost::{classify_path, classify_safety};
use rustix::fs::{openat2, unlinkat, AtFlags, Dir, Mode, OFlags, ResolveFlags, CWD};
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
    let mut result = DeleteResult::default();
    for path in paths {
        let path = path.as_ref();
        let removal = prepare_target(path).and_then(|(parent, name, target)| {
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
    if pinned_path != canonical_parent {
        return Err(io::Error::other(
            "Target parent changed during verification",
        ));
    }
    check_safety(&pinned_path.join(std::ffi::OsStr::from_bytes(name.to_bytes())))?;
    let target = open_child(&parent, &name)?;
    Ok((parent, name, target))
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
    verify_identity(parent, name, target)?;
    let is_dir = target.metadata()?.is_dir();
    if is_dir {
        let (directory, names) = directory_entries(target)?;
        for child_name in names {
            let child = open_child(&directory, &child_name)?;
            remove_at(&directory, &child_name, &child)?;
        }
    }
    verify_identity(parent, name, target)?;
    // unlinkat never follows the final symlink; recursive work used pinned handles only.
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

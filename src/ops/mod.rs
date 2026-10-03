pub mod delete;
pub mod trash;

pub use delete::permanently_delete;
pub use trash::move_to_trash;

use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Holding the inode open prevents reuse while a selection or confirmation is live.
#[derive(Clone, Debug)]
pub(crate) struct TargetIdentity(Arc<File>);

impl TargetIdentity {
    pub(crate) fn capture(path: &Path) -> io::Result<Self> {
        let file = rustix::fs::openat2(
            rustix::fs::CWD,
            path,
            rustix::fs::OFlags::PATH | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
            rustix::fs::ResolveFlags::empty(),
        )?;
        Ok(Self(Arc::new(File::from(file))))
    }

    fn verify(&self, current: &File) -> io::Result<()> {
        let expected = self.0.metadata()?;
        let actual = current.metadata()?;
        if (expected.dev(), expected.ino(), expected.file_type())
            != (actual.dev(), actual.ino(), actual.file_type())
        {
            return Err(io::Error::other(
                "Target changed; select it again and confirm a new action",
            ));
        }
        Ok(())
    }

    pub(crate) fn matches_path(&self, path: &Path) -> bool {
        Self::capture(path)
            .and_then(|current| self.verify(&current.0))
            .is_ok()
    }

    pub(crate) fn matches_ids(&self, dev: u64, ino: u64, is_dir: bool, is_symlink: bool) -> bool {
        if dev == 0 && ino == 0 {
            return false;
        }
        self.0
            .metadata()
            .map(|meta| {
                meta.dev() == dev
                    && meta.ino() == ino
                    && meta.is_dir() == is_dir
                    && meta.is_symlink() == is_symlink
            })
            .unwrap_or(false)
    }
}

pub(crate) type TargetIdentities = HashMap<PathBuf, TargetIdentity>;

pub(crate) fn verify_confirmed(
    identities: &TargetIdentities,
    path: &Path,
    target: &File,
) -> io::Result<()> {
    identities
        .get(path)
        .ok_or_else(|| {
            io::Error::other("Missing target identity; select it again and confirm a new action")
        })?
        .verify(target)
}

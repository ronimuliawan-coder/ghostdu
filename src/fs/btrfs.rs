//! Btrfs CoW extent insight (Feature 4b): report how much of a file's data is
//! shared with other files (snapshots, reflinks) versus exclusive to it, so
//! users know whether deleting the file actually reclaims blocks.
//!
//! The kernel reports per-extent sharing through `FS_IOC_FIEMAP`, which needs
//! no privileges. qgroup queries and extent-tree walking (which need root)
//! are deliberately out of scope.

use std::io;
use std::path::Path;

/// `BTRFS_SUPER_MAGIC` from `linux/magic.h`, reported in statfs `f_type`.
pub const BTRFS_SUPER_MAGIC: u32 = 0x9123_683E;

/// `FS_IOC_FIEMAP`: `_IOWR('f', 11, struct fiemap)` with the 32-byte header.
const FS_IOC_FIEMAP: libc::c_ulong = 0xC020_660B;
/// `FIEMAP_EXTENT_LAST` from `linux/fiemap.h`: final extent of the file.
const FIEMAP_EXTENT_LAST: u32 = 0x1;
/// `FIEMAP_EXTENT_SHARED`: the extent's blocks are referenced by other files.
const FIEMAP_EXTENT_SHARED: u32 = 0x2000;

/// `struct fiemap` header from `linux/fiemap.h` (32 bytes).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct FiemapHead {
    fm_start: u64,
    fm_length: u64,
    fm_flags: u32,
    fm_mapped_extents: u32,
    fm_extent_count: u32,
    fm_reserved: u32,
}

/// `struct fiemap_extent` from `linux/fiemap.h` (56 bytes — note the two
/// reserved words before the flags).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct FiemapExtent {
    fe_logical: u64,
    fe_physical: u64,
    fe_length: u64,
    fe_reserved64: [u64; 2],
    fe_flags: u32,
    fe_reserved: [u32; 3],
}

/// One ioctl buffer: header with the extent array inline. `repr(C)` keeps the
/// array immediately after the header, which is what the kernel expects.
#[repr(C)]
struct FiemapBuf<const N: usize> {
    head: FiemapHead,
    extents: [FiemapExtent; N],
}

const _: () = assert!(std::mem::size_of::<FiemapHead>() == 32);
const _: () = assert!(std::mem::size_of::<FiemapExtent>() == 56);

/// True when `path` lives on a Btrfs filesystem. Unprivileged: a single
/// statfs call. Missing files and NUL bytes report false, never an error.
pub fn is_btrfs(path: &Path) -> bool {
    let cpath = match std::ffi::CString::new(path.as_os_str().as_encoded_bytes()) {
        Ok(c) => c,
        Err(_) => return false,
    };
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(cpath.as_ptr(), &mut stat) } != 0 {
        return false;
    }
    stat.f_type as u32 == BTRFS_SUPER_MAGIC
}

/// `(shared_bytes, exclusive_bytes)` as logical extent lengths (not a reclaim
/// promise: compressed, delayed, and unwritten extents report lengths without
/// matching physical allocation). Walks FIEMAP batches of `N` extents until
/// the kernel marks an extent LAST (or a short batch ends the file). Works on
/// any FIEMAP-capable filesystem, not just Btrfs.
fn extent_sharing_batched<const N: usize>(path: &Path) -> io::Result<(u64, u64)> {
    let file = open_regular_nonblocking(path)?;
    let mut shared = 0u64;
    let mut exclusive = 0u64;
    let mut batches = 0usize;
    let mut pending = Some(0u64);
    while let Some(start) = pending {
        let ((batch_shared, batch_exclusive), next) = walk_batch::<N>(&file, start)?;
        batches += 1;
        // This runs synchronously when the item panel opens: bound the ioctl
        // count so a pathologically fragmented file costs milliseconds, not
        // a hung UI. Over budget the whole query fails (line omitted) rather
        // than reporting partial sums that understate sharing.
        if batches > MAX_BATCHES {
            return Err(io::Error::other("extent budget exceeded"));
        }
        shared = shared.saturating_add(batch_shared);
        exclusive = exclusive.saturating_add(batch_exclusive);
        pending = next;
    }
    Ok((shared, exclusive))
}

/// Upper bound on FIEMAP ioctls per query: milliseconds of UI time, then give
/// up instead of hanging.
const MAX_BATCHES: usize = 64;

/// Open for FIEMAP without ever blocking or following symlinks: FIFOs open
/// fine with `O_NONBLOCK` but then fail the regular-file check, while a
/// blocking open on a writer-less FIFO would hang the UI thread that opened
/// the item panel. Symlinks are rejected outright (`O_NOFOLLOW`).
fn open_regular_nonblocking(path: &Path) -> io::Result<std::fs::File> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::io::FromRawFd;
    let cpath = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "interior NUL"))?;
    let fd = unsafe {
        libc::open(
            cpath.as_ptr(),
            libc::O_RDONLY | libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // Own the fd immediately so every path below closes it via Drop.
    let file = unsafe { std::fs::File::from_raw_fd(fd) };
    // fstat on our own open fd cannot fail in practice; a false reading
    // fails closed (rejected) rather than misclassified.
    if !file.metadata().map(|m| m.is_file()).unwrap_or(false) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    Ok(file)
}

/// Run `op` until it succeeds or fails with anything but `Interrupted`:
/// a signal arriving mid-syscall must not turn into a spurious CoW omission.
fn retry_on_interrupted(mut op: impl FnMut() -> io::Result<i32>) -> io::Result<i32> {
    loop {
        match op() {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            other => return other,
        }
    }
}

/// One FIEMAP batch at `start`: the batch sums plus the next start, or `None`
/// when the file is fully walked (empty batch, LAST extent, or short batch).
fn walk_batch<const N: usize>(
    file: &std::fs::File,
    start: u64,
) -> io::Result<((u64, u64), Option<u64>)> {
    use std::os::unix::io::AsRawFd;
    let mut buf = FiemapBuf {
        head: FiemapHead {
            fm_start: start,
            fm_length: u64::MAX,
            fm_extent_count: N as u32,
            ..Default::default()
        },
        extents: [FiemapExtent::default(); N],
    };
    let fd = file.as_raw_fd();
    let ptr = &mut buf as *mut FiemapBuf<N>;
    retry_on_interrupted(|| {
        let ret = unsafe { libc::ioctl(fd, FS_IOC_FIEMAP, ptr) };
        if ret < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(ret)
        }
    })?;
    let n = (buf.head.fm_mapped_extents as usize).min(N);
    let (batch_shared, batch_exclusive) = sum_extents(&buf.extents[..n]);
    let last = buf.extents[..n]
        .iter()
        .any(|e| e.fe_flags & FIEMAP_EXTENT_LAST != 0);
    let next = if n == 0 || last || (buf.head.fm_mapped_extents as usize) < N {
        None
    } else {
        let tail = buf.extents[n - 1];
        advance_cursor(start, tail.fe_logical, tail.fe_length)
    };
    Ok(((batch_shared, batch_exclusive), next))
}

/// Next FIEMAP cursor after one extent, or `None` when the extent does not
/// advance past `start`. Zero-length or backwards extents would otherwise
/// loop the walker forever on a stuck cursor (blocking the UI thread that
/// opened the item panel).
fn advance_cursor(start: u64, logical: u64, length: u64) -> Option<u64> {
    let next = logical.saturating_add(length);
    (next > start).then_some(next)
}

/// `(shared_bytes, exclusive_bytes)` for one file. Directories, missing
/// files, and filesystems without FIEMAP report an error; callers omit the
/// CoW line in those cases.
pub fn extent_sharing(path: &Path) -> io::Result<(u64, u64)> {
    extent_sharing_batched::<64>(path)
}

/// Add up one batch: extents flagged SHARED count toward shared, the rest
/// (including unknown/delalloc, which conservatively read as reclaimable)
/// toward exclusive.
fn sum_extents(extents: &[FiemapExtent]) -> (u64, u64) {
    let mut shared = 0u64;
    let mut exclusive = 0u64;
    for extent in extents {
        if extent.fe_flags & FIEMAP_EXTENT_SHARED != 0 {
            shared = shared.saturating_add(extent.fe_length);
        } else {
            exclusive = exclusive.saturating_add(extent.fe_length);
        }
    }
    (shared, exclusive)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extent(length: u64, flags: u32) -> FiemapExtent {
        FiemapExtent {
            fe_length: length,
            fe_flags: flags,
            ..Default::default()
        }
    }

    #[test]
    fn interrupted_retries_then_returns() {
        let mut calls = 0;
        let ret = retry_on_interrupted(|| {
            calls += 1;
            if calls < 3 {
                Err(io::Error::from(io::ErrorKind::Interrupted))
            } else {
                Ok(7)
            }
        })
        .unwrap();
        assert_eq!((ret, calls), (7, 3));
        // Non-interrupted errors pass straight through.
        let err = retry_on_interrupted(|| -> io::Result<i32> {
            Err(io::Error::from(io::ErrorKind::Other))
        })
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Other);
    }

    #[test]
    fn stuck_cursor_ends_the_walk() {
        assert_eq!(advance_cursor(10, 10, 5), Some(15));
        assert_eq!(advance_cursor(10, 10, 0), None);
        assert_eq!(advance_cursor(10, 5, 3), None);
    }

    #[test]
    fn sharing_sums_shared_and_exclusive() {
        let extents = [
            extent(100, FIEMAP_EXTENT_SHARED),
            extent(200, 0),
            extent(50, FIEMAP_EXTENT_SHARED | FIEMAP_EXTENT_LAST),
        ];
        assert_eq!(sum_extents(&extents), (150, 200));
        assert_eq!(sum_extents(&[]), (0, 0));
    }

    #[test]
    fn non_btrfs_paths_report_false() {
        // /proc is never Btrfs on any Linux host; interior NUL and missing
        // paths report false instead of erroring.
        assert!(!is_btrfs(Path::new("/proc")));
        assert!(!is_btrfs(Path::new("/definitely/not/here-12345")));
        assert!(!is_btrfs(Path::new("/a\0b")));
    }

    /// Scratch files for FIEMAP tests. `tempfile` lands on tmpfs here (and
    /// tmpfs answers FIEMAP with EOPNOTSUPP), so these tests use a scratch
    /// directory under `target/` instead: btrfs on this host, ext4/overlay
    /// on CI runners, all FIEMAP-capable. Unique per test for parallelism.
    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/btrfs-test-scratch")
            .join(format!("{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn empty_file_shares_nothing() {
        let dir = scratch("empty");
        let empty = dir.join("empty.txt");
        std::fs::write(&empty, []).unwrap();
        assert_eq!(extent_sharing(&empty).unwrap(), (0, 0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn small_file_reports_its_bytes() {
        let dir = scratch("small");
        let file = dir.join("f.txt");
        std::fs::write(&file, vec![7u8; 4096]).unwrap();
        let (shared, exclusive) = extent_sharing(&file).unwrap();
        assert!(shared + exclusive > 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_errors() {
        assert!(extent_sharing(Path::new("/definitely/not/here-12345")).is_err());
    }

    #[test]
    fn ioctl_failure_errors() {
        // Character devices answer FIEMAP with ENOTTY on every Linux host.
        assert!(extent_sharing(Path::new("/dev/null")).is_err());
        // Direct batch probe: opening /dev/null as a File always succeeds,
        // so the ioctl error return is exercised deterministically.
        let devnull = std::fs::File::open("/dev/null").unwrap();
        assert!(walk_batch::<1>(&devnull, 0).is_err());
    }

    #[test]
    fn over_budget_walk_fails_instead_of_partial() {
        // 5000 scattered blocks exceed 64 batches at any batch size: the
        // query must fail (line omitted) rather than report partial sums.
        let dir = scratch("fragmented");
        let file = dir.join("frag.bin");
        {
            use std::io::{Seek, SeekFrom, Write};
            let mut f = std::fs::File::create(&file).unwrap();
            for i in 0..5000u64 {
                f.seek(SeekFrom::Start(i * (1 << 20))).unwrap();
                f.write_all(&[9u8; 4096]).unwrap();
            }
        }
        assert!(extent_sharing(&file).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fifo_and_symlink_are_rejected() {
        let dir = scratch("special");
        let fifo = dir.join("pipe");
        assert_eq!(
            unsafe {
                libc::mkfifo(
                    std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes())
                        .unwrap()
                        .as_ptr(),
                    0o600,
                )
            },
            0
        );
        // O_NONBLOCK opens the writer-less FIFO fine; the regular-file check
        // rejects it instead of hanging or walking it.
        assert!(extent_sharing(&fifo).is_err());
        let link = dir.join("link");
        std::os::unix::fs::symlink(&fifo, &link).unwrap();
        assert!(extent_sharing(&link).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn batches_chain_across_many_extents() {
        // A sparse file has one extent per written block, so a batch of 1
        // forces the ioctl loop to chain through every batch.
        let dir = scratch("sparse");
        let file = dir.join("sparse.bin");
        {
            use std::io::{Seek, SeekFrom, Write};
            let mut f = std::fs::File::create(&file).unwrap();
            for i in 0..4u64 {
                f.seek(SeekFrom::Start(i * (1 << 20))).unwrap();
                f.write_all(&[9u8; 4096]).unwrap();
            }
        }
        let (shared, exclusive) = extent_sharing_batched::<1>(&file).unwrap();
        // Holes report as unwritten extents on top of the written blocks.
        assert!(shared + exclusive >= 4 * 4096);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Live proof against a real shared extent. Excluded from tarpaulin:
    /// reflinks need Btrfs, which CI runners don't have, so this runs in
    /// plain `cargo test` (skipping gracefully elsewhere) instead.
    /// Incompressible input matters: runs of identical bytes compress into
    /// extents the kernel does not flag shared.
    #[cfg(not(tarpaulin_include))]
    #[test]
    fn reflinked_extents_report_shared() {
        // Scratch lives on the crate's own filesystem (btrfs here); /tmp is
        // tmpfs and would skip the proof entirely.
        let dir = scratch("reflink");
        if !is_btrfs(&dir) {
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        let a = dir.join("a.bin");
        // Pseudorandom bytes: incompressible, so the reflinked extent is
        // reported shared (runs of identical bytes compress into extents
        // the kernel does not flag). Bounded and deterministic, unlike
        // reading /dev/urandom to EOF (which never comes).
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let noise: Vec<u8> = (0..1 << 20)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 11) as u8
            })
            .collect();
        std::fs::write(&a, &noise).unwrap();
        let b = dir.join("b.bin");
        let linked = std::process::Command::new("cp")
            .arg("--reflink=always")
            .arg(&a)
            .arg(&b)
            .status()
            .map(|st| st.success())
            .unwrap_or(false);
        if !linked {
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        let (shared, _) = extent_sharing(&a).unwrap();
        assert!(shared > 0, "reflinked extent must report shared");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

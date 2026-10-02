//! Run explicitly with `cargo test --test mount_boundaries -- --ignored` on Linux
//! with unshare, mount, and user namespaces available. All mounts live in a child namespace.
use ghostdu::fs::scanner::{scan_directory_with_options, ScannerOptions};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

#[test]
#[ignore = "requires Linux user/mount namespaces and mount/unshare utilities"]
fn bind_mount_boundaries_and_ancestor_cycles() {
    if std::env::var_os("GHOSTDU_MOUNT_TEST_CHILD").is_none() {
        let output = Command::new("timeout")
            .args(["30s", "unshare", "--user", "--map-root-user", "--mount"])
            .arg(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "bind_mount_boundaries_and_ancestor_cycles",
                "--nocapture",
            ])
            .env("GHOSTDU_MOUNT_TEST_CHILD", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    assert!(Command::new("mount")
        .args(["--make-rprivate", "/"])
        .status()
        .unwrap()
        .success());
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("scan");
    let external = fixture.path().join("external");
    let bind = root.join("same device mount");
    let ancestor = root.join("ancestor");
    let other_device = root.join("other-device");
    for path in [&external, &bind, &ancestor, &other_device] {
        fs::create_dir_all(path).unwrap();
    }
    fs::write(root.join("local"), b"1234567").unwrap();
    fs::write(external.join("bound"), b"12345").unwrap();
    assert!(Command::new("mount")
        .arg("--bind")
        .arg(&external)
        .arg(&bind)
        .status()
        .unwrap()
        .success());
    assert!(Command::new("mount")
        .arg("--bind")
        .arg(&root)
        .arg(&ancestor)
        .status()
        .unwrap()
        .success());
    assert!(Command::new("mount")
        .args(["-t", "tmpfs", "tmpfs"])
        .arg(&other_device)
        .status()
        .unwrap()
        .success());
    fs::write(other_device.join("separate"), b"123456789").unwrap();
    assert_eq!(
        fs::metadata(&root).unwrap().dev(),
        fs::metadata(&bind).unwrap().dev()
    );
    assert_ne!(
        fs::metadata(&root).unwrap().dev(),
        fs::metadata(&other_device).unwrap().dev()
    );

    for cross_mounts in [false, true] {
        let tree = scan_directory_with_options(
            &root,
            None,
            Arc::new(AtomicBool::new(false)),
            ScannerOptions { cross_mounts },
        )
        .unwrap();
        assert_eq!(tree.size, if cross_mounts { 21 } else { 7 });
        for mount in [&bind, &other_device] {
            let entry = tree
                .children
                .iter()
                .find(|entry| &entry.path == mount)
                .unwrap();
            assert_eq!(entry.children.len(), usize::from(cross_mounts));
        }
        let cycle = tree
            .children
            .iter()
            .find(|entry| entry.path == ancestor)
            .unwrap();
        assert!(cycle.children.is_empty());
    }
    // TempDir cleanup must happen after unmounting the synthetic filesystems.
    for mount in [&ancestor, &bind, &other_device] {
        assert!(Command::new("umount")
            .arg(mount)
            .status()
            .unwrap()
            .success());
    }
}

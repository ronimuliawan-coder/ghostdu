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
    // Removal must reject both mount points and ancestors containing mounted data.
    for target in [&root, &bind, &ancestor, &other_device] {
        let trash_result = ghostdu::ops::move_to_trash(&[target]);
        assert!(trash_result.succeeded.is_empty());
        assert_eq!(trash_result.failed.len(), 1);
        let result = ghostdu::ops::permanently_delete(&[target]);
        assert!(result.succeeded.is_empty());
        assert_eq!(result.failed.len(), 1);
        assert_eq!(fs::read(root.join("local")).unwrap(), b"1234567");
        assert_eq!(fs::read(external.join("bound")).unwrap(), b"12345");
        assert_eq!(
            fs::read(other_device.join("separate")).unwrap(),
            b"123456789"
        );
    }
    // Trashing a normal file on another volume uses that volume's private trash.
    let trash_source = other_device.join("trash-file");
    fs::write(&trash_source, "restore me").unwrap();
    let result = ghostdu::ops::move_to_trash(&[&trash_source]);
    assert!(result.failed.is_empty(), "{:?}", result.failed);
    assert_eq!(result.succeeded.len(), 1);
    assert!(!trash_source.exists());
    let private_trash = fs::read_dir(&other_device)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".Trash-")
        })
        .unwrap();
    let stored = fs::read_dir(private_trash.join("files"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert_eq!(fs::read_to_string(stored).unwrap(), "restore me");
    let metadata = fs::read_dir(private_trash.join("info"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert!(fs::read_to_string(metadata)
        .unwrap()
        .contains("Path=trash-file\n"));

    // Also support the FreeDesktop sticky shared .Trash/<uid> layout.
    use std::os::unix::fs::PermissionsExt;
    let shared_trash = other_device.join(".Trash");
    fs::create_dir(&shared_trash).unwrap();
    fs::set_permissions(&shared_trash, fs::Permissions::from_mode(0o1777)).unwrap();
    fs::write(&trash_source, "shared location").unwrap();
    let result = ghostdu::ops::move_to_trash(&[&trash_source]);
    assert!(result.failed.is_empty(), "{:?}", result.failed);
    let user_trash = fs::read_dir(&shared_trash)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let stored = fs::read_dir(user_trash.join("files"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert_eq!(fs::read_to_string(stored).unwrap(), "shared location");
    // TempDir cleanup must happen after unmounting the synthetic filesystems.
    for mount in [&ancestor, &bind, &other_device] {
        assert!(Command::new("umount")
            .arg(mount)
            .status()
            .unwrap()
            .success());
    }
}

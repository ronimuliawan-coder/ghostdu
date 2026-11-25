//! Audit M16: constructing the app must not perform the Docker / `/proc`
//! queries. A wedged Docker daemon would otherwise stall the first frame for
//! its read timeout, so startup has to stay independent of it.
//!
//! The socket at `/var/run/docker.sock` belongs to the host's real daemon and
//! is not ours to substitute, so this measures the observable contract
//! instead: constructing an `App` performs no filesystem work under `/proc`
//! and opens no Docker socket, while the first Ghost Inspector open does.

use ghostdu::ui::App;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

fn scanned(path: &std::path::Path) -> ghostdu::fs::FileEntry {
    ghostdu::fs::scan_directory(path, None, Arc::new(AtomicBool::new(false))).unwrap()
}

#[test]
fn app_construction_does_not_fetch_ghost_data() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.bin"), b"x").unwrap();

    let app = App::new(scanned(dir.path()));

    // Nothing was fetched, and nothing pretends a fetch failed.
    assert!(!app.docker_info.is_available);
    assert!(app.docker_info.error_message.is_none());
    assert!(app.deleted_open_files.is_empty());
    assert_eq!(app.docker_info.items.len(), 0);
}

#[test]
fn first_ghost_open_fetches_exactly_once() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.bin"), b"x").unwrap();
    let mut app = App::new(scanned(dir.path()));

    app.ensure_ghost_loaded();
    // The fetch ran: either it found a daemon or it recorded why it could not.
    assert!(
        app.docker_info.is_available || app.docker_info.error_message.is_some(),
        "the deferred fetch must actually run on first open"
    );

    // Reopening is a no-op, so the inspector does not re-pay the cost each time.
    app.status_message = None;
    app.ensure_ghost_loaded();
    assert!(app.current_status().is_none());
}

/// A rescan before the inspector is ever opened must not fetch ghost data.
/// It would reintroduce the Docker stall the lazy startup removed, and leave
/// `ghost_loaded` false so the first Tab/g paid for the same queries again.
#[test]
fn rescan_before_first_ghost_open_stays_lazy() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.bin"), b"x").unwrap();
    let mut app = App::new(scanned(dir.path()));

    app.refresh_all();
    assert!(
        app.docker_info.error_message.is_none(),
        "a rescan must not query Docker before the inspector is opened"
    );
    assert!(app.deleted_open_files.is_empty());

    // And because the fetch never ran, the first open still does the work.
    app.status_message = None;
    app.ensure_ghost_loaded();
    assert!(
        app.docker_info.is_available || app.docker_info.error_message.is_some(),
        "the first open must still fetch after a lazy rescan"
    );
}

/// Once loaded, a rescan must re-query so the inspector never shows data from
/// whenever it was last opened. Proven by clearing the data first: only a
/// re-query brings it back.
#[test]
fn rescan_after_ghost_load_refreshes_the_data() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.bin"), b"x").unwrap();
    let mut app = App::new(scanned(dir.path()));

    app.ensure_ghost_loaded();
    // Wipe the observed payload, so a stale read is distinguishable from a
    // fresh query.
    app.docker_info = ghostdu::ghost::DockerDiskInfo::default();
    assert!(!app.docker_info.is_available);

    app.refresh_all();
    assert!(
        app.docker_info.is_available || app.docker_info.error_message.is_some(),
        "a rescan after loading must re-query the Docker data"
    );
}

use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

// Test directory scanner & sizing
#[test]
fn test_scanner_and_inode_dedup() {
    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let base = temp_dir.path();

    // Create directory structure:
    // base/
    //   file1.txt (1000 bytes)
    //   hardlink_to_file1.txt (hardlink, shares inode!)
    //   subdir/
    //     file2.txt (2000 bytes)
    //     ghost_dir/ (named node_modules)
    //       cache.bin (5000 bytes)
    let file1_path = base.join("file1.txt");
    let hardlink_path = base.join("hardlink_to_file1.txt");
    let subdir_path = base.join("subdir");
    let file2_path = subdir_path.join("file2.txt");
    let ghost_dir_path = subdir_path.join("node_modules");
    let cache_bin_path = ghost_dir_path.join("cache.bin");

    fs::create_dir_all(&ghost_dir_path).unwrap();

    let data1000 = vec![b'A'; 1000];
    let mut f1 = File::create(&file1_path).unwrap();
    f1.write_all(&data1000).unwrap();
    drop(f1);

    // Create hard link
    fs::hard_link(&file1_path, &hardlink_path).unwrap();

    let data2000 = vec![b'B'; 2000];
    let mut f2 = File::create(&file2_path).unwrap();
    f2.write_all(&data2000).unwrap();
    drop(f2);

    let data5000 = vec![b'C'; 5000];
    let mut f3 = File::create(&cache_bin_path).unwrap();
    f3.write_all(&data5000).unwrap();
    drop(f3);

    // Run scanner
    let stop_signal = Arc::new(AtomicBool::new(false));
    let root_entry = ghostdu_scanner::scan_directory(base, None, stop_signal).unwrap();

    assert_eq!(root_entry.children.len(), 3); // file1.txt, hardlink_to_file1.txt, subdir

    // Verify apparent size: 1000 (file1) + 0 (hardlink deduped!) + 2000 (file2) + 5000 (cache) = 8000 bytes!
    // Without inode dedup it would have been 9000 bytes!
    assert_eq!(root_entry.size, 8000);

    // Verify node_modules was classified as DependencyTree
    let subdir = root_entry
        .children
        .iter()
        .find(|c| c.name == "subdir")
        .unwrap();
    let node_modules = subdir
        .children
        .iter()
        .find(|c| c.name == "node_modules")
        .unwrap();
    assert_eq!(
        node_modules.ghost_kind,
        ghostdu_scanner::GhostKind::DependencyTree
    );
    assert!(node_modules.ghost_kind.is_ghost());
}

#[test]
fn test_permanent_delete() {
    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let file_path = temp_dir.path().join("to_delete.txt");
    let dir_path = temp_dir.path().join("dir_to_delete");
    fs::create_dir_all(&dir_path).unwrap();
    fs::write(&file_path, "erase me").unwrap();
    fs::write(dir_path.join("subfile.txt"), "erase me too").unwrap();

    assert!(file_path.exists());
    assert!(dir_path.exists());

    let targets = vec![file_path.clone(), dir_path.clone()];
    let res = ghostdu_scanner::permanently_delete(&targets);

    assert_eq!(res.succeeded.len(), 2);
    assert_eq!(res.failed.len(), 0);
    assert!(!file_path.exists());
    assert!(!dir_path.exists());
}

#[test]
fn test_move_to_trash() {
    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let file_path = temp_dir.path().join("test_trash.txt");
    fs::write(&file_path, "move to wastebin test").unwrap();
    assert!(file_path.exists());

    let res = ghostdu_scanner::move_to_trash(std::slice::from_ref(&file_path));
    // FreeDesktop trash on /tmp or $HOME:
    if res.succeeded.len() == 1 {
        assert!(!file_path.exists());
    } else {
        // Some container/isolated tempfs might not have trash directory mounted
        assert_eq!(res.failed.len(), 1);
    }
}

#[test]
fn test_app_state_and_navigation() {
    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let base = temp_dir.path();

    let sub_a = base.join("dir_a");
    let sub_b = base.join("dir_b");
    fs::create_dir_all(&sub_a).unwrap();
    fs::create_dir_all(&sub_b).unwrap();
    fs::write(sub_a.join("a.txt"), "hello world").unwrap();
    fs::write(sub_b.join("b.txt"), "antigravity").unwrap();

    let stop_signal = Arc::new(AtomicBool::new(false));
    let root = ghostdu_scanner::scan_directory(base, None, stop_signal).unwrap();

    let mut app = ghostdu::ui::App::new(root);
    assert_eq!(app.visible_children().len(), 2);

    // Test cursor movement
    assert_eq!(app.cursor_index, 0);
    app.cursor_down();
    assert_eq!(app.cursor_index, 1);
    app.cursor_down(); // bounded at max
    assert_eq!(app.cursor_index, 1);
    app.cursor_up();
    assert_eq!(app.cursor_index, 0);

    // Test entering directory
    app.enter_selected();
    assert_eq!(app.path_stack.len(), 1);
    assert_eq!(app.visible_children().len(), 1);

    // Test going up
    app.go_up();
    assert_eq!(app.path_stack.len(), 0);
    assert_eq!(app.visible_children().len(), 2);

    // Test selection
    assert_eq!(app.selected_paths.len(), 0);
    app.toggle_selection();
    assert_eq!(app.selected_paths.len(), 1);
    let (sel_cnt, _) = app.selection_summary();
    assert_eq!(sel_cnt, 1);
    app.toggle_selection();
    assert_eq!(app.selected_paths.len(), 0);

    // Test select all
    app.select_all_visible();
    assert_eq!(app.selected_paths.len(), 2);
    app.select_all_visible();
    assert_eq!(app.selected_paths.len(), 0);

    // Test search filter
    app.search_query = "dir_a".to_string();
    let filtered = app.visible_children();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].name, "dir_a");
    app.search_query.clear();
    assert_eq!(app.visible_children().len(), 2);

    // Test ghost filter cycling
    assert_eq!(app.ghost_filter, ghostdu::ui::GhostFilterMode::ShowAll);
    app.ghost_filter = app.ghost_filter.next();
    assert_eq!(app.ghost_filter, ghostdu::ui::GhostFilterMode::HideGhost);
    app.ghost_filter = app.ghost_filter.next();
    assert_eq!(app.ghost_filter, ghostdu::ui::GhostFilterMode::GhostOnly);
    app.ghost_filter = app.ghost_filter.next();
    assert_eq!(app.ghost_filter, ghostdu::ui::GhostFilterMode::ShowAll);

    // Test sorting cycling
    assert_eq!(app.sort_mode, ghostdu::ui::SortMode::BySizeDesc);
    app.sort_mode = app.sort_mode.next();
    assert_eq!(app.sort_mode, ghostdu::ui::SortMode::BySizeAsc);
    app.sort_mode = app.sort_mode.next();
    assert_eq!(app.sort_mode, ghostdu::ui::SortMode::ByName);
    app.sort_mode = app.sort_mode.next();
    assert_eq!(app.sort_mode, ghostdu::ui::SortMode::ByItems);

    // Test confirm modals
    app.prompt_move_to_trash();
    assert_eq!(app.active_view, ghostdu::ui::ActiveView::ConfirmModal);
    assert_eq!(
        app.pending_action,
        Some(ghostdu::ui::ConfirmAction::MoveToTrash)
    );
    app.cancel_modal();
    assert_eq!(app.active_view, ghostdu::ui::ActiveView::Filesystem);
    assert_eq!(app.pending_action, None);

    app.prompt_permanent_delete();
    assert_eq!(app.active_view, ghostdu::ui::ActiveView::ConfirmModal);
    assert_eq!(
        app.pending_action,
        Some(ghostdu::ui::ConfirmAction::PermanentDelete)
    );
    app.cancel_modal();
    assert_eq!(app.active_view, ghostdu::ui::ActiveView::Filesystem);

    // Test page down / up and bounds
    app.cursor_to_end();
    assert_eq!(app.cursor_index, 1);
    app.cursor_to_start();
    assert_eq!(app.cursor_index, 0);
    app.page_down(10);
    assert_eq!(app.cursor_index, 1);
    app.page_up(10);
    assert_eq!(app.cursor_index, 0);

    // Test parent ascension when at root of scan
    let parent = app.go_up();
    assert!(parent.is_some());
    assert_eq!(parent.unwrap(), base.parent().unwrap());

    // Test item info modal ('i' key)
    assert!(
        app.fs_info.is_some(),
        "app.fs_info should be loaded on init"
    );
    app.open_item_info();
    assert_eq!(app.active_view, ghostdu::ui::ActiveView::ItemInfoModal);
    assert!(app.item_info.is_some());
    let item_info = app.item_info.as_ref().unwrap();
    assert!(item_info.name == "dir_a" || item_info.name == "dir_b");
    assert!(item_info.is_dir);
    assert!(item_info.fs_info.is_some());
}

#[test]
fn test_in_place_refresh() {
    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let base = temp_dir.path();
    let f1 = base.join("f1.txt");
    fs::write(&f1, "12345").unwrap(); // 5 bytes

    let stop_signal = Arc::new(AtomicBool::new(false));
    let root = ghostdu_scanner::scan_directory(base, None, stop_signal).unwrap();
    let mut app = ghostdu::ui::App::new(root);

    assert_eq!(app.visible_children().len(), 1);
    assert_eq!(app.root_entry.size, 5);

    // Write a second file while app is running
    let f2 = base.join("f2.txt");
    fs::write(&f2, "6789012345").unwrap(); // 10 bytes

    // Refresh in-place without restarting!
    app.refresh_all();

    assert_eq!(app.visible_children().len(), 2);
    assert_eq!(app.root_entry.size, 15);
    assert!(app.current_status().is_some());
    assert!(app.current_status().unwrap().contains("Refreshed"));

    // Test refresh while navigating inside a subdirectory
    let sub = base.join("subfolder");
    fs::create_dir_all(&sub).unwrap();
    fs::write(sub.join("sub_a.txt"), "sub_a").unwrap();

    app.refresh_all();
    assert_eq!(app.visible_children().len(), 3);

    // Enter subfolder
    let sub_idx = app
        .visible_children()
        .iter()
        .position(|c| c.name == "subfolder")
        .unwrap();
    app.cursor_index = sub_idx;
    app.enter_selected();
    assert_eq!(app.path_stack.len(), 1);
    assert_eq!(app.visible_children().len(), 1);

    // Add another file in subfolder
    fs::write(sub.join("sub_b.txt"), "sub_b").unwrap();
    app.refresh_all();

    // Still in subfolder, now seeing 2 files!
    assert_eq!(app.path_stack.len(), 1);
    assert_eq!(app.visible_children().len(), 2);
}

#[test]
fn test_small_terminal_rendering() {
    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let base = temp_dir.path();
    fs::write(base.join("test.txt"), "content").unwrap();

    let stop_signal = Arc::new(AtomicBool::new(false));
    let root = ghostdu_scanner::scan_directory(base, None, stop_signal).unwrap();
    let mut app = ghostdu::ui::App::new(root);

    // Test a variety of terminal dimensions from wide to tiny, including 112 (the user's terminal size)
    let sizes = [
        (120, 30), // Widescreen
        (112, 30), // User reported cutoff width
        (105, 25), // Transition zone
        (95, 25),  // Pre-100 threshold
        (80, 24),  // Standard
        (70, 18),  // Medium
        (60, 14),  // Small
        (45, 10),  // Narrow & Short
        (35, 6),   // Tiny
        (20, 3),   // Extreme
    ];

    for &(w, h) in &sizes {
        let backend = ratatui::backend::TestBackend::new(w, h);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();

        // Filesystem View
        app.active_view = ghostdu::ui::ActiveView::Filesystem;
        let frame = terminal.draw(|f| ghostdu::ui::render_ui(f, &app)).unwrap();
        // Verify buffer has no "items items" anywhere
        for y in 0..h {
            let mut row = String::new();
            for x in 0..w {
                row.push_str(frame.buffer[(x, y)].symbol());
            }
            assert!(
                !row.contains("items items"),
                "Width {w} Row {y} contained duplicate 'items items': {row}"
            );
        }

        // Help Modal
        app.active_view = ghostdu::ui::ActiveView::HelpModal;
        terminal.draw(|f| ghostdu::ui::render_ui(f, &app)).unwrap();

        // Item Info Modal
        app.active_view = ghostdu::ui::ActiveView::ItemInfoModal;
        app.item_info = ghostdu::fs::get_detailed_item_info(&app.root_entry.path, 1);
        let frame_modal = terminal.draw(|f| ghostdu::ui::render_ui(f, &app)).unwrap();
        for y in 0..h {
            let mut row = String::new();
            for x in 0..w {
                row.push_str(frame_modal.buffer[(x, y)].symbol());
            }
            assert!(
                !row.contains("items items"),
                "Modal Width {w} Row {y} contained duplicate 'items items': {row}"
            );
        }

        // Ghost Inspector View
        app.active_view = ghostdu::ui::ActiveView::GhostInspector;
        terminal.draw(|f| ghostdu::ui::render_ui(f, &app)).unwrap();
    }
}

#[test]
fn test_category_taxonomy_scanning() {
    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let base = temp_dir.path();

    // Create subdirs and files representing various categories
    let trash_dir = base.join(".Trash-1000");
    fs::create_dir_all(&trash_dir).unwrap();
    fs::write(trash_dir.join("discarded.dat"), "trash content").unwrap();

    let node_modules_dir = base.join("node_modules");
    fs::create_dir_all(&node_modules_dir).unwrap();
    fs::write(node_modules_dir.join("index.js"), "module.exports = {}").unwrap();

    let target_dir = base.join("target");
    fs::create_dir_all(&target_dir).unwrap();
    fs::write(target_dir.join("build.rs"), "fn main() {}").unwrap();

    let log_file = base.join("system.log");
    fs::write(&log_file, "log line").unwrap();

    let ai_file = base.join("weights.safetensors");
    fs::write(&ai_file, "tensor weights").unwrap();

    let iso_file = base.join("installer.iso");
    fs::write(&iso_file, "iso header").unwrap();

    let normal_file = base.join("notes.txt");
    fs::write(&normal_file, "hello world").unwrap();

    let stop_signal = Arc::new(AtomicBool::new(false));
    let root = ghostdu_scanner::scan_directory(base, None, stop_signal).unwrap();

    let find_child = |name: &str| root.children.iter().find(|c| c.name == name).unwrap();

    assert_eq!(
        find_child(".Trash-1000").ghost_kind,
        ghostdu_scanner::GhostKind::Trash
    );
    assert_eq!(find_child(".Trash-1000").ghost_kind.badge(), "🗑️ TRASH");

    assert_eq!(
        find_child("node_modules").ghost_kind,
        ghostdu_scanner::GhostKind::DependencyTree
    );
    assert_eq!(find_child("node_modules").ghost_kind.badge(), "📦 DEPS");

    assert_eq!(
        find_child("target").ghost_kind,
        ghostdu_scanner::GhostKind::BuildCache
    );
    assert_eq!(find_child("target").ghost_kind.badge(), "👻 CACHE");

    assert_eq!(
        find_child("system.log").ghost_kind,
        ghostdu_scanner::GhostKind::LogFiles
    );
    assert_eq!(find_child("system.log").ghost_kind.badge(), "📜 LOGS");

    assert_eq!(
        find_child("weights.safetensors").ghost_kind,
        ghostdu_scanner::GhostKind::AiModel
    );
    assert_eq!(
        find_child("weights.safetensors").ghost_kind.badge(),
        "🤖 AI"
    );

    assert_eq!(
        find_child("installer.iso").ghost_kind,
        ghostdu_scanner::GhostKind::VmOrIso
    );
    assert_eq!(find_child("installer.iso").ghost_kind.badge(), "💿 VM/ISO");

    assert_eq!(
        find_child("notes.txt").ghost_kind,
        ghostdu_scanner::GhostKind::None
    );
    assert_eq!(find_child("notes.txt").ghost_kind.badge(), "");

    // Check Deletion Safety Tiers
    assert_eq!(
        find_child(".Trash-1000").delete_safety,
        ghostdu_scanner::DeleteSafety::Safe
    );
    assert_eq!(
        find_child("target").delete_safety,
        ghostdu_scanner::DeleteSafety::Safe
    );
    assert_eq!(
        find_child("system.log").delete_safety,
        ghostdu_scanner::DeleteSafety::Safe
    );
    assert_eq!(
        find_child("node_modules").delete_safety,
        ghostdu_scanner::DeleteSafety::Recheck
    );
    assert_eq!(
        find_child("weights.safetensors").delete_safety,
        ghostdu_scanner::DeleteSafety::Recheck
    );
    assert_eq!(
        find_child("installer.iso").delete_safety,
        ghostdu_scanner::DeleteSafety::Recheck
    );
    assert_eq!(
        find_child("notes.txt").delete_safety,
        ghostdu_scanner::DeleteSafety::UserData
    );

    // Verify safe reclaimable calculation
    assert!(root.safe_reclaimable_bytes() > 0);
    assert_eq!(root.safe_items_count(), 3); // .Trash-1000, target, system.log

    // Verify Safe-Only filter in App
    let mut app = ghostdu::ui::App::new(root);
    assert_eq!(app.visible_children().len(), 7);

    app.toggle_safe_filter();
    assert!(app.safe_only_filter);
    let safe_children = app.visible_children();
    assert_eq!(safe_children.len(), 3);
    for child in safe_children {
        assert_eq!(child.delete_safety, ghostdu_scanner::DeleteSafety::Safe);
    }
}

#[test]
fn test_system_deletion_guardrail() {
    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let safe_file = temp_dir.path().join("cache.tmp");
    fs::write(&safe_file, "cleanable").unwrap();

    let stop_signal = Arc::new(AtomicBool::new(false));
    let root = ghostdu_scanner::scan_directory(temp_dir.path(), None, stop_signal).unwrap();
    let mut app = ghostdu::ui::App::new(root);

    // 1. Trying to delete a critical system path (e.g. /etc or /usr) must be actively blocked!
    app.action_targets = vec![PathBuf::from("/etc")];
    app.action_total_size = 1024;
    let ghost = ghostdu_scanner::classify_path(&PathBuf::from("/etc"));
    let safety = ghostdu_scanner::classify_safety(&PathBuf::from("/etc"), ghost);
    assert_eq!(safety, ghostdu_scanner::DeleteSafety::System);

    app.action_has_system = true;
    app.action_safety_blocked = true;
    app.pending_action = Some(ghostdu::ui::ConfirmAction::PermanentDelete);

    // Attempting execution must NOT delete anything and must disarm
    app.execute_pending_action();
    assert_eq!(app.pending_action, None);
    assert!(app.status_message.as_ref().unwrap().0.contains("BLOCKED"));

    // Verify /etc is obviously still there
    assert!(PathBuf::from("/etc").exists());
}

#[test]
fn test_symlink_to_directory_deletion_preserves_target() {
    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let base = temp_dir.path();

    // Create target directory with file inside
    let target_dir = base.join("real_photos");
    fs::create_dir_all(&target_dir).unwrap();
    let photo_file = target_dir.join("photo.jpg");
    fs::write(&photo_file, "precious data").unwrap();

    // Create symlink to directory: ~/symlink_to_photos -> ~/real_photos
    let symlink_path = base.join("symlink_to_photos");
    std::os::unix::fs::symlink(&target_dir, &symlink_path).unwrap();

    // Confirm setup
    assert!(symlink_path.exists());
    assert!(symlink_path.is_dir()); // follows link!
    assert!(photo_file.exists());

    // Call permanently_delete on the SYMLINK
    let res = ghostdu_scanner::permanently_delete(std::slice::from_ref(&symlink_path));
    assert_eq!(res.succeeded.len(), 1);
    assert_eq!(res.succeeded[0], symlink_path);
    assert!(res.failed.is_empty());

    // The symlink must be gone
    assert!(!symlink_path.exists());
    // CRITICAL INVARIANT: The target directory and its contents MUST be 100% intact!
    assert!(target_dir.exists(), "Target directory must not be deleted!");
    assert!(
        photo_file.exists(),
        "Files inside target directory must not be deleted!"
    );
    assert_eq!(fs::read_to_string(&photo_file).unwrap(), "precious data");
}

#[test]
fn test_backend_deletion_safety_gate() {
    // 1. Direct call to permanently_delete with /etc must fail without touching disk
    let res = ghostdu_scanner::permanently_delete(&[PathBuf::from("/etc")]);
    assert!(res.succeeded.is_empty());
    assert_eq!(res.failed.len(), 1);
    assert!(res.failed[0].1.contains("Blocked: Protected system"));

    // 2. Direct call to move_to_trash with /usr must fail
    let res_trash = ghostdu_scanner::move_to_trash(&[PathBuf::from("/usr")]);
    assert!(res_trash.succeeded.is_empty());
    assert_eq!(res_trash.failed.len(), 1);
    assert!(res_trash.failed[0].1.contains("Blocked: Protected system"));

    // 3. Symlink pointing to /etc CAN be safely unlinked without touching /etc
    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let link_to_etc = temp_dir.path().join("my_link_to_etc");
    std::os::unix::fs::symlink("/etc", &link_to_etc).unwrap();

    let res_link = ghostdu_scanner::permanently_delete(std::slice::from_ref(&link_to_etc));
    assert_eq!(res_link.succeeded.len(), 1);
    assert!(!link_to_etc.exists());
    assert!(std::path::Path::new("/etc").exists());

    // 4. Non-symlink traversal path resolving to /etc is rejected by canonical check
    let sneaky_path = temp_dir.path().join("../../../../../../../../../etc");
    if sneaky_path.canonicalize().is_ok() {
        let res_sneaky = ghostdu_scanner::permanently_delete(&[sneaky_path]);
        assert!(res_sneaky.succeeded.is_empty());
        assert_eq!(res_sneaky.failed.len(), 1);
        assert!(res_sneaky.failed[0].1.contains("Blocked: Protected system"));
    }
}

#[test]
fn test_docker_system_df_parsing_fixture() {
    let canned_json = r#"{
        "Images": [
            {
                "Id": "sha256:dangling1234567890abcdef",
                "RepoTags": ["<none>:<none>"],
                "Size": 500000000,
                "Containers": 0
            },
            {
                "Id": "sha256:taggedused1234567890abcdef",
                "RepoTags": ["ubuntu:latest"],
                "Size": 700000000,
                "Containers": 2
            },
            {
                "Id": "sha256:taggedunused123456789abcdef",
                "RepoTags": ["alpine:3.19"],
                "Size": 10000000,
                "Containers": 0
            }
        ],
        "Containers": [
            {
                "Id": "c1",
                "Names": ["/my-stopped-app"],
                "SizeRw": 50000,
                "State": "exited",
                "Status": "Exited (0) 2 hours ago"
            },
            {
                "Id": "c2",
                "Names": ["/my-dead-container"],
                "SizeRw": 30000,
                "State": "dead",
                "Status": "Dead"
            },
            {
                "Id": "c3",
                "Names": ["/my-running-web"],
                "SizeRw": 90000,
                "State": "running",
                "Status": "Up 3 hours"
            }
        ],
        "Volumes": [
            {
                "Name": "dangling_vol",
                "UsageData": {
                    "Size": 120000000,
                    "RefCount": 0
                }
            }
        ],
        "BuildCache": [
            {
                "ID": "bc1",
                "Size": 200000000,
                "Reclaimable": true
            }
        ]
    }"#;

    let info = ghostdu_scanner::parse_docker_df_json(canned_json).expect("parse canned docker df");
    assert!(info.is_available);
    assert_eq!(info.images_count, 3);
    // Only the untagged image with containers == 0 is dangling (500 MB), NOT alpine (tagged)!
    assert_eq!(info.images_reclaimable_size, 500000000);
    // Containers: stopped (exited + dead) = 50000 + 30000 = 80000 bytes reclaimable
    assert_eq!(info.containers_count, 3);
    assert_eq!(info.containers_reclaimable_size, 80000);
    // Volume: 120 MB reclaimable
    assert_eq!(info.volumes_reclaimable_size, 120000000);
    // Build Cache: 200 MB reclaimable
    assert_eq!(info.build_cache_total_size, 200000000);
    assert_eq!(info.build_cache_reclaimable_size, 200000000);
}

#[test]
fn test_non_ascii_unicode_safety() {
    let non_ascii_inputs = [
        "café/résumé.txt",
        "📁 photos/🎉 party.png",
        "ドキュメント/日本語.md",
        "Здравствуйте/мир.log",
        "🚀🔥💻✨/data",
    ];

    for s in &non_ascii_inputs {
        let count = s.chars().count();
        for max_len in 0..count + 10 {
            // Slicing using char count and take should never panic
            if count > max_len && max_len > 3 {
                let head: String = s.chars().take(max_len - 3).collect();
                let display = format!("{}...", head);
                assert!(display.ends_with("..."));
            }
        }
    }
}

#[test]
fn test_scanner_cross_mount_option() {
    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let base = temp_dir.path();
    let sub = base.join("subdir");
    fs::create_dir_all(&sub).unwrap();
    fs::write(sub.join("file.txt"), "hello").unwrap();

    let stop_signal = Arc::new(AtomicBool::new(false));
    let options = ghostdu_scanner::ScannerOptions {
        cross_mounts: false,
    };
    let root =
        ghostdu_scanner::scan_directory_with_options(base, None, stop_signal, options).unwrap();
    assert_eq!(root.children.len(), 1);
}

#[test]
fn test_reclaimable_precomputed_aggregates_o1() {
    let temp_dir = tempfile::tempdir().expect("create temp dir");
    let base = temp_dir.path();
    let cache_dir = base.join(".cache").join("app");
    fs::create_dir_all(&cache_dir).unwrap();
    fs::write(cache_dir.join("cached.dat"), vec![0u8; 10000]).unwrap();
    let user_dir = base.join("Documents");
    fs::create_dir_all(&user_dir).unwrap();
    fs::write(user_dir.join("notes.txt"), "my notes").unwrap();

    let stop_signal = Arc::new(AtomicBool::new(false));
    let root = ghostdu_scanner::scan_directory(base, None, stop_signal).unwrap();

    // safe_reclaimable_bytes and safe_items_count are precomputed and O(1)
    assert!(root.safe_reclaimable_bytes() > 0);
    assert_eq!(root.safe_items_count(), 1); // .cache is 1 safe directory
}

#[test]
fn test_target_substring_safety_and_cargo_cache_roots() {
    // 1. Files inside an arbitrary path containing '/target/' must NOT be classified as BuildCache
    let doc_in_target = PathBuf::from("/home/user/Documents/target/financial_report.pdf");
    assert_ne!(
        ghostdu_scanner::classify_path(&doc_in_target),
        ghostdu_scanner::GhostKind::BuildCache
    );

    // 2. Exact cargo registry and git roots MUST be classified as BuildCache
    let cargo_registry = PathBuf::from("/home/user/.cargo/registry");
    let cargo_git = PathBuf::from("/home/user/.cargo/git");
    assert_eq!(
        ghostdu_scanner::classify_path(&cargo_registry),
        ghostdu_scanner::GhostKind::BuildCache
    );
    assert_eq!(
        ghostdu_scanner::classify_path(&cargo_git),
        ghostdu_scanner::GhostKind::BuildCache
    );
}

#[test]
fn test_unicode_width_truncation_with_wide_characters() {
    use unicode_width::UnicodeWidthStr;

    // String with 2-column wide characters (Japanese and Emoji)
    let wide_str = "🚀 こんにちは世界 📦";

    let trunc_end = ghostdu_scanner::truncate_end_by_width(wide_str, 12);
    assert!(trunc_end.width() <= 12);
    assert!(trunc_end.ends_with("..."));

    let trunc_start = ghostdu_scanner::truncate_start_by_width(wide_str, 12);
    assert!(trunc_start.width() <= 12);
    assert!(trunc_start.starts_with("..."));
}

#[test]
fn test_truncation_measures_complete_unicode_sequences() {
    use unicode_width::UnicodeWidthStr;

    let inputs = [
        "",
        "plain ASCII filename.txt",
        "日本語のファイル名.txt",
        "❤️❤️❤️❤️❤️❤️",
        "👩‍💻👩‍💻👩‍💻/notes.txt",
        "e\u{301}e\u{301}/notes.txt",
    ];
    for input in inputs {
        for max_width in 0..=input.width() + 3 {
            for truncate in [
                ghostdu_scanner::truncate_end_by_width,
                ghostdu_scanner::truncate_start_by_width,
            ] {
                let result = truncate(input, max_width);
                assert!(
                    result.width() <= max_width,
                    "{input:?} truncated to {max_width} cells produced {result:?} ({} cells)",
                    result.width()
                );
                if input.width() <= max_width {
                    assert_eq!(result, input);
                }
            }
        }
    }
}

#[test]
fn test_summary_columns_align_for_unicode_names() {
    use unicode_width::UnicodeWidthStr;

    let dir = tempfile::tempdir().unwrap();
    let names = [
        "ascii.txt".to_string(),
        "日本語.log".to_string(),
        "e\u{301}👩‍💻.txt".to_string(),
        "❤️".repeat(24),
        "界".repeat(24),
    ];
    for name in &names {
        fs::write(dir.path().join(name), "").unwrap();
    }
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ghostdu"))
        .arg("--summary")
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    let stdout = String::from_utf8(output.stdout).unwrap();
    let rows: Vec<_> = stdout
        .lines()
        .filter(|line| line.starts_with("     📄 "))
        .collect();
    assert_eq!(rows.len(), names.len());
    for row in rows {
        let size_start = row.find("0 B").unwrap();
        assert_eq!(row[..size_start].width(), 46, "{row}");
        let bar_start = row.find('[').unwrap();
        assert_eq!(row[..bar_start].width(), 59, "{row}");
        if let Some(category_start) = row.find("📜 LOGS") {
            assert_eq!(row[..category_start].width(), 79, "{row}");
        }
    }
}

#[test]
fn test_item_info_refresh_preserves_previous_view() {
    let mut root = ghostdu_scanner::FileEntry::new_dir(
        "root".to_string(),
        PathBuf::from("/test"),
        1,
        1,
        ghostdu_scanner::GhostKind::None,
        ghostdu_scanner::DeleteSafety::UserData,
    );
    root.children.push(ghostdu_scanner::FileEntry::new_file(
        "file.txt".to_string(),
        PathBuf::from("/test/file.txt"),
        100,
        1024,
        false,
        1,
        2,
        ghostdu_scanner::GhostKind::None,
        ghostdu_scanner::DeleteSafety::UserData,
    ));
    let mut app = ghostdu::ui::App::new(root);

    // Initial view: Filesystem
    assert_eq!(app.active_view, ghostdu::ui::ActiveView::Filesystem);

    // Open modal: previous_view becomes Filesystem, active_view becomes ItemInfoModal
    app.open_item_info();
    assert_eq!(app.active_view, ghostdu::ui::ActiveView::ItemInfoModal);
    assert_eq!(app.previous_view, ghostdu::ui::ActiveView::Filesystem);

    // Trigger 'r' event
    let key = crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('r'),
        crossterm::event::KeyModifiers::NONE,
    );
    ghostdu::ui::handle_key_event(&mut app, key);

    // CRITICAL BUG CHECK: previous_view MUST remain Filesystem, not get overwritten by ItemInfoModal!
    assert_eq!(app.previous_view, ghostdu::ui::ActiveView::Filesystem);

    // Now press 'q' or 'Esc' to close modal
    let esc_key = crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Esc,
        crossterm::event::KeyModifiers::NONE,
    );
    ghostdu::ui::handle_key_event(&mut app, esc_key);
    assert_eq!(app.active_view, ghostdu::ui::ActiveView::Filesystem);
}

// Minimal exposure for integration testing
mod ghostdu_scanner {
    pub use ghostdu::fs::entry::{
        truncate_end_by_width, truncate_start_by_width, DeleteSafety, FileEntry, GhostKind,
    };
    pub use ghostdu::fs::scanner::{scan_directory, scan_directory_with_options, ScannerOptions};
    pub use ghostdu::ghost::{classify_path, classify_safety, parse_docker_df_json};
    pub use ghostdu::ops::delete::permanently_delete;
    pub use ghostdu::ops::trash::move_to_trash;
}

#[test]
fn test_truncation_preserves_grapheme_boundaries() {
    use unicode_segmentation::UnicodeSegmentation;
    use unicode_width::UnicodeWidthStr;

    for cluster in ["e\u{301}", "👩‍💻", "❤️", "🇮🇩", "👍🏽"] {
        let input = cluster.repeat(6);
        for width in 0..=input.width() + 1 {
            for (truncate, from_start) in [
                (
                    ghostdu_scanner::truncate_end_by_width as fn(&str, usize) -> String,
                    false,
                ),
                (
                    ghostdu_scanner::truncate_start_by_width as fn(&str, usize) -> String,
                    true,
                ),
            ] {
                let output = truncate(&input, width);
                assert!(output.width() <= width);
                let retained = if from_start {
                    output.trim_start_matches('.')
                } else {
                    output.trim_end_matches('.')
                };
                assert!(
                    retained.graphemes(true).all(|g| g == cluster),
                    "{input:?} -> {output:?}"
                );
                if input.width() <= width {
                    assert_eq!(output, input);
                }
            }
        }
    }
    assert_eq!(
        ghostdu_scanner::truncate_start_by_width("xe\u{301}", 1),
        "e\u{301}"
    );
    assert_eq!(
        ghostdu_scanner::truncate_end_by_width("e\u{301}x", 1),
        "e\u{301}"
    );
}

#[test]
fn test_duplicate_safe_hardlink_stored_totals() {
    let dir = tempfile::tempdir().unwrap();
    let original = dir.path().join("original.log");
    fs::write(&original, vec![b'x'; 4096]).unwrap();
    fs::hard_link(&original, dir.path().join("alias.log")).unwrap();
    let root = ghostdu_scanner::scan_directory(dir.path(), None, Arc::new(AtomicBool::new(false)))
        .unwrap();
    let duplicate = root
        .children
        .iter()
        .find(|entry| entry.disk_usage == 0)
        .unwrap();
    let counted = root
        .children
        .iter()
        .find(|entry| entry.disk_usage != 0)
        .unwrap();
    assert_eq!((duplicate.safe_reclaimable, duplicate.safe_items), (0, 0));
    assert_eq!(duplicate.safe_items_count(), 0);
    assert_eq!(counted.safe_reclaimable, 0);
    assert_eq!(counted.safe_reclaimable_bytes(), 0);
    assert_eq!(counted.safe_items, 1);
    assert_eq!(root.safe_reclaimable, 0);
    assert_eq!(root.safe_items, 1);
}

#[test]
fn test_snap_archives_are_not_virtual_filesystems() {
    use ghostdu::ghost::is_virtual_fs_path;
    for path in ["/var/lib/snapd/snaps", "/var/lib/snapd/snaps/core_123.snap"] {
        assert!(!is_virtual_fs_path(std::path::Path::new(path)));
    }
    assert!(is_virtual_fs_path(std::path::Path::new(
        "/var/lib/snapd/mnt/core"
    )));
    assert!(is_virtual_fs_path(std::path::Path::new("/proc/1/stat")));
}

#[test]
fn test_external_hardlinks_do_not_inflate_safe_directory_savings() {
    let fixture = tempfile::tempdir().unwrap();
    let safe = fixture.path().join(".cache");
    fs::create_dir(&safe).unwrap();
    fs::write(safe.join("linked"), vec![b'x'; 8192]).unwrap();
    fs::hard_link(safe.join("linked"), fixture.path().join("outside")).unwrap();
    fs::write(safe.join("single"), vec![b'x'; 4096]).unwrap();
    let root =
        ghostdu_scanner::scan_directory(&safe, None, Arc::new(AtomicBool::new(false))).unwrap();
    let linked = root.children.iter().find(|e| e.name == "linked").unwrap();
    let single = root
        .children
        .iter()
        .find(|e| e.name == "single")
        .unwrap()
        .clone();
    assert!(linked.disk_usage > 0);
    assert_eq!(linked.reclaimable, 0);
    assert_eq!(root.safe_reclaimable_bytes(), single.disk_usage);
    assert_eq!(root.safe_reclaimable, single.disk_usage);
    // UI recomputation must preserve conservative estimates as subtrees change.
    let mut app = ghostdu::ui::App::new(root);
    app.replace_subtree(&safe.join("single"), single.clone());
    assert_eq!(app.root_entry.safe_reclaimable_bytes(), single.disk_usage);
}

#[test]
fn test_failed_deletion_reconciles_stale_tree() {
    let dir = tempfile::tempdir().unwrap();
    let removed = dir.path().join("removed");
    fs::write(&removed, "already gone").unwrap();
    fs::write(dir.path().join("remaining"), "keep").unwrap();
    let root = ghostdu_scanner::scan_directory(dir.path(), None, Arc::new(AtomicBool::new(false)))
        .unwrap();
    let mut app = ghostdu::ui::App::new(root);
    app.search_query = "removed".into();
    app.toggle_selection();
    app.prompt_permanent_delete();
    fs::remove_file(&removed).unwrap();
    app.search_query.clear();
    app.execute_pending_action();
    assert_eq!(app.root_entry.children.len(), 1);
    assert_eq!(app.root_entry.children[0].name, "remaining");
    assert!(app.selected_paths.is_empty());
    assert!(app.current_status().unwrap().contains("1 failed"));
}

#[test]
fn test_cleanup_preserves_navigation_when_preceding_sibling_is_removed() {
    for fail_one in [false, true] {
        let fixture = tempfile::tempdir().unwrap();
        for name in ["a", "b", "c"] {
            fs::create_dir(fixture.path().join(name)).unwrap();
        }
        let mut root =
            ghostdu_scanner::scan_directory(fixture.path(), None, Arc::new(AtomicBool::new(false)))
                .unwrap();
        root.children
            .sort_by(|left, right| left.name.cmp(&right.name));
        let current = fixture.path().join("b");
        let mut app = ghostdu::ui::App::new(root);
        app.sort_mode = ghostdu::ui::SortMode::ByName;
        app.toggle_selection(); // a
        if fail_one {
            app.cursor_to_end(); // c
            app.toggle_selection();
        }
        app.prompt_permanent_delete();
        if fail_one {
            fs::rename(fixture.path().join("c"), fixture.path().join("old-c")).unwrap();
            fs::create_dir(fixture.path().join("c")).unwrap();
        }
        assert!(app.navigate_to_path(&current));
        app.execute_pending_action();
        assert_eq!(app.current_dir_entry().path, current);
        assert!(!fixture.path().join("a").exists());
        assert!(fixture.path().join("c").exists());
        if fail_one {
            assert!(app.current_status().unwrap().contains("1 failed"));
        }
    }
}

#[test]
fn confirmation_rejects_replacement_files_directories_and_symlinks() {
    for trash in [false, true] {
        for kind in ["file", "directory", "symlink"] {
            let fixture = tempfile::tempdir().unwrap();
            let source = fixture.path().join("selected");
            match kind {
                "directory" => fs::create_dir(&source).unwrap(),
                "symlink" => std::os::unix::fs::symlink("missing-target", &source).unwrap(),
                _ => fs::write(&source, "original").unwrap(),
            }
            let root = ghostdu_scanner::scan_directory(
                fixture.path(),
                None,
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap();
            let mut app = ghostdu::ui::App::new(root);
            if trash {
                app.prompt_move_to_trash();
            } else {
                app.prompt_permanent_delete();
            }
            assert!(app.pending_action.is_some());
            fs::rename(&source, fixture.path().join("original")).unwrap();
            fs::write(&source, "replacement must survive").unwrap();
            app.execute_pending_action();
            assert_eq!(
                fs::read_to_string(&source).unwrap(),
                "replacement must survive"
            );
            assert!(fs::symlink_metadata(fixture.path().join("original")).is_ok());
            assert!(app.current_status().unwrap().contains("Target changed"));
        }
    }
}

#[test]
fn stale_selections_require_new_confirmation_and_refresh_discards_them() {
    for refresh in [false, true] {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("selected");
        fs::write(&source, "original").unwrap();
        let root =
            ghostdu_scanner::scan_directory(fixture.path(), None, Arc::new(AtomicBool::new(false)))
                .unwrap();
        let mut app = ghostdu::ui::App::new(root);
        app.toggle_selection();
        fs::rename(&source, fixture.path().join("original")).unwrap();
        fs::write(&source, "replacement").unwrap();
        if refresh {
            app.refresh_all();
        } else {
            app.prompt_permanent_delete();
            assert!(app.pending_action.is_none());
            assert!(app.current_status().unwrap().contains("Selection changed"));
        }
        assert!(app.selected_paths.is_empty());
        assert_eq!(fs::read_to_string(&source).unwrap(), "replacement");
    }
}

#[test]
fn failed_deletion_rescans_only_affected_subtrees_and_preserves_hardlink_totals() {
    let fixture = tempfile::tempdir().unwrap();
    let root_path = fixture.path().join("root");
    let affected = root_path.join("affected");
    let unrelated = root_path.join("unrelated");
    fs::create_dir_all(&affected).unwrap();
    fs::create_dir(&unrelated).unwrap();
    fs::write(affected.join("old"), "old payload").unwrap();
    let shared = unrelated.join("shared");
    fs::write(&shared, vec![b'x'; 8192]).unwrap();
    let root = ghostdu_scanner::scan_directory(&root_path, None, Arc::new(AtomicBool::new(false)))
        .unwrap();
    let expected_disk = root
        .children
        .iter()
        .find(|child| child.path == unrelated)
        .unwrap()
        .disk_usage;
    let mut app = ghostdu::ui::App::new(root);
    app.search_query = "affected".into();
    app.prompt_permanent_delete();
    fs::rename(&affected, fixture.path().join("original")).unwrap();
    fs::create_dir(&affected).unwrap();
    fs::hard_link(&shared, affected.join("shared-link")).unwrap();
    // A full-root scan would discover this. Reconciliation must leave it for a user refresh.
    fs::write(unrelated.join("not-part-of-reconciliation"), "new sibling").unwrap();
    app.execute_pending_action();
    let updated = app
        .root_entry
        .children
        .iter()
        .find(|child| child.path == affected)
        .unwrap();
    assert_eq!(updated.children.len(), 1);
    assert_eq!(updated.children[0].name, "shared-link");
    assert_eq!(updated.disk_usage, 0);
    let retained = app
        .root_entry
        .children
        .iter()
        .find(|child| child.path == unrelated)
        .unwrap();
    assert_eq!(retained.children.len(), 1);
    assert_eq!(app.root_entry.disk_usage, expected_disk);
    assert_eq!(app.root_entry.items_count, 5);
    assert!(app.current_status().unwrap().contains("1 failed"));
}

#[test]
fn replacement_between_scan_and_confirmation_or_selection_is_rejected() {
    let fixture = tempfile::tempdir().unwrap();
    let file_path = fixture.path().join("target.txt");
    fs::write(&file_path, "initial").unwrap();
    let root =
        ghostdu_scanner::scan_directory(fixture.path(), None, Arc::new(AtomicBool::new(false)))
            .unwrap();

    // Rename original file away so its inode remains allocated and cannot be recycled
    fs::rename(&file_path, fixture.path().join("original.txt")).unwrap();
    fs::write(&file_path, "replaced with different ino").unwrap();

    // 1. Attempting to select the replaced item must fail closed
    let mut app = ghostdu::ui::App::new(root.clone());
    app.toggle_selection();
    assert!(app.selected_paths.is_empty());
    assert!(app
        .current_status()
        .unwrap()
        .contains("Target changed since scan"));

    // 2. Attempting to confirm action on replaced cursor item must fail closed
    let mut app2 = ghostdu::ui::App::new(root);
    app2.prompt_permanent_delete();
    assert!(app2.pending_action.is_none());
    assert!(app2
        .current_status()
        .unwrap()
        .contains("Target changed since scan"));

    // 3. Verify nested subdirectory target replacement also traverses and fails closed
    let nested_dir = fixture.path().join("sub/nested");
    fs::create_dir_all(&nested_dir).unwrap();
    let nested_file = nested_dir.join("inner.txt");
    fs::write(&nested_file, "nested initial").unwrap();
    let root_nested =
        ghostdu_scanner::scan_directory(fixture.path(), None, Arc::new(AtomicBool::new(false)))
            .unwrap();

    fs::rename(&nested_file, nested_dir.join("inner_original.txt")).unwrap();
    fs::write(&nested_file, "nested replaced").unwrap();

    let mut app3 = ghostdu::ui::App::new(root_nested);
    assert!(app3.navigate_to_path(&nested_dir));
    app3.toggle_selection();
    assert!(app3.selected_paths.is_empty());
    assert!(app3
        .current_status()
        .unwrap()
        .contains("Target changed since scan"));
}

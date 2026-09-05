use std::fs::{self, File};
use std::io::Write;
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

    // Verify node_modules was classified as BuildCache
    let subdir = root_entry.children.iter().find(|c| c.name == "subdir").unwrap();
    let node_modules = subdir.children.iter().find(|c| c.name == "node_modules").unwrap();
    assert_eq!(node_modules.ghost_kind, ghostdu_scanner::GhostKind::BuildCache);
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

    let res = ghostdu_scanner::move_to_trash(&[file_path.clone()]);
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
    assert_eq!(app.pending_action, Some(ghostdu::ui::ConfirmAction::MoveToTrash));
    app.cancel_modal();
    assert_eq!(app.active_view, ghostdu::ui::ActiveView::Filesystem);
    assert_eq!(app.pending_action, None);

    app.prompt_permanent_delete();
    assert_eq!(app.active_view, ghostdu::ui::ActiveView::ConfirmModal);
    assert_eq!(app.pending_action, Some(ghostdu::ui::ConfirmAction::PermanentDelete));
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
    assert!(app.fs_info.is_some(), "app.fs_info should be loaded on init");
    app.open_item_info();
    assert_eq!(app.active_view, ghostdu::ui::ActiveView::ItemInfoModal);
    assert!(app.item_info.is_some());
    let item_info = app.item_info.as_ref().unwrap();
    assert_eq!(item_info.name, "dir_b");
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
    let sub_idx = app.visible_children().iter().position(|c| c.name == "subfolder").unwrap();
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

    // Test a variety of terminal dimensions from wide to tiny
    let sizes = [
        (120, 30), // Widescreen
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
        terminal.draw(|f| ghostdu::ui::render_ui(f, &app)).unwrap();

        // Help Modal
        app.active_view = ghostdu::ui::ActiveView::HelpModal;
        terminal.draw(|f| ghostdu::ui::render_ui(f, &app)).unwrap();

        // Item Info Modal
        app.active_view = ghostdu::ui::ActiveView::ItemInfoModal;
        app.item_info = ghostdu::fs::get_detailed_item_info(&app.root_entry.path, 1);
        terminal.draw(|f| ghostdu::ui::render_ui(f, &app)).unwrap();

        // Ghost Inspector View
        app.active_view = ghostdu::ui::ActiveView::GhostInspector;
        terminal.draw(|f| ghostdu::ui::render_ui(f, &app)).unwrap();
    }
}

// Minimal exposure for integration testing
mod ghostdu_scanner {
    pub use ghostdu::fs::entry::GhostKind;
    pub use ghostdu::fs::scanner::scan_directory;
    pub use ghostdu::ops::delete::permanently_delete;
    pub use ghostdu::ops::trash::move_to_trash;
}

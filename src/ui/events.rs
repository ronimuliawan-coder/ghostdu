use crate::ui::app::{ActiveView, App, ConfirmAction};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::path::PathBuf;

/// Copy text via Wayland then X11 clipboard tools. Reports the first tool
/// that accepts the input; errors only when none exists.
fn copy_to_clipboard(text: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::process::Stdio;
    let attempts = [
        ("wl-copy", Vec::<&str>::new()),
        ("xclip", vec!["-selection", "clipboard"]),
        ("xsel", vec!["--clipboard", "--input"]),
    ];
    for (tool, args) in attempts {
        if let Ok(mut child) = std::process::Command::new(tool)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            if child
                .stdin
                .take()
                .is_some_and(|mut stdin| stdin.write_all(text.as_bytes()).is_ok())
                && child.wait().is_ok_and(|status| status.success())
            {
                return Ok(());
            }
        }
    }
    Err(std::io::Error::other("no clipboard tool available"))
}

pub enum EventResult {
    Continue,
    Exit,
    RescanRequested,
    RescanPath(PathBuf),
    Subshell(PathBuf),
}

pub fn handle_key_event(app: &mut App, key: KeyEvent) -> EventResult {
    // Global interrupt: Ctrl+C always quits
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return EventResult::Exit;
    }

    match app.active_view {
        ActiveView::ConfirmModal => handle_confirm_keys(app, key),
        ActiveView::HelpModal => handle_help_keys(app, key),
        ActiveView::ItemInfoModal => handle_item_info_keys(app, key),
        ActiveView::GhostInspector => handle_ghost_keys(app, key),
        ActiveView::TopFiles => handle_top_files_keys(app, key),
        ActiveView::Janitor => handle_janitor_keys(app, key),
        ActiveView::Filesystem => handle_filesystem_keys(app, key),
    }
}

fn handle_confirm_keys(app: &mut App, key: KeyEvent) -> EventResult {
    // Scroll the frozen multi-target list without touching the confirmation.
    if app.action_targets.len() > 1 {
        match key.code {
            KeyCode::Up => {
                app.confirm_list_offset = app.confirm_list_offset.saturating_sub(1);
                return EventResult::Continue;
            }
            KeyCode::Down => {
                let max = app.action_targets.len().saturating_sub(1);
                if app.confirm_list_offset < max {
                    app.confirm_list_offset += 1;
                }
                return EventResult::Continue;
            }
            _ => {}
        }
    }
    // Process termination answers 1/2 instead of y/n. Execution consumes the
    // stored confirmation, so no fresh PID crosses this boundary.
    let killing = matches!(&app.pending_action, Some(ConfirmAction::KillProcess { .. }));
    if killing {
        match key.code {
            KeyCode::Char('1') => app.execute_kill(true),
            KeyCode::Char('2') => app.execute_kill(false),
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => app.cancel_modal(),
            _ => {}
        }
        return EventResult::Continue;
    }
    match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            if app.action_safety_blocked {
                app.set_status("⛔ Deletion blocked: Cannot delete critical system files!");
            } else {
                app.execute_pending_action();
            }
        }
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
            app.cancel_modal();
        }
        _ => {}
    }
    EventResult::Continue
}

fn handle_help_keys(app: &mut App, key: KeyEvent) -> EventResult {
    match key.code {
        KeyCode::Char('?') | KeyCode::Char('q') | KeyCode::Esc => {
            app.active_view = app.previous_view;
        }
        _ => {}
    }
    EventResult::Continue
}

fn handle_item_info_keys(app: &mut App, key: KeyEvent) -> EventResult {
    match key.code {
        KeyCode::Char('r') | KeyCode::Char('R') => {
            let previous_view = app.previous_view;
            app.open_item_info();
            app.previous_view = previous_view;
            app.set_status("Item details refreshed");
        }
        KeyCode::Char('i')
        | KeyCode::Char('I')
        | KeyCode::Char('q')
        | KeyCode::Esc
        | KeyCode::Enter => {
            app.active_view = app.previous_view;
        }
        _ => {}
    }
    EventResult::Continue
}

fn handle_ghost_keys(app: &mut App, key: KeyEvent) -> EventResult {
    match key.code {
        KeyCode::Tab | KeyCode::Char('g') | KeyCode::Esc => {
            app.active_view = ActiveView::Filesystem;
        }
        KeyCode::Char('1') | KeyCode::Left => {
            app.ghost_tab_index = 0;
            app.ghost_cursor_index = 0;
        }
        KeyCode::Char('2') | KeyCode::Right => {
            app.ghost_tab_index = 1;
            app.ghost_cursor_index = 0;
        }
        KeyCode::Char('j') | KeyCode::Down => {
            app.cursor_down();
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.cursor_up();
        }
        KeyCode::PageDown => {
            app.page_down(10);
        }
        KeyCode::PageUp => {
            app.page_up(10);
        }
        KeyCode::Home => {
            app.cursor_to_start();
        }
        KeyCode::End => {
            app.cursor_to_end();
        }
        KeyCode::Char('p') | KeyCode::Char('P') => {
            if app.ghost_tab_index == 0 && app.docker_info.is_available {
                // Requires a live Docker daemon; excluded from line coverage.
                #[cfg(not(tarpaulin_include))]
                #[allow(unexpected_cfgs)]
                app.prompt_docker_prune();
            }
        }
        // Terminate the highlighted ghost-table process (table PIDs only).
        KeyCode::Char('K') => {
            if app.ghost_tab_index == 1 {
                let target = app
                    .deleted_open_files
                    .get(app.ghost_cursor_index)
                    .map(|entry| entry.pid);
                if let Some(pid) = target {
                    app.prompt_kill_process(pid);
                }
            }
        }
        KeyCode::Char('r') | KeyCode::F(5) => {
            app.refresh_all();
        }
        KeyCode::Char('q') => {
            app.active_view = ActiveView::Filesystem;
        }
        KeyCode::Char('?') => {
            app.previous_view = app.active_view;
            app.active_view = ActiveView::HelpModal;
        }
        _ => {}
    }
    EventResult::Continue
}

fn handle_top_files_keys(app: &mut App, key: KeyEvent) -> EventResult {
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => {
            if !app.top_files.is_empty() {
                app.top_cursor = (app.top_cursor + 1).min(app.top_files.len() - 1);
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.top_cursor = app.top_cursor.saturating_sub(1);
        }
        KeyCode::Home => app.top_cursor = 0,
        KeyCode::End => {
            app.top_cursor = app.top_files.len().saturating_sub(1);
        }
        KeyCode::Enter => {
            if !app.jump_to_top_file() {
                app.set_status("Cannot jump: file no longer in tree");
            }
        }
        KeyCode::Char('t') => app.top_file_action(true),
        KeyCode::Char('d') | KeyCode::Char('D') => app.top_file_action(false),
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Tab | KeyCode::Char('g') => {
            app.active_view = ActiveView::Filesystem;
        }
        _ => {}
    }
    EventResult::Continue
}

fn handle_janitor_keys(app: &mut App, key: KeyEvent) -> EventResult {
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => {
            let rows = app.janitor_row_count();
            if rows > 0 {
                app.janitor_cursor = (app.janitor_cursor + 1).min(rows - 1);
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.janitor_cursor = app.janitor_cursor.saturating_sub(1);
        }
        KeyCode::Home => app.janitor_cursor = 0,
        KeyCode::End => {
            app.janitor_cursor = app.janitor_row_count().saturating_sub(1);
        }
        KeyCode::Right => {
            if let Some((cat_idx, item_idx)) = app.janitor_row_at(app.janitor_cursor) {
                if item_idx.is_none() {
                    app.janitor_cats[cat_idx].expanded = true;
                }
            }
        }
        KeyCode::Left => {
            if let Some((cat_idx, item_idx)) = app.janitor_row_at(app.janitor_cursor) {
                if item_idx.is_none() {
                    app.janitor_cats[cat_idx].expanded = false;
                }
            }
        }
        KeyCode::Char(' ') => app.toggle_janitor_row(),
        KeyCode::Char('a') => app.toggle_janitor_all(),
        KeyCode::Tab => app.toggle_janitor_scope(),
        KeyCode::Enter => app.janitor_action(true),
        KeyCode::Char('d') | KeyCode::Char('D') => app.janitor_action(false),
        KeyCode::Esc | KeyCode::Char('q') => {
            app.active_view = ActiveView::Filesystem;
        }
        _ => {}
    }
    EventResult::Continue
}

fn handle_filesystem_keys(app: &mut App, key: KeyEvent) -> EventResult {
    // 1. Text filter search mode
    if app.is_searching {
        match key.code {
            KeyCode::Enter => {
                app.is_searching = false;
            }
            KeyCode::Esc => {
                app.is_searching = false;
                app.search_query.clear();
            }
            KeyCode::Backspace => {
                app.search_query.pop();
            }
            KeyCode::Char(c) => {
                app.search_query.push(c);
            }
            _ => {}
        }
        return EventResult::Continue;
    }

    // Ctrl shortcuts
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('d') => {
                app.page_down(15);
                return EventResult::Continue;
            }
            KeyCode::Char('u') => {
                app.page_up(15);
                return EventResult::Continue;
            }
            _ => {}
        }
    }

    // 2. Normal filesystem navigation
    match key.code {
        KeyCode::Char('q') => EventResult::Exit,

        // Navigation
        KeyCode::Char('j') | KeyCode::Down => {
            app.cursor_down();
            EventResult::Continue
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.cursor_up();
            EventResult::Continue
        }
        KeyCode::PageDown => {
            app.page_down(15);
            EventResult::Continue
        }
        KeyCode::PageUp => {
            app.page_up(15);
            EventResult::Continue
        }
        KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => {
            app.enter_selected();
            EventResult::Continue
        }
        KeyCode::Backspace | KeyCode::Char('h') | KeyCode::Left => {
            if let Some(parent) = app.go_up() {
                EventResult::RescanPath(parent)
            } else {
                EventResult::Continue
            }
        }
        KeyCode::Home => {
            app.cursor_to_start();
            EventResult::Continue
        }
        KeyCode::End => {
            app.cursor_to_end();
            EventResult::Continue
        }
        KeyCode::Char('\\') => EventResult::RescanPath(PathBuf::from("/")),
        KeyCode::Char('~') => {
            // Without HOME (minimal containers), stay where we are instead
            // of a dead key: one path, no environment-dependent branch.
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            EventResult::RescanPath(PathBuf::from(home))
        }

        // Selection
        KeyCode::Char(' ') => {
            app.toggle_selection();
            EventResult::Continue
        }
        KeyCode::Char('a') => {
            app.select_all_visible();
            EventResult::Continue
        }
        KeyCode::Char('A') => {
            app.apparent_size = !app.apparent_size;
            EventResult::Continue
        }

        // Wastebin & Permanent Deletion
        KeyCode::Char('t') | KeyCode::Char('w') => {
            app.prompt_move_to_trash();
            EventResult::Continue
        }
        KeyCode::Char('d') | KeyCode::Char('D') => {
            app.prompt_permanent_delete();
            EventResult::Continue
        }

        // Ghost & Docker
        KeyCode::Tab | KeyCode::Char('g') => {
            app.active_view = ActiveView::GhostInspector;
            EventResult::Continue
        }
        KeyCode::Char('G') => {
            app.ghost_filter = app.ghost_filter.next();
            app.set_status(format!("Filter: {}", app.ghost_filter.label()));
            EventResult::Continue
        }
        KeyCode::Char('c') | KeyCode::Char('C') => {
            app.toggle_safe_filter();
            EventResult::Continue
        }

        // Sorting & Searching
        KeyCode::Char('s') => {
            app.sort_mode = app.sort_mode.next();
            app.set_status(format!("Sorting by {}", app.sort_mode.label()));
            EventResult::Continue
        }
        KeyCode::Char('/') => {
            app.is_searching = true;
            app.search_query.clear();
            EventResult::Continue
        }
        KeyCode::Esc => {
            if !app.search_query.is_empty() {
                app.search_query.clear();
            }
            EventResult::Continue
        }

        // Utilities
        KeyCode::Char('i') | KeyCode::Char('I') => {
            app.open_item_info();
            EventResult::Continue
        }
        KeyCode::Char('T') => {
            app.open_top_files();
            EventResult::Continue
        }
        KeyCode::Char('J') => {
            app.open_janitor();
            EventResult::Continue
        }
        // Shell & desktop integration: suspend for a subshell, or fire-and-forget.
        KeyCode::Char('!') => {
            let dir = app
                .visible_children()
                .get(app.cursor_index)
                .map(|e| {
                    if e.is_dir && !e.is_symlink {
                        e.path.clone()
                    } else {
                        e.path
                            .parent()
                            .map(|p| p.to_path_buf())
                            .unwrap_or_else(|| e.path.clone())
                    }
                })
                .unwrap_or_else(|| app.current_dir_entry().path.clone());
            EventResult::Subshell(dir)
        }
        KeyCode::Char('o') | KeyCode::Char('O') => {
            if let Some(target) = app
                .visible_children()
                .get(app.cursor_index)
                .map(|e| e.path.clone())
            {
                use std::process::Stdio;
                match std::process::Command::new("xdg-open")
                    .arg(&target)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                {
                    // Detach output so child warnings cannot corrupt the TUI;
                    // reap on a thread so no zombie is left behind.
                    Ok(mut child) => {
                        std::thread::spawn(move || {
                            let _ = child.wait();
                        });
                        app.set_status(format!("Opened {}", target.display()))
                    }
                    Err(error) => app.set_status(format!("Cannot open: {error}")),
                }
            }
            EventResult::Continue
        }
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            if let Some(target) = app
                .visible_children()
                .get(app.cursor_index)
                .map(|e| e.path.clone())
            {
                match copy_to_clipboard(&target.to_string_lossy()) {
                    Ok(_) => app.set_status(format!("Copied: {}", target.display())),
                    Err(_) => app.set_status("No clipboard tool (wl-copy/xclip/xsel)"),
                }
            }
            EventResult::Continue
        }
        KeyCode::Char('r') => {
            app.refresh_all();
            EventResult::Continue
        }
        KeyCode::Char('R') | KeyCode::F(5) => EventResult::RescanRequested,
        KeyCode::Char('?') => {
            app.previous_view = app.active_view;
            app.active_view = ActiveView::HelpModal;
            EventResult::Continue
        }

        _ => EventResult::Continue,
    }
}

#[cfg(test)]
mod key_matrix_tests {
    use super::*;
    use crate::fs::scanner::scan_directory;
    use std::sync::{atomic::AtomicBool, Arc};

    pub(crate) struct Fixture {
        _dir: tempfile::TempDir,
        pub(crate) app: App,
    }

    pub(crate) fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(dir.path().join("a.txt"), "aaaaaaaaaa").unwrap();
        std::fs::write(sub.join("b.txt"), "b").unwrap();
        let root = scan_directory(dir.path(), None, Arc::new(AtomicBool::new(false))).unwrap();
        Fixture {
            app: App::new(root),
            _dir: dir,
        }
    }

    pub(crate) fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    pub(crate) fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    #[test]
    fn filesystem_navigation_keys() {
        let mut fx = fixture();
        let app = &mut fx.app;
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('j'))),
            EventResult::Continue
        ));
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Down)),
            EventResult::Continue
        ));
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('k'))),
            EventResult::Continue
        ));
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Up)),
            EventResult::Continue
        ));
        assert!(matches!(
            handle_key_event(app, key(KeyCode::PageDown)),
            EventResult::Continue
        ));
        assert!(matches!(
            handle_key_event(app, key(KeyCode::PageUp)),
            EventResult::Continue
        ));
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Home)),
            EventResult::Continue
        ));
        assert!(matches!(
            handle_key_event(app, key(KeyCode::End)),
            EventResult::Continue
        ));
        // Enter the subdirectory, then go back up.
        app.cursor_index = app
            .visible_children()
            .iter()
            .position(|e| e.name == "sub")
            .unwrap();
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Enter)),
            EventResult::Continue
        ));
        assert!(!app.path_stack.is_empty());
        app.cursor_index = 0;
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('l'))),
            EventResult::Continue
        ));
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Right)),
            EventResult::Continue
        ));
        // Leaving the subdirectory pops it internally (Continue); at the scan
        // root the parent path is returned for a rescan.
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Backspace)),
            EventResult::Continue
        ));
        assert!(app.path_stack.is_empty());
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Backspace)),
            EventResult::RescanPath(_)
        ));
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('h'))),
            EventResult::Continue | EventResult::RescanPath(_)
        ));
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Left)),
            EventResult::Continue | EventResult::RescanPath(_)
        ));
        // Root and home shortcuts return rescan paths.
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('\\'))),
            EventResult::RescanPath(_)
        ));
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('~'))),
            EventResult::RescanPath(_) | EventResult::Continue
        ));
        assert!(matches!(
            handle_key_event(app, ctrl(KeyCode::Char('d'))),
            EventResult::Continue
        ));
        assert!(matches!(
            handle_key_event(app, ctrl(KeyCode::Char('u'))),
            EventResult::Continue
        ));
        assert!(matches!(
            handle_key_event(app, ctrl(KeyCode::Char('x'))),
            EventResult::Continue
        ));
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('q'))),
            EventResult::Exit
        ));
        assert!(matches!(
            handle_key_event(app, ctrl(KeyCode::Char('c'))),
            EventResult::Exit
        ));
    }
}

#[cfg(test)]
mod key_action_tests {
    use super::key_matrix_tests::{ctrl, fixture, key};
    use super::*;

    #[test]
    fn filesystem_selection_and_filters() {
        let mut fx = fixture();
        let app = &mut fx.app;
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char(' '))),
            EventResult::Continue
        ));
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('a'))),
            EventResult::Continue
        ));
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('A'))),
            EventResult::Continue
        ));
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Tab)),
            EventResult::Continue
        ));
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('g'))),
            EventResult::Continue
        ));
        app.active_view = ActiveView::Filesystem;
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('G'))),
            EventResult::Continue
        ));
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('c'))),
            EventResult::Continue
        ));
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('C'))),
            EventResult::Continue
        ));
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('s'))),
            EventResult::Continue
        ));
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('T'))),
            EventResult::Continue
        ));
        assert_eq!(app.active_view, ActiveView::TopFiles);
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Esc)),
            EventResult::Continue
        ));
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('J'))),
            EventResult::Continue
        ));
        assert_eq!(app.active_view, ActiveView::Janitor);
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Esc)),
            EventResult::Continue
        ));
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('?'))),
            EventResult::Continue
        ));
        assert_eq!(app.active_view, ActiveView::HelpModal);
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Esc)),
            EventResult::Continue
        ));
    }

    #[test]
    fn filesystem_prompts_and_search() {
        let mut fx = fixture();
        let app = &mut fx.app;
        // Trash + delete prompts open confirm modals without executing.
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('t'))),
            EventResult::Continue
        ));
        assert_eq!(app.active_view, ActiveView::ConfirmModal);
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Esc)),
            EventResult::Continue
        ));
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('w'))),
            EventResult::Continue
        ));
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Esc)),
            EventResult::Continue
        ));
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('d'))),
            EventResult::Continue
        ));
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Esc)),
            EventResult::Continue
        ));
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('D'))),
            EventResult::Continue
        ));
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Esc)),
            EventResult::Continue
        ));
        // Item info modal and its keys.
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('i'))),
            EventResult::Continue
        ));
        assert_eq!(app.active_view, ActiveView::ItemInfoModal);
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('r'))),
            EventResult::Continue
        ));
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('I'))),
            EventResult::Continue
        ));
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Enter)),
            EventResult::Continue
        ));
        assert_eq!(app.active_view, ActiveView::Filesystem);
        // Search mode captures typing, Enter/Escape leave it.
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('/'))),
            EventResult::Continue
        ));
        assert!(app.is_searching);
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('x'))),
            EventResult::Continue
        ));
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Backspace)),
            EventResult::Continue
        ));
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Enter)),
            EventResult::Continue
        ));
        assert!(!app.is_searching);
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('/'))),
            EventResult::Continue
        ));
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('q'))),
            EventResult::Continue
        ));
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Esc)),
            EventResult::Continue
        ));
        assert!(!app.is_searching);
        // Escape with a live query keeps the query but leaves search mode.
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('/'))),
            EventResult::Continue
        ));
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('z'))),
            EventResult::Continue
        ));
        assert!(!app.search_query.is_empty());
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Esc)),
            EventResult::Continue
        ));
        assert!(!app.is_searching);
        // Leaving search keeps the query as a filter; Esc in the filesystem
        // view then clears that lingering query.
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('/'))),
            EventResult::Continue
        ));
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('z'))),
            EventResult::Continue
        ));
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Enter)),
            EventResult::Continue
        ));
        assert!(!app.search_query.is_empty());
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Esc)),
            EventResult::Continue
        ));
        assert!(app.search_query.is_empty());
        // Empty view: subshell and actions fall back without a highlighted row.
        app.search_query.clear();
        app.is_searching = false;
        app.cursor_index = app
            .visible_children()
            .iter()
            .position(|e| e.is_dir)
            .unwrap();
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('!'))),
            EventResult::Subshell(_)
        ));
        app.cursor_index = app
            .visible_children()
            .iter()
            .position(|e| !e.is_dir)
            .unwrap();
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('!'))),
            EventResult::Subshell(_)
        ));
        app.search_query = "zzz-no-match".to_string();
        app.is_searching = false;
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('!'))),
            EventResult::Subshell(_)
        ));
        // Refresh paths: in-place rescan and full rescan request.
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('r'))),
            EventResult::Continue
        ));
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('R'))),
            EventResult::RescanRequested
        ));
        assert!(matches!(
            handle_key_event(app, key(KeyCode::F(5))),
            EventResult::RescanRequested
        ));
        // Subshell + open + copy return without running anything here.
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('!'))),
            EventResult::Subshell(_)
        ));
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('o'))),
            EventResult::Continue
        ));
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('O'))),
            EventResult::Continue
        ));
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('y'))),
            EventResult::Continue
        ));
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('Y'))),
            EventResult::Continue
        ));
    }
}

#[cfg(test)]
mod key_view_tests {
    use super::key_matrix_tests::{fixture, key};
    use super::*;

    #[test]
    fn ghost_inspector_keys() {
        let mut fx = fixture();
        let app = &mut fx.app;
        app.active_view = ActiveView::GhostInspector;
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Tab)),
            EventResult::Continue
        ));
        assert_eq!(app.active_view, ActiveView::Filesystem);
        app.active_view = ActiveView::GhostInspector;
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('g'))),
            EventResult::Continue
        ));
        app.active_view = ActiveView::GhostInspector;
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Esc)),
            EventResult::Continue
        ));
        app.active_view = ActiveView::GhostInspector;
        for code in [
            KeyCode::Char('1'),
            KeyCode::Char('2'),
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Char('j'),
            KeyCode::Down,
            KeyCode::Char('k'),
            KeyCode::Up,
            KeyCode::PageDown,
            KeyCode::PageUp,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::Char('p'),
            KeyCode::Char('P'),
            KeyCode::Char('K'),
            KeyCode::Char('r'),
            KeyCode::F(5),
            KeyCode::Char('q'),
            KeyCode::Char('?'),
        ] {
            app.active_view = ActiveView::GhostInspector;
            assert!(
                matches!(handle_key_event(app, key(code)), EventResult::Continue),
                "ghost key {code:?}"
            );
        }
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('q'))),
            EventResult::Continue
        ));
    }

    #[test]
    fn top_files_and_janitor_keys() {
        let mut fx = fixture();
        let app = &mut fx.app;
        app.open_top_files();
        for code in [
            KeyCode::Char('j'),
            KeyCode::Down,
            KeyCode::Char('k'),
            KeyCode::Up,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::Char('t'),
            KeyCode::Char('d'),
            KeyCode::Esc,
        ] {
            app.active_view = ActiveView::TopFiles;
            assert!(
                matches!(handle_key_event(app, key(code)), EventResult::Continue),
                "top key {code:?}"
            );
        }
        // Enter jumps back to the explorer; q exits from there.
        app.active_view = ActiveView::TopFiles;
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Enter)),
            EventResult::Continue
        ));
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Tab)),
            EventResult::Continue
        ));
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('g'))),
            EventResult::Continue
        ));

        app.open_janitor();
        for code in [
            KeyCode::Char('j'),
            KeyCode::Down,
            KeyCode::Char('k'),
            KeyCode::Up,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::Right,
            KeyCode::Left,
            KeyCode::Char(' '),
            KeyCode::Char('a'),
            KeyCode::Tab,
            KeyCode::Enter,
            KeyCode::Char('d'),
            KeyCode::Char('D'),
            KeyCode::Esc,
        ] {
            app.active_view = ActiveView::Janitor;
            assert!(
                matches!(handle_key_event(app, key(code)), EventResult::Continue),
                "janitor key {code:?}"
            );
        }
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('q'))),
            EventResult::Exit
        ));
        app.active_view = ActiveView::Janitor;
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('q'))),
            EventResult::Continue
        ));
        assert_eq!(app.active_view, ActiveView::Filesystem);
    }

    #[test]
    fn confirm_and_help_keys() {
        let mut fx = fixture();
        let app = &mut fx.app;
        // Plain y confirm round-trip on a trash prompt (executes for real).
        app.prompt_move_to_trash();
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('y'))),
            EventResult::Continue
        ));
        app.prompt_move_to_trash();
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('n'))),
            EventResult::Continue
        ));
        app.prompt_move_to_trash();
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('N'))),
            EventResult::Continue
        ));
        // Blocked-system path: y reports instead of executing.
        app.action_safety_blocked = true;
        app.pending_action = Some(ConfirmAction::MoveToTrash);
        app.active_view = ActiveView::ConfirmModal;
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('y'))),
            EventResult::Continue
        ));
        app.cancel_modal();
        // Up/Down scroll a multi-target list; other keys fall through.
        app.prompt_move_to_trash();
        app.action_targets.extend([
            std::path::PathBuf::from("/x"),
            std::path::PathBuf::from("/y"),
        ]);
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Down)),
            EventResult::Continue
        ));
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Up)),
            EventResult::Continue
        ));
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Up)),
            EventResult::Continue
        ));
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('z'))),
            EventResult::Continue
        ));
        app.cancel_modal();
        // Kill confirmation answers 1/2; unknown PIDs refuse safely.
        app.pending_action = Some(ConfirmAction::KillProcess {
            pid: 2_147_000_000,
            name: "x".to_string(),
            start_time: 0,
            pidfd: None,
        });
        app.active_view = ActiveView::ConfirmModal;
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('1'))),
            EventResult::Continue
        ));
        app.pending_action = Some(ConfirmAction::KillProcess {
            pid: 2_147_000_000,
            name: "x".to_string(),
            start_time: 0,
            pidfd: None,
        });
        app.active_view = ActiveView::ConfirmModal;
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('2'))),
            EventResult::Continue
        ));
        // Kill-modal cancel paths dismiss without signalling.
        app.pending_action = Some(ConfirmAction::KillProcess {
            pid: 2_147_000_000,
            name: "x".to_string(),
            start_time: 0,
            pidfd: None,
        });
        app.active_view = ActiveView::ConfirmModal;
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('n'))),
            EventResult::Continue
        ));
        assert!(app.pending_action.is_none());
        // Help modal closes on ?/q/Esc.
        app.active_view = ActiveView::HelpModal;
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('?'))),
            EventResult::Continue
        ));
        app.active_view = ActiveView::HelpModal;
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('q'))),
            EventResult::Continue
        ));
        // Item info modal close paths.
        app.open_item_info();
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Char('q'))),
            EventResult::Continue
        ));
        app.open_item_info();
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Esc)),
            EventResult::Continue
        ));
        app.open_item_info();
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Enter)),
            EventResult::Continue
        ));
    }
}

#[cfg(test)]
mod key_jump_tests {
    use super::key_matrix_tests::{fixture, key};
    use super::*;

    #[test]
    fn jump_to_vanished_parent_reports_status() {
        let mut fx = fixture();
        let app = &mut fx.app;
        app.open_top_files();
        // A row no live tree path can resolve: navigation fails cleanly.
        app.top_files.push(crate::ui::app::TopFile {
            path: std::path::PathBuf::from("/nonexistent-xyz/deep/file.txt"),
            name: "file.txt".to_string(),
            size: 1,
            disk_usage: 1,
            safety: crate::fs::entry::DeleteSafety::UserData,
        });
        app.top_cursor = app.top_files.len() - 1;
        app.active_view = ActiveView::TopFiles;
        assert!(matches!(
            handle_key_event(app, key(KeyCode::Enter)),
            EventResult::Continue
        ));
        assert!(app
            .current_status()
            .unwrap_or("")
            .contains("no longer in tree"));
    }
}

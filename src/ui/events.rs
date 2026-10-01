use crate::ui::app::{ActiveView, App};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::path::PathBuf;

pub enum EventResult {
    Continue,
    Exit,
    RescanRequested,
    RescanPath(PathBuf),
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
        ActiveView::Filesystem => handle_filesystem_keys(app, key),
    }
}

fn handle_confirm_keys(app: &mut App, key: KeyEvent) -> EventResult {
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
                app.prompt_docker_prune();
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
            if let Ok(home) = std::env::var("HOME") {
                EventResult::RescanPath(PathBuf::from(home))
            } else {
                EventResult::Continue
            }
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

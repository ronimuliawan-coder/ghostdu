use crate::fs::entry::{format_count, format_size, GhostKind};
use crate::ui::app::{ActiveView, App, ConfirmAction, GhostFilterMode};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Borders, Clear, Paragraph, Row, Scrollbar, ScrollbarOrientation,
        ScrollbarState, Table, Tabs, Wrap,
    },
    Frame,
};

pub fn render_ui(f: &mut Frame, app: &App) {
    let size = f.area();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Header
            Constraint::Min(5),    // Main content
            Constraint::Length(1), // Footer / Keybindings
        ])
        .split(size);

    render_header(f, app, chunks[0]);

    match app.active_view {
        ActiveView::Filesystem => render_filesystem_view(f, app, chunks[1]),
        ActiveView::GhostInspector => render_ghost_inspector(f, app, chunks[1]),
        ActiveView::ConfirmModal => {
            // Render underlying view then overlay modal
            if app.previous_view == ActiveView::GhostInspector {
                render_ghost_inspector(f, app, chunks[1]);
            } else {
                render_filesystem_view(f, app, chunks[1]);
            }
            render_confirm_modal(f, app, size);
        }
        ActiveView::HelpModal => {
            render_filesystem_view(f, app, chunks[1]);
            render_help_modal(f, size);
        }
    }

    render_footer(f, app, chunks[2]);
}

fn render_header(f: &mut Frame, app: &App, area: Rect) {
    let current_dir = app.current_dir_entry();
    let current_path = current_dir.path.to_string_lossy();
    let dir_size = current_dir.display_size(app.apparent_size);
    let size_str = format_size(dir_size);
    let count_str = format_count(current_dir.items_count);

    let (sel_count, sel_size) = app.selection_summary();
    let selection_span = if sel_count > 0 {
        Span::styled(
            format!(" [★ {} sel: {}]", sel_count, format_size(sel_size)),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::raw("")
    };

    let filter_span = match app.ghost_filter {
        GhostFilterMode::ShowAll => Span::styled(" [Ghost: All]", Style::default().fg(Color::DarkGray)),
        GhostFilterMode::HideGhost => {
            Span::styled(" [Ghost: Hidden]", Style::default().fg(Color::Green))
        }
        GhostFilterMode::GhostOnly => {
            Span::styled(" [Ghost: ONLY]", Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD))
        }
    };

    let size_mode_span = if app.apparent_size {
        Span::styled(" [Apparent]", Style::default().fg(Color::Cyan))
    } else {
        Span::styled(" [Disk Block]", Style::default().fg(Color::Blue))
    };

    let view_tab_span = if app.active_view == ActiveView::GhostInspector {
        Span::styled(" [👻 GHOST INSPECTOR]", Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD))
    } else {
        Span::styled(" [📂 EXPLORER]", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
    };

    let title_line = Line::from(vec![
        Span::styled("👻 ghostdu ", Style::default().fg(Color::LightCyan).add_modifier(Modifier::BOLD)),
        view_tab_span,
        Span::raw(" │ "),
        Span::styled(format!("📁 {}", current_path), Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
    ]);

    let subtitle_line = Line::from(vec![
        Span::styled(format!("Size: {}", size_str), Style::default().fg(Color::Green)),
        Span::raw(" │ "),
        Span::styled(format!("Items: {}", count_str), Style::default().fg(Color::Gray)),
        Span::raw(" │ "),
        Span::styled(format!("Sort: {}", app.sort_mode.label()), Style::default().fg(Color::LightYellow)),
        filter_span,
        size_mode_span,
        selection_span,
    ]);

    let header_widget = Paragraph::new(vec![title_line, subtitle_line])
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(Color::DarkGray)),
        );

    f.render_widget(header_widget, area);
}

fn compute_scroll_window(
    cursor: usize,
    current_offset: usize,
    viewport_height: usize,
    total_items: usize,
) -> usize {
    if total_items == 0 || viewport_height == 0 {
        return 0;
    }
    let mut offset = current_offset;
    if cursor < offset {
        offset = cursor;
    } else if cursor >= offset + viewport_height {
        offset = cursor + 1 - viewport_height;
    }
    if offset > total_items.saturating_sub(viewport_height) {
        offset = total_items.saturating_sub(viewport_height);
    }
    offset
}

fn render_filesystem_view(f: &mut Frame, app: &App, area: Rect) {
    let visible = app.visible_children();
    let current_dir = app.current_dir_entry();
    let parent_size = current_dir.display_size(app.apparent_size).max(1);

    // If searching, reserve 1 line at the bottom for search bar
    let (table_area, search_area) = if app.is_searching || !app.search_query.is_empty() {
        let split = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(3), Constraint::Length(1)])
            .split(area);
        (split[0], Some(split[1]))
    } else {
        (area, None)
    };

    let total_items = visible.len();
    let viewport_height = table_area.height.saturating_sub(4).max(1) as usize;
    let scroll_offset = compute_scroll_window(
        app.cursor_index,
        app.scroll_offset.get(),
        viewport_height,
        total_items,
    );
    app.scroll_offset.set(scroll_offset);

    let rows: Vec<Row> = visible
        .iter()
        .enumerate()
        .skip(scroll_offset)
        .take(viewport_height)
        .map(|(idx, entry)| {
            let is_cursor = idx == app.cursor_index;
            let is_selected = app.selected_paths.contains(&entry.path);

            let cursor_str = if is_cursor { "▶" } else { " " };
            let sel_str = if is_selected { " [*]" } else { " [ ]" };

            let marker_span = Span::styled(
                format!("{}{}", cursor_str, sel_str),
                if is_selected {
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
                } else if is_cursor {
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::DarkGray)
                },
            );

            // Icon + Name
            let icon = if entry.is_symlink {
                "🔗 "
            } else if entry.is_dir {
                "📁 "
            } else {
                "📄 "
            };

            let name_style = if entry.has_err {
                Style::default().fg(Color::Red)
            } else if entry.is_dir {
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };

            let name_span = Span::styled(format!("{}{}", icon, entry.name), name_style);

            // Ghost/Docker Badge
            let badge_span = match entry.ghost_kind {
                GhostKind::DockerOverlay
                | GhostKind::DockerVolume
                | GhostKind::DockerContainer
                | GhostKind::DockerBuildkit
                | GhostKind::DockerUser => {
                    Span::styled(entry.ghost_kind.badge(), Style::default().fg(Color::LightBlue).add_modifier(Modifier::BOLD))
                }
                GhostKind::PodmanUser => {
                    Span::styled("🦭 PODMAN", Style::default().fg(Color::Magenta))
                }
                GhostKind::BuildCache => {
                    Span::styled("👻 CACHE", Style::default().fg(Color::LightMagenta))
                }
                GhostKind::PackageCache => {
                    Span::styled("📦 PKG", Style::default().fg(Color::LightYellow))
                }
                GhostKind::DeletedOpen => {
                    Span::styled("👻 GHOST", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))
                }
                GhostKind::None => {
                    if entry.has_err {
                        Span::styled("[!] LOCKED", Style::default().fg(Color::Red))
                    } else {
                        Span::raw("")
                    }
                }
            };

            // Size with color gradient
            let entry_size = entry.display_size(app.apparent_size);
            let size_style = if entry_size >= 10 * 1024 * 1024 * 1024 {
                Style::default().fg(Color::LightRed).add_modifier(Modifier::BOLD)
            } else if entry_size >= 1024 * 1024 * 1024 {
                Style::default().fg(Color::LightYellow)
            } else if entry_size >= 100 * 1024 * 1024 {
                Style::default().fg(Color::LightGreen)
            } else {
                Style::default().fg(Color::Cyan)
            };

            let size_str = format_size(entry_size);
            let size_span = Span::styled(size_str, size_style);

            // Proportional Bar Graph
            let percent = ((entry_size as f64 / parent_size as f64) * 100.0).clamp(0.0, 100.0);
            let bar_len = 10;
            let filled_len = ((percent / 100.0) * bar_len as f64).round() as usize;
            let bar_filled = "█".repeat(filled_len.min(bar_len));
            let bar_empty = "░".repeat(bar_len.saturating_sub(filled_len));
            let bar_text = format!("[{}{}] {:>5.1}%", bar_filled, bar_empty, percent);

            let bar_style = if percent > 50.0 {
                Style::default().fg(Color::Red)
            } else if percent > 20.0 {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::Green)
            };
            let bar_span = Span::styled(bar_text, bar_style);

            // Item count
            let items_span = if entry.is_dir {
                Span::styled(format_count(entry.items_count), Style::default().fg(Color::DarkGray))
            } else {
                Span::raw("")
            };

            let row_style = if is_cursor {
                Style::default().bg(Color::Rgb(30, 35, 45))
            } else {
                Style::default()
            };

            Row::new(vec![
                Line::from(marker_span),
                Line::from(name_span),
                Line::from(badge_span),
                Line::from(size_span),
                Line::from(bar_span),
                Line::from(items_span),
            ])
            .style(row_style)
        })
        .collect();

    let widths = [
        Constraint::Length(6),  // Marker & Sel
        Constraint::Percentage(40), // Name
        Constraint::Length(14), // Badge
        Constraint::Length(12), // Size
        Constraint::Length(20), // Bar Graph
        Constraint::Length(12), // Items
    ];

    let header_row = Row::new(vec![
        "Sel",
        "Name",
        "Category",
        "Size",
        "Graph",
        "Items",
    ])
    .style(
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );

    let scroll_indicator = if total_items > viewport_height {
        if scroll_offset > 0 && scroll_offset + viewport_height < total_items {
            format!(" [{}/{} items ↕] ", app.cursor_index + 1, total_items)
        } else if scroll_offset > 0 {
            format!(" [{}/{} items ▲] ", app.cursor_index + 1, total_items)
        } else {
            format!(" [{}/{} items ▼] ", app.cursor_index + 1, total_items)
        }
    } else if total_items > 0 {
        format!(" [{}/{} items] ", app.cursor_index + 1, total_items)
    } else {
        " [0 items] ".to_string()
    };

    let table = Table::new(rows, widths)
        .header(header_row)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(Color::DarkGray))
                .title(Line::from(vec![
                    Span::styled(" Directory Contents", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                    Span::styled(scroll_indicator, Style::default().fg(Color::LightYellow)),
                ])),
        );

    f.render_widget(table, table_area);

    if total_items > viewport_height {
        let mut scrollbar_state = ScrollbarState::new(total_items).position(app.cursor_index);
        f.render_stateful_widget(
            Scrollbar::default()
                .orientation(ScrollbarOrientation::VerticalRight)
                .begin_symbol(Some("▲"))
                .end_symbol(Some("▼")),
            table_area,
            &mut scrollbar_state,
        );
    }

    // Render search prompt if active
    if let Some(s_area) = search_area {
        let search_line = Line::from(vec![
            Span::styled(" Search: ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled(&app.search_query, Style::default().fg(Color::White)),
            if app.is_searching {
                Span::styled("▋", Style::default().fg(Color::Yellow))
            } else {
                Span::styled(" [Esc to clear]", Style::default().fg(Color::DarkGray))
            },
        ]);
        f.render_widget(Paragraph::new(search_line), s_area);
    }
}

fn render_ghost_inspector(f: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Sub tabs
            Constraint::Min(5),    // Table / list
            Constraint::Length(3), // Summary bar
        ])
        .split(area);

    let tab_titles = vec![
        Line::from(vec![
            Span::raw("1. "),
            Span::styled("🐳 Docker Reclaimable Storage", Style::default().add_modifier(Modifier::BOLD)),
        ]),
        Line::from(vec![
            Span::raw("2. "),
            Span::styled("👻 Deleted-Open Ghost Files (/proc/*/fd)", Style::default().add_modifier(Modifier::BOLD)),
        ]),
    ];

    let tabs = Tabs::new(tab_titles)
        .select(app.ghost_tab_index)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(Color::DarkGray))
                .title(" Ghost & Docker Storage Inspector (Tab / 1 / 2 to switch) "),
        )
        .highlight_style(
            Style::default()
                .fg(Color::LightCyan)
                .add_modifier(Modifier::BOLD),
        );

    f.render_widget(tabs, chunks[0]);

    if app.ghost_tab_index == 0 {
        // Tab 1: Docker Storage
        render_docker_tab(f, app, chunks[1]);
        render_docker_summary(f, app, chunks[2]);
    } else {
        // Tab 2: Deleted Open Files
        render_deleted_open_tab(f, app, chunks[1]);
        render_deleted_open_summary(f, app, chunks[2]);
    }
}

fn render_docker_tab(f: &mut Frame, app: &App, area: Rect) {
    if !app.docker_info.is_available {
        let err_msg = app
            .docker_info
            .error_message
            .as_deref()
            .unwrap_or("Docker daemon is not reachable at /var/run/docker.sock");
        let p = Paragraph::new(vec![
            Line::from(Span::styled("Docker Daemon Unavailable", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))),
            Line::from(Span::styled(err_msg, Style::default().fg(Color::DarkGray))),
            Line::from(""),
            Line::from(Span::styled("Make sure Docker service is started: `systemctl start docker`", Style::default().fg(Color::Gray))),
        ])
        .block(Block::default().borders(Borders::ALL).border_type(BorderType::Rounded));
        f.render_widget(p, area);
        return;
    }

    let total_items = app.docker_info.items.len();
    let viewport_height = area.height.saturating_sub(3).max(1) as usize;
    let scroll_offset = compute_scroll_window(
        app.ghost_cursor_index,
        app.ghost_docker_scroll_offset.get(),
        viewport_height,
        total_items,
    );
    app.ghost_docker_scroll_offset.set(scroll_offset);

    let rows: Vec<Row> = app
        .docker_info
        .items
        .iter()
        .enumerate()
        .skip(scroll_offset)
        .take(viewport_height)
        .map(|(idx, item)| {
            let is_cursor = idx == app.ghost_cursor_index;
            let cursor = if is_cursor { "▶ " } else { "  " };

            let cat_style = match item.category {
                "Image" => Style::default().fg(Color::LightCyan),
                "Container" => Style::default().fg(Color::LightGreen),
                "Volume" => Style::default().fg(Color::LightYellow),
                "BuildCache" => Style::default().fg(Color::LightMagenta),
                _ => Style::default().fg(Color::White),
            };

            let recl_span = if item.is_reclaimable {
                Span::styled("✔ RECLAIMABLE", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD))
            } else {
                Span::styled("IN USE", Style::default().fg(Color::DarkGray))
            };

            let row_style = if is_cursor {
                Style::default().bg(Color::Rgb(30, 35, 45))
            } else {
                Style::default()
            };

            Row::new(vec![
                Line::from(vec![Span::raw(cursor), Span::styled(item.category, cat_style)]),
                Line::from(Span::styled(&item.id_or_name, Style::default().fg(Color::White))),
                Line::from(Span::styled(format_size(item.size), Style::default().fg(Color::Cyan))),
                Line::from(recl_span),
                Line::from(Span::styled(&item.details, Style::default().fg(Color::Gray))),
            ])
            .style(row_style)
        })
        .collect();

    let widths = [
        Constraint::Length(16),
        Constraint::Percentage(30),
        Constraint::Length(14),
        Constraint::Length(16),
        Constraint::Percentage(40),
    ];

    let header = Row::new(vec!["Category", "ID / Name", "Size", "Status", "Details"])
        .style(Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD));

    let scroll_indicator = if total_items > viewport_height {
        format!(" [{}/{}] ↕ ", app.ghost_cursor_index + 1, total_items)
    } else if total_items > 0 {
        format!(" [{}/{}] ", app.ghost_cursor_index + 1, total_items)
    } else {
        String::new()
    };

    let table = Table::new(rows, widths)
        .header(header)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title(Line::from(vec![
                    Span::styled(" Docker Artifacts & Volumes", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                    Span::styled(scroll_indicator, Style::default().fg(Color::LightYellow)),
                ])),
        );

    f.render_widget(table, area);

    if total_items > viewport_height {
        let mut scrollbar_state = ScrollbarState::new(total_items).position(app.ghost_cursor_index);
        f.render_stateful_widget(
            Scrollbar::default()
                .orientation(ScrollbarOrientation::VerticalRight)
                .begin_symbol(Some("▲"))
                .end_symbol(Some("▼")),
            area,
            &mut scrollbar_state,
        );
    }
}

fn render_docker_summary(f: &mut Frame, app: &App, area: Rect) {
    let total_reclaimable = app.docker_info.images_reclaimable_size
        + app.docker_info.containers_reclaimable_size
        + app.docker_info.volumes_reclaimable_size
        + app.docker_info.build_cache_reclaimable_size;

    let total_docker = app.docker_info.images_total_size
        + app.docker_info.containers_total_size
        + app.docker_info.volumes_total_size
        + app.docker_info.build_cache_total_size;

    let line = Line::from(vec![
        Span::styled(format!(" Total Docker: {}", format_size(total_docker)), Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        Span::raw(" │ "),
        Span::styled(format!("Reclaimable Ghost Space: {}", format_size(total_reclaimable)), Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
        Span::raw(" │ "),
        Span::styled("[p] Prune Dangling Resources", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Span::raw(" │ "),
        Span::styled("[r] Refresh Docker Data", Style::default().fg(Color::Cyan)),
    ]);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::DarkGray));

    f.render_widget(Paragraph::new(line).block(block), area);
}

fn render_deleted_open_tab(f: &mut Frame, app: &App, area: Rect) {
    if app.deleted_open_files.is_empty() {
        let p = Paragraph::new(vec![
            Line::from(Span::styled("No unlinked open files detected holding significant disk space.", Style::default().fg(Color::Green))),
            Line::from(""),
            Line::from(Span::styled("When processes delete open files on disk, they stay in /proc/*/fd holding disk space invisibly.", Style::default().fg(Color::DarkGray))),
            Line::from(Span::styled("Any such hidden ghost files will be displayed right here with their PID and process name.", Style::default().fg(Color::DarkGray))),
        ])
        .block(Block::default().borders(Borders::ALL).border_type(BorderType::Rounded).title(" Open Unlinked Files "));
        f.render_widget(p, area);
        return;
    }

    let total_items = app.deleted_open_files.len();
    let viewport_height = area.height.saturating_sub(3).max(1) as usize;
    let scroll_offset = compute_scroll_window(
        app.ghost_cursor_index,
        app.ghost_deleted_scroll_offset.get(),
        viewport_height,
        total_items,
    );
    app.ghost_deleted_scroll_offset.set(scroll_offset);

    let rows: Vec<Row> = app
        .deleted_open_files
        .iter()
        .enumerate()
        .skip(scroll_offset)
        .take(viewport_height)
        .map(|(idx, item)| {
            let is_cursor = idx == app.ghost_cursor_index;
            let cursor = if is_cursor { "▶ " } else { "  " };

            let row_style = if is_cursor {
                Style::default().bg(Color::Rgb(30, 35, 45))
            } else {
                Style::default()
            };

            Row::new(vec![
                Line::from(format!("{}{}", cursor, item.pid)),
                Line::from(Span::styled(&item.process_name, Style::default().fg(Color::LightYellow))),
                Line::from(Span::styled(format_size(item.size), Style::default().fg(Color::LightRed).add_modifier(Modifier::BOLD))),
                Line::from(Span::styled(&item.original_path, Style::default().fg(Color::White))),
            ])
            .style(row_style)
        })
        .collect();

    let widths = [
        Constraint::Length(10),
        Constraint::Length(20),
        Constraint::Length(14),
        Constraint::Percentage(60),
    ];

    let header = Row::new(vec!["PID", "Process", "Held Size", "Original File Path (deleted)"])
        .style(Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD));

    let scroll_indicator = if total_items > viewport_height {
        format!(" [{}/{}] ↕ ", app.ghost_cursor_index + 1, total_items)
    } else if total_items > 0 {
        format!(" [{}/{}] ", app.ghost_cursor_index + 1, total_items)
    } else {
        String::new()
    };

    let table = Table::new(rows, widths)
        .header(header)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title(Line::from(vec![
                    Span::styled(" Open Unlinked Files Holding Space", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                    Span::styled(scroll_indicator, Style::default().fg(Color::LightYellow)),
                ])),
        );

    f.render_widget(table, area);

    if total_items > viewport_height {
        let mut scrollbar_state = ScrollbarState::new(total_items).position(app.ghost_cursor_index);
        f.render_stateful_widget(
            Scrollbar::default()
                .orientation(ScrollbarOrientation::VerticalRight)
                .begin_symbol(Some("▲"))
                .end_symbol(Some("▼")),
            area,
            &mut scrollbar_state,
        );
    }
}

fn render_deleted_open_summary(f: &mut Frame, app: &App, area: Rect) {
    let total_held: u64 = app.deleted_open_files.iter().map(|f| f.size).sum();

    let line = Line::from(vec![
        Span::styled(format!(" Total Ghost Space Held: {}", format_size(total_held)), Style::default().fg(Color::LightRed).add_modifier(Modifier::BOLD)),
        Span::raw(" │ "),
        Span::styled(format!("Ghost Files: {}", app.deleted_open_files.len()), Style::default().fg(Color::White)),
        Span::raw(" │ "),
        Span::styled("To reclaim: restart or stop the corresponding process", Style::default().fg(Color::DarkGray)),
    ]);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::DarkGray));

    f.render_widget(Paragraph::new(line).block(block), area);
}

fn render_confirm_modal(f: &mut Frame, app: &App, screen: Rect) {
    let action = match app.pending_action {
        Some(a) => a,
        None => return,
    };

    let popup_width = 65.min(screen.width.saturating_sub(4));
    let popup_height = 14.min(screen.height.saturating_sub(4));

    let area = Rect {
        x: (screen.width.saturating_sub(popup_width)) / 2,
        y: (screen.height.saturating_sub(popup_height)) / 2,
        width: popup_width,
        height: popup_height,
    };

    f.render_widget(Clear, area);

    let (title, border_color, prompt_line, info_lines) = match action {
        ConfirmAction::MoveToTrash => {
            let count = app.action_targets.len();
            let sz = format_size(app.action_total_size);
            let title = " 🗑️  MOVE TO WASTEBIN (TRASH) ";
            let border_color = Color::Green;

            let prompt = Line::from(vec![
                Span::styled(" [y] Move to Wastebin ", Style::default().bg(Color::Green).fg(Color::Black).add_modifier(Modifier::BOLD)),
                Span::raw("    "),
                Span::styled(" [n / Esc] Cancel ", Style::default().bg(Color::DarkGray).fg(Color::White)),
            ]);

            let mut infos = vec![
                Line::from(vec![
                    Span::styled(format!("Move {} item(s) (total {}) to Wastebin?", count, sz), Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                ]),
                Line::from(""),
                Line::from(Span::styled("✔ Safe & Recoverable: Items are moved to ~/.local/share/Trash", Style::default().fg(Color::LightGreen))),
                Line::from(Span::styled("✔ Restore anytime using Dolphin, Nautilus, or trash-restore", Style::default().fg(Color::DarkGray))),
                Line::from(""),
            ];

            if let Some(first) = app.action_targets.first() {
                infos.push(Line::from(Span::styled(format!("Target: {}", first.to_string_lossy()), Style::default().fg(Color::Yellow))));
            }

            (title, border_color, prompt, infos)
        }
        ConfirmAction::PermanentDelete => {
            let count = app.action_targets.len();
            let sz = format_size(app.action_total_size);
            let title = " ⚠️  PERMANENT DESTRUCTION (CANNOT BE UNDONE) ";
            let border_color = Color::Red;

            let prompt = Line::from(vec![
                Span::styled(" [y] PERMANENTLY REMOVE ", Style::default().bg(Color::Red).fg(Color::White).add_modifier(Modifier::BOLD)),
                Span::raw("    "),
                Span::styled(" [n / Esc] Cancel ", Style::default().bg(Color::DarkGray).fg(Color::White)),
            ]);

            let mut infos = vec![
                Line::from(vec![
                    Span::styled(format!("PERMANENTLY ERASE {} item(s) (total {})?", count, sz), Style::default().fg(Color::LightRed).add_modifier(Modifier::BOLD)),
                ]),
                Line::from(""),
                Line::from(Span::styled("❌ WARNING: This bypasses Wastebin and deletes data FOREVER!", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))),
                Line::from(Span::styled("❌ Files cannot be recovered after this action.", Style::default().fg(Color::LightYellow))),
                Line::from(""),
            ];

            if let Some(first) = app.action_targets.first() {
                infos.push(Line::from(Span::styled(format!("Target: {}", first.to_string_lossy()), Style::default().fg(Color::Yellow))));
            }

            (title, border_color, prompt, infos)
        }
        ConfirmAction::DockerPrune => {
            let sz = format_size(app.action_total_size);
            let title = " 🐳  DOCKER SYSTEM PRUNE ";
            let border_color = Color::Yellow;

            let prompt = Line::from(vec![
                Span::styled(" [y] Prune Dangling Resources ", Style::default().bg(Color::Yellow).fg(Color::Black).add_modifier(Modifier::BOLD)),
                Span::raw("    "),
                Span::styled(" [n / Esc] Cancel ", Style::default().bg(Color::DarkGray).fg(Color::White)),
            ]);

            let infos = vec![
                Line::from(Span::styled(format!("Reclaim up to {} of Docker ghost data?", sz), Style::default().fg(Color::White).add_modifier(Modifier::BOLD))),
                Line::from(""),
                Line::from(Span::styled("This will safely remove:", Style::default().fg(Color::LightCyan))),
                Line::from(Span::styled(" • All stopped containers", Style::default().fg(Color::White))),
                Line::from(Span::styled(" • All dangling / untagged images", Style::default().fg(Color::White))),
                Line::from(Span::styled(" • Unused build cache layers", Style::default().fg(Color::White))),
                Line::from(Span::styled(" • Unattached local volumes", Style::default().fg(Color::White))),
                Line::from(""),
            ];

            (title, border_color, prompt, infos)
        }
    };

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(4), Constraint::Length(2)])
        .margin(1)
        .split(area);

    let modal_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(border_color))
        .title(title);

    f.render_widget(modal_block, area);
    f.render_widget(Paragraph::new(info_lines), layout[0]);
    f.render_widget(Paragraph::new(prompt_line).alignment(Alignment::Center), layout[1]);
}

fn render_help_modal(f: &mut Frame, screen: Rect) {
    let popup_width = 72.min(screen.width.saturating_sub(4));
    let popup_height = 20.min(screen.height.saturating_sub(2));

    let area = Rect {
        x: (screen.width.saturating_sub(popup_width)) / 2,
        y: (screen.height.saturating_sub(popup_height)) / 2,
        width: popup_width,
        height: popup_height,
    };

    f.render_widget(Clear, area);

    let help_text = vec![
        Line::from(Span::styled("NAVIGATION & BROWSING", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))),
        Line::from("  j / Down       Move cursor down"),
        Line::from("  k / Up         Move cursor up"),
        Line::from("  PgDn / Ctrl+d  Page down (15 items)"),
        Line::from("  PgUp / Ctrl+u  Page up (15 items)"),
        Line::from("  Enter / l      Enter / drill down into directory"),
        Line::from("  Backspace / h  Go up to parent directory (ascends to root /)"),
        Line::from("  \\              Jump directly to root filesystem (/)"),
        Line::from("  ~              Jump directly to home directory (~/)"),
        Line::from("  Home / End     Jump to top / bottom"),
        Line::from(""),
        Line::from(Span::styled("SELECTION & WASTEBIN / REMOVAL", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))),
        Line::from("  Space          Toggle selection on item"),
        Line::from("  a              Select all / unselect all in current folder"),
        Line::from("  t / w          Move selected (or current) to Wastebin / Trash"),
        Line::from("  d / D          Completely remove (permanent deletion)"),
        Line::from(""),
        Line::from(Span::styled("GHOST FILES & DOCKER MANAGEMENT", Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD))),
        Line::from("  Tab / g        Switch between Filesystem and Ghost/Docker Inspector"),
        Line::from("  G              Cycle ghost filter (All -> Hide Ghost -> Ghost ONLY)"),
        Line::from("  p              Prune Docker dangling resources (in Ghost view)"),
        Line::from(""),
        Line::from(Span::styled("DISPLAY & UTILITIES", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD))),
        Line::from("  s              Cycle sort order (Size desc, Size asc, Name, Items)"),
        Line::from("  A              Toggle Apparent size vs Actual block disk usage"),
        Line::from("  /              Interactive live search / filter"),
        Line::from("  r              Rescan current directory / refresh stats"),
        Line::from("  ?              Toggle this help overlay"),
        Line::from("  q / Ctrl+C     Quit ghostdu"),
    ];

    let help_widget = Paragraph::new(help_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(Color::LightCyan))
                .title(" 👻 ghostdu Keyboard Shortcuts (? or Esc to close) "),
        )
        .wrap(Wrap { trim: false });

    f.render_widget(help_widget, area);
}

fn render_footer(f: &mut Frame, app: &App, area: Rect) {
    if let Some(status) = app.current_status() {
        let status_line = Line::from(vec![
            Span::styled(" 📢 ", Style::default().fg(Color::Yellow)),
            Span::styled(status, Style::default().fg(Color::LightGreen).add_modifier(Modifier::BOLD)),
        ]);
        f.render_widget(Paragraph::new(status_line), area);
        return;
    }

    let footer_spans = match app.active_view {
        ActiveView::Filesystem => vec![
            Span::styled(" [?] Help ", Style::default().fg(Color::White).bg(Color::DarkGray)),
            Span::raw(" "),
            Span::styled(" [Tab] Ghost/Docker ", Style::default().fg(Color::Black).bg(Color::Magenta)),
            Span::raw(" "),
            Span::styled(" [G] Ghost Filter ", Style::default().fg(Color::Black).bg(Color::LightCyan)),
            Span::raw(" "),
            Span::styled(" [Space] Select ", Style::default().fg(Color::White).bg(Color::Blue)),
            Span::raw(" "),
            Span::styled(" [t] Wastebin ", Style::default().fg(Color::Black).bg(Color::Green)),
            Span::raw(" "),
            Span::styled(" [d] Delete ", Style::default().fg(Color::White).bg(Color::Red)),
            Span::raw(" "),
            Span::styled(" [/] Filter ", Style::default().fg(Color::Black).bg(Color::Yellow)),
            Span::raw(" "),
            Span::styled(" [s] Sort ", Style::default().fg(Color::White).bg(Color::DarkGray)),
            Span::raw(" "),
            Span::styled(" [q] Quit ", Style::default().fg(Color::White).bg(Color::DarkGray)),
        ],
        ActiveView::GhostInspector => vec![
            Span::styled(" [Tab] Filesystem ", Style::default().fg(Color::Black).bg(Color::LightCyan)),
            Span::raw(" "),
            Span::styled(" [1/2] Switch Tab ", Style::default().fg(Color::White).bg(Color::DarkGray)),
            Span::raw(" "),
            Span::styled(" [p] Prune Docker ", Style::default().fg(Color::Black).bg(Color::Yellow)),
            Span::raw(" "),
            Span::styled(" [r] Refresh ", Style::default().fg(Color::Black).bg(Color::Green)),
            Span::raw(" "),
            Span::styled(" [q] Back/Quit ", Style::default().fg(Color::White).bg(Color::DarkGray)),
        ],
        ActiveView::ConfirmModal => vec![
            Span::styled(" [y] Confirm Action ", Style::default().fg(Color::Black).bg(Color::Yellow)),
            Span::raw(" "),
            Span::styled(" [n / Esc] Cancel ", Style::default().fg(Color::White).bg(Color::DarkGray)),
        ],
        ActiveView::HelpModal => vec![
            Span::styled(" [? / Esc] Close Help ", Style::default().fg(Color::White).bg(Color::DarkGray)),
        ],
    };

    f.render_widget(Paragraph::new(Line::from(footer_spans)), area);
}

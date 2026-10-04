use crate::fs::entry::{format_count, format_count_short, format_size, DeleteSafety, GhostKind};
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

fn truncate_path(path: &str, max_len: usize) -> String {
    crate::fs::entry::truncate_start_by_width(path, max_len)
}

fn append_if_fits<'a>(
    line_spans: &mut Vec<Span<'a>>,
    new_spans: Vec<Span<'a>>,
    max_width: usize,
) -> bool {
    let mut candidate = line_spans.clone();
    candidate.extend(new_spans.clone());
    if Line::from(candidate).width() <= max_width {
        line_spans.extend(new_spans);
        true
    } else {
        false
    }
}

pub fn render_ui(f: &mut Frame, app: &App) {
    let size = f.area();
    if size.width < 10 || size.height < 3 {
        f.render_widget(Paragraph::new("Terminal too small"), size);
        return;
    }

    let footer_len = if size.height >= 5 { 1 } else { 0 };
    let header_len = if size.height >= 22 {
        5 // 3 lines of content + 2 border rows
    } else if size.height >= 16 {
        4 // 2 lines of content + 2 border rows
    } else if size.height >= 10 {
        3 // 1 line of content + 2 border rows
    } else if size.height >= 5 {
        1 // 1 line raw text, no border
    } else {
        0
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(header_len),
            Constraint::Min(2),
            Constraint::Length(footer_len),
        ])
        .split(size);

    if header_len > 0 {
        render_header(f, app, chunks[0], header_len);
    }

    match app.active_view {
        ActiveView::Filesystem => render_filesystem_view(f, app, chunks[1]),
        ActiveView::GhostInspector => render_ghost_inspector(f, app, chunks[1]),
        ActiveView::TopFiles => render_top_files(f, app, chunks[1]),
        ActiveView::Janitor => render_janitor(f, app, chunks[1]),
        ActiveView::ConfirmModal => {
            // Render underlying view then overlay modal
            if app.previous_view == ActiveView::GhostInspector {
                render_ghost_inspector(f, app, chunks[1]);
            } else if app.previous_view == ActiveView::TopFiles {
                render_top_files(f, app, chunks[1]);
            } else if app.previous_view == ActiveView::Janitor {
                render_janitor(f, app, chunks[1]);
            } else {
                render_filesystem_view(f, app, chunks[1]);
            }
            render_confirm_modal(f, app, size);
        }
        ActiveView::HelpModal => {
            render_filesystem_view(f, app, chunks[1]);
            render_help_modal(f, size);
        }
        ActiveView::ItemInfoModal => {
            render_filesystem_view(f, app, chunks[1]);
            render_item_info_modal(f, app, size);
        }
    }

    if footer_len > 0 {
        render_footer(f, app, chunks[2]);
    }
}

fn render_header(f: &mut Frame, app: &App, area: Rect, header_len: u16) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let current_dir = app.current_dir_entry();
    let current_path = current_dir.path.to_string_lossy();
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
        GhostFilterMode::ShowAll => {
            Span::styled(" [Ghost: All]", Style::default().fg(Color::DarkGray))
        }
        GhostFilterMode::HideGhost => {
            Span::styled(" [Ghost: Hidden]", Style::default().fg(Color::Green))
        }
        GhostFilterMode::GhostOnly => Span::styled(
            " [Ghost: ONLY]",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
    };

    let size_mode_span = if app.apparent_size {
        Span::styled(" [Apparent]", Style::default().fg(Color::Cyan))
    } else {
        Span::styled(" [Disk Block]", Style::default().fg(Color::Blue))
    };

    let view_tab_span = if app.active_view == ActiveView::GhostInspector {
        Span::styled(
            " [👻 GHOST]",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(
            " [📂 EXPLORER]",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
    };

    let safe_reclaimable = current_dir.safe_reclaimable_bytes();
    let safe_stat_span = if safe_reclaimable > 0 {
        Span::styled(
            format!(" │ 🟢 Safe: {}", format_size(safe_reclaimable)),
            Style::default()
                .fg(Color::LightGreen)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::raw("")
    };

    let safe_filter_span = if app.safe_only_filter {
        Span::styled(
            " [🟢 SAFE ONLY]",
            Style::default()
                .fg(Color::LightGreen)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::raw("")
    };

    let content_width = if header_len >= 3 {
        (area.width as usize).saturating_sub(2)
    } else {
        area.width as usize
    };

    // Mode 1: Minimal single-line header (header_len == 1, no border)
    if header_len == 1 {
        let mut line_spans = vec![Span::styled("📁 ", Style::default().fg(Color::Cyan))];
        let free_str = if let Some(ref fs) = app.fs_info {
            format!(" │ Free: {}", format_size(fs.avail_bytes))
        } else {
            String::new()
        };

        let fixed_right_len = 3
            + format_size(current_dir.disk_usage).len()
            + free_str.len()
            + (if safe_reclaimable > 0 { 16 } else { 0 })
            + (if app.safe_only_filter { 16 } else { 0 })
            + (if sel_count > 0 { 18 } else { 0 });
        let max_path = content_width.saturating_sub(fixed_right_len).max(8);
        let trunc_p = truncate_path(&current_path, max_path);

        line_spans.push(Span::styled(
            trunc_p,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ));
        if safe_reclaimable > 0 {
            append_if_fits(&mut line_spans, vec![safe_stat_span], content_width);
        }
        append_if_fits(
            &mut line_spans,
            vec![
                Span::raw(" │ "),
                Span::styled(
                    format_size(current_dir.disk_usage),
                    Style::default().fg(Color::Green),
                ),
            ],
            content_width,
        );
        if !free_str.is_empty() {
            append_if_fits(
                &mut line_spans,
                vec![Span::styled(
                    free_str,
                    Style::default().fg(Color::LightGreen),
                )],
                content_width,
            );
        }
        if app.safe_only_filter {
            append_if_fits(&mut line_spans, vec![safe_filter_span], content_width);
        }
        if sel_count > 0 {
            append_if_fits(&mut line_spans, vec![selection_span], content_width);
        }

        f.render_widget(Paragraph::new(Line::from(line_spans)), area);
        return;
    }

    // Mode 2: Compact 1 content line inside border (header_len == 3)
    if header_len == 3 {
        let mut line_spans = vec![Span::styled("📁 ", Style::default().fg(Color::Cyan))];
        let free_str = if let Some(ref fs) = app.fs_info {
            format!(
                " │ 💾 Free: {} ({:.0}%)",
                format_size(fs.avail_bytes),
                fs.use_percent
            )
        } else {
            String::new()
        };

        let fixed_right_len = 5
            + format_size(current_dir.disk_usage).len()
            + free_str.len()
            + (if safe_reclaimable > 0 { 16 } else { 0 })
            + (if app.safe_only_filter { 16 } else { 0 })
            + (if sel_count > 0 { 18 } else { 0 });
        let max_path = content_width.saturating_sub(fixed_right_len).max(8);
        let trunc_p = truncate_path(&current_path, max_path);

        line_spans.push(Span::styled(
            trunc_p,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ));
        if safe_reclaimable > 0 {
            append_if_fits(&mut line_spans, vec![safe_stat_span], content_width);
        }
        append_if_fits(
            &mut line_spans,
            vec![
                Span::raw(" │ "),
                Span::styled(
                    format!("📊 {}", format_size(current_dir.disk_usage)),
                    Style::default().fg(Color::Green),
                ),
            ],
            content_width,
        );
        if !free_str.is_empty() {
            append_if_fits(
                &mut line_spans,
                vec![Span::styled(
                    free_str,
                    Style::default().fg(Color::LightGreen),
                )],
                content_width,
            );
        }
        if app.safe_only_filter {
            append_if_fits(&mut line_spans, vec![safe_filter_span], content_width);
        }
        if sel_count > 0 {
            append_if_fits(&mut line_spans, vec![selection_span], content_width);
        }

        let widget = Paragraph::new(Line::from(line_spans)).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(Color::DarkGray)),
        );
        f.render_widget(widget, area);
        return;
    }

    // Mode 3: Medium 2 content lines inside border (header_len == 4)
    if header_len == 4 {
        let max_path = content_width.saturating_sub(25).max(10);
        let trunc_p = truncate_path(&current_path, max_path);
        let mut line1_spans = vec![
            Span::styled(
                "👻 ghostdu ",
                Style::default()
                    .fg(Color::LightCyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("📁 {}", trunc_p),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ];
        if app.safe_only_filter {
            append_if_fits(&mut line1_spans, vec![safe_filter_span], content_width);
        }
        if sel_count > 0 {
            append_if_fits(
                &mut line1_spans,
                vec![selection_span.clone()],
                content_width,
            );
        }

        let free_str = if let Some(ref fs) = app.fs_info {
            format!(
                " │ 💾 Free: {} / {} ({:.0}%)",
                format_size(fs.avail_bytes),
                format_size(fs.total_bytes),
                fs.use_percent
            )
        } else {
            String::new()
        };

        let mut line2_spans = vec![
            Span::styled(
                format!("📊 {}", format_size(current_dir.disk_usage)),
                Style::default().fg(Color::Green),
            ),
            Span::styled(
                format!(" ({})", count_str),
                Style::default().fg(Color::White),
            ),
        ];
        if safe_reclaimable > 0 {
            append_if_fits(
                &mut line2_spans,
                vec![safe_stat_span.clone()],
                content_width,
            );
        }
        if !free_str.is_empty() {
            append_if_fits(
                &mut line2_spans,
                vec![Span::styled(
                    free_str,
                    Style::default().fg(Color::LightGreen),
                )],
                content_width,
            );
        }
        append_if_fits(
            &mut line2_spans,
            vec![
                Span::raw(" │ "),
                Span::styled(
                    format!("Sort: {}", app.sort_mode.label()),
                    Style::default().fg(Color::LightYellow),
                ),
            ],
            content_width,
        );

        let widget = Paragraph::new(vec![Line::from(line1_spans), Line::from(line2_spans)]).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(Color::DarkGray)),
        );
        f.render_widget(widget, area);
        return;
    }

    // Mode 4: Full 3 content lines inside border (header_len >= 5)
    // Line 1: Title & Current Path
    let show_hints = current_path != "/" && content_width >= 85;
    let hints_span = Span::styled(
        " (Bksp: up, \\: root)",
        Style::default().fg(Color::DarkGray),
    );
    let fixed_title_line = Line::from(vec![
        Span::styled(
            "👻 ghostdu ",
            Style::default()
                .fg(Color::LightCyan)
                .add_modifier(Modifier::BOLD),
        ),
        view_tab_span.clone(),
        safe_filter_span.clone(),
        Span::raw(" │ 📁 "),
        if show_hints {
            hints_span.clone()
        } else {
            Span::raw("")
        },
    ]);
    let fixed_title_w = fixed_title_line.width();
    let max_path = content_width.saturating_sub(fixed_title_w).max(8);
    let trunc_p = truncate_path(&current_path, max_path);

    let mut title_spans = vec![
        Span::styled(
            "👻 ghostdu ",
            Style::default()
                .fg(Color::LightCyan)
                .add_modifier(Modifier::BOLD),
        ),
        view_tab_span,
        safe_filter_span,
        Span::raw(" │ "),
        Span::styled(
            format!("📁 {}", trunc_p),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    if show_hints {
        title_spans.push(hints_span);
    }
    let title_line = Line::from(title_spans);

    // Line 2: Folder statistics
    let mut folder_spans = vec![
        Span::styled(
            if content_width >= 40 {
                "📊 Folder: "
            } else {
                "📊 "
            },
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            if content_width >= 90 {
                format!("{} (disk)", format_size(current_dir.disk_usage))
            } else {
                format_size(current_dir.disk_usage)
            },
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
    ];

    if safe_reclaimable > 0 {
        append_if_fits(
            &mut folder_spans,
            vec![
                Span::raw(" │ "),
                Span::styled(
                    format!("🟢 Safe: {}", format_size(safe_reclaimable)),
                    Style::default()
                        .fg(Color::LightGreen)
                        .add_modifier(Modifier::BOLD),
                ),
            ],
            content_width,
        );
    }

    if content_width >= 115 {
        append_if_fits(
            &mut folder_spans,
            vec![
                Span::raw(" │ "),
                Span::styled(
                    format!("{} (apparent)", format_size(current_dir.size)),
                    Style::default().fg(Color::Cyan),
                ),
            ],
            content_width,
        );
    }

    append_if_fits(
        &mut folder_spans,
        vec![
            Span::raw(" │ "),
            Span::styled(count_str.clone(), Style::default().fg(Color::White)),
        ],
        content_width,
    );

    append_if_fits(
        &mut folder_spans,
        vec![
            Span::raw(" │ "),
            Span::styled(
                format!("Sort: {}", app.sort_mode.label()),
                Style::default().fg(Color::LightYellow),
            ),
        ],
        content_width,
    );

    if app.ghost_filter != GhostFilterMode::ShowAll {
        append_if_fits(&mut folder_spans, vec![filter_span], content_width);
    }
    if content_width >= 120 {
        append_if_fits(&mut folder_spans, vec![size_mode_span], content_width);
    }
    if sel_count > 0 {
        append_if_fits(
            &mut folder_spans,
            vec![selection_span.clone()],
            content_width,
        );
    }

    let folder_stat_line = Line::from(folder_spans);

    // Line 3: Global filesystem statistics
    let fs_line = if let Some(ref fs) = app.fs_info {
        let bar_len = 10;
        let filled_len = ((fs.use_percent / 100.0) * bar_len as f64).round() as usize;
        let bar_filled = "█".repeat(filled_len.min(bar_len));
        let bar_empty = "░".repeat(bar_len.saturating_sub(filled_len));
        let bar_color = if fs.use_percent >= 90.0 {
            Color::Red
        } else if fs.use_percent >= 75.0 {
            Color::Yellow
        } else {
            Color::Green
        };

        // Option A: Full detail
        let opt_a = Line::from(vec![
            Span::styled("💾 Global Disk: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{} ({})", fs.device, fs.fs_type),
                Style::default().fg(Color::White),
            ),
            Span::styled(
                format!(" on {}", fs.mount_point.display()),
                Style::default().fg(Color::DarkGray),
            ),
            Span::raw(" │ "),
            Span::styled(
                format!("Total: {}", format_size(fs.total_bytes)),
                Style::default().fg(Color::White),
            ),
            Span::raw(" │ "),
            Span::styled(
                format!("Used: {}", format_size(fs.used_bytes)),
                Style::default().fg(Color::LightRed),
            ),
            Span::raw(" "),
            Span::styled(
                format!("[{}{}] {:.1}%", bar_filled, bar_empty, fs.use_percent),
                Style::default().fg(bar_color).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" │ "),
            Span::styled(
                format!("Free: {}", format_size(fs.avail_bytes)),
                Style::default()
                    .fg(Color::LightGreen)
                    .add_modifier(Modifier::BOLD),
            ),
        ]);

        // Option B: Medium detail (device omitted, mount + fs type shown)
        let compact_len = 8;
        let c_fill = ((fs.use_percent / 100.0) * compact_len as f64).round() as usize;
        let c_bar = format!(
            "{}{}",
            "█".repeat(c_fill.min(compact_len)),
            "░".repeat(compact_len.saturating_sub(c_fill))
        );
        let opt_b = Line::from(vec![
            Span::styled("💾 Disk: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{} on {}", fs.fs_type, fs.mount_point.display()),
                Style::default().fg(Color::White),
            ),
            Span::raw(" │ "),
            Span::styled(
                format!("Total: {}", format_size(fs.total_bytes)),
                Style::default().fg(Color::White),
            ),
            Span::raw(" │ "),
            Span::styled(
                format!("Used: {}", format_size(fs.used_bytes)),
                Style::default().fg(Color::LightRed),
            ),
            Span::raw(" "),
            Span::styled(
                format!("[{}] {:.1}%", c_bar, fs.use_percent),
                Style::default().fg(bar_color).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" │ "),
            Span::styled(
                format!("Free: {}", format_size(fs.avail_bytes)),
                Style::default()
                    .fg(Color::LightGreen)
                    .add_modifier(Modifier::BOLD),
            ),
        ]);

        // Option C: Compact
        let opt_c = Line::from(vec![
            Span::styled("💾 Disk: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{} ({})", fs.mount_point.display(), fs.fs_type),
                Style::default().fg(Color::White),
            ),
            Span::raw(" │ "),
            Span::styled(
                format!(
                    "{}/{}",
                    format_size(fs.used_bytes),
                    format_size(fs.total_bytes)
                ),
                Style::default().fg(Color::LightRed),
            ),
            Span::styled(
                format!(" ({:.0}%)", fs.use_percent),
                Style::default().fg(bar_color),
            ),
            Span::raw(" │ "),
            Span::styled(
                format!("Free: {}", format_size(fs.avail_bytes)),
                Style::default()
                    .fg(Color::LightGreen)
                    .add_modifier(Modifier::BOLD),
            ),
        ]);

        // Option D: Mini
        let opt_d = Line::from(vec![
            Span::styled(
                format!("💾 {}: ", fs.mount_point.display()),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                format!(
                    "{}/{} ({:.0}%)",
                    format_size(fs.used_bytes),
                    format_size(fs.total_bytes),
                    fs.use_percent
                ),
                Style::default().fg(Color::LightRed),
            ),
            Span::raw(" │ "),
            Span::styled(
                format!("Free: {}", format_size(fs.avail_bytes)),
                Style::default()
                    .fg(Color::LightGreen)
                    .add_modifier(Modifier::BOLD),
            ),
        ]);

        // Option E: Minimal fallback
        let opt_e = Line::from(vec![
            Span::styled("💾 Free: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format_size(fs.avail_bytes),
                Style::default()
                    .fg(Color::LightGreen)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" ({:.0}% used)", fs.use_percent),
                Style::default().fg(Color::DarkGray),
            ),
        ]);

        if opt_a.width() <= content_width {
            opt_a
        } else if opt_b.width() <= content_width {
            opt_b
        } else if opt_c.width() <= content_width {
            opt_c
        } else if opt_d.width() <= content_width {
            opt_d
        } else {
            opt_e
        }
    } else {
        Line::from(vec![
            Span::styled("💾 Disk: ", Style::default().fg(Color::DarkGray)),
            Span::styled("Unavailable", Style::default().fg(Color::DarkGray)),
        ])
    };

    let header_widget = Paragraph::new(vec![title_line, folder_stat_line, fs_line]).block(
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
    if area.width < 10 || area.height == 0 {
        return;
    }

    let visible = app.visible_children();
    let current_dir = app.current_dir_entry();
    let parent_size = current_dir.display_size(app.apparent_size).max(1);

    // If searching, reserve 1 line at the bottom for search bar
    let (table_area, search_area) =
        if (app.is_searching || !app.search_query.is_empty()) && area.height >= 4 {
            let split = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(2), Constraint::Length(1)])
                .split(area);
            (split[0], Some(split[1]))
        } else {
            (area, None)
        };

    let has_border = table_area.height >= 5;
    let border_padding = if has_border { 2 } else { 0 };
    let viewport_height = (table_area.height as usize)
        .saturating_sub(border_padding + 1)
        .max(1);

    let total_items = visible.len();
    let scroll_offset = compute_scroll_window(
        app.cursor_index,
        app.scroll_offset.get(),
        viewport_height,
        total_items,
    );
    app.scroll_offset.set(scroll_offset);

    // Dynamic column layout based on available width
    let col_mode = if table_area.width >= 100 {
        0 // Wide: 6 full columns
    } else if table_area.width >= 75 {
        1 // Standard: 6 compact columns
    } else if table_area.width >= 50 {
        2 // Narrow: 4 columns (embed badge into name)
    } else {
        3 // Ultra narrow: 3 columns
    };

    let rows: Vec<Row> = visible
        .iter()
        .enumerate()
        .skip(scroll_offset)
        .take(viewport_height)
        .map(|(idx, entry)| {
            let is_cursor = idx == app.cursor_index;
            let is_selected = app.selected_paths.contains(&entry.path);

            let cursor_str = if is_cursor { "▶" } else { " " };
            let sel_str = if is_selected {
                if col_mode == 3 {
                    "*"
                } else {
                    " [*]"
                }
            } else if col_mode == 3 {
                " "
            } else {
                " [ ]"
            };

            let marker_span = Span::styled(
                format!("{}{}", cursor_str, sel_str),
                if is_selected {
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                } else if is_cursor {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
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
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };

            // In narrow modes (2 & 3), embed ghost badge & safety directly into name column
            let display_name = if col_mode >= 2 {
                if entry.ghost_kind.is_ghost() {
                    format!(
                        "{}{} {} {}",
                        icon,
                        entry.delete_safety.glyph(),
                        entry.ghost_kind.badge(),
                        entry.name
                    )
                } else if entry.delete_safety == DeleteSafety::System {
                    format!(
                        "{}{} SYSTEM {}",
                        icon,
                        entry.delete_safety.glyph(),
                        entry.name
                    )
                } else {
                    format!("{}{}", icon, entry.name)
                }
            } else {
                format!("{}{}", icon, entry.name)
            };

            let name_span = Span::styled(display_name, name_style);

            // Ghost/Docker/Category/Safety Badge for wide/standard modes
            let badge_span = match entry.ghost_kind {
                GhostKind::DockerOverlay
                | GhostKind::DockerVolume
                | GhostKind::DockerContainer
                | GhostKind::DockerBuildkit
                | GhostKind::DockerUser => Span::styled(
                    format!(
                        "{} {}",
                        entry.delete_safety.glyph(),
                        entry.ghost_kind.badge()
                    ),
                    Style::default()
                        .fg(Color::LightBlue)
                        .add_modifier(Modifier::BOLD),
                ),
                GhostKind::PodmanUser => Span::styled(
                    format!("{} 🦭 PODMAN", entry.delete_safety.glyph()),
                    Style::default().fg(Color::Magenta),
                ),
                GhostKind::DeletedOpen => Span::styled(
                    format!("{} 👻 GHOST", entry.delete_safety.glyph()),
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ),
                GhostKind::Trash => Span::styled(
                    format!("{} 🗑️ TRASH", entry.delete_safety.glyph()),
                    Style::default().fg(Color::LightRed),
                ),
                GhostKind::LogFiles => Span::styled(
                    format!("{} 📜 LOGS", entry.delete_safety.glyph()),
                    Style::default().fg(Color::Yellow),
                ),
                GhostKind::Flatpak => Span::styled(
                    format!("{} 📦 FLATPAK", entry.delete_safety.glyph()),
                    Style::default().fg(Color::LightCyan),
                ),
                GhostKind::SnapPackage => Span::styled(
                    format!("{} 📦 SNAP", entry.delete_safety.glyph()),
                    Style::default().fg(Color::LightCyan),
                ),
                GhostKind::DependencyTree => Span::styled(
                    format!("{} 📦 DEPS", entry.delete_safety.glyph()),
                    Style::default().fg(Color::Cyan),
                ),
                GhostKind::GamingCompat => Span::styled(
                    format!("{} 🎮 GAME", entry.delete_safety.glyph()),
                    Style::default().fg(Color::LightGreen),
                ),
                GhostKind::AiModel => Span::styled(
                    format!("{} 🤖 AI", entry.delete_safety.glyph()),
                    Style::default()
                        .fg(Color::LightMagenta)
                        .add_modifier(Modifier::BOLD),
                ),
                GhostKind::VmOrIso => Span::styled(
                    format!("{} 💿 VM/ISO", entry.delete_safety.glyph()),
                    Style::default().fg(Color::LightBlue),
                ),
                GhostKind::BrowserCache => Span::styled(
                    format!("{} 🌐 BROWSER", entry.delete_safety.glyph()),
                    Style::default().fg(Color::LightYellow),
                ),
                GhostKind::CoreDump => Span::styled(
                    format!("{} 💥 CRASH", entry.delete_safety.glyph()),
                    Style::default()
                        .fg(Color::LightRed)
                        .add_modifier(Modifier::BOLD),
                ),
                GhostKind::SystemSnapshot => Span::styled(
                    format!("{} 🔒 SNAP", entry.delete_safety.glyph()),
                    Style::default().fg(Color::LightRed),
                ),
                GhostKind::PackageCache => Span::styled(
                    format!("{} 📦 PKG", entry.delete_safety.glyph()),
                    Style::default().fg(Color::LightYellow),
                ),
                GhostKind::BuildCache => Span::styled(
                    format!("{} 👻 CACHE", entry.delete_safety.glyph()),
                    Style::default().fg(Color::DarkGray),
                ),
                GhostKind::None => {
                    if entry.has_err {
                        Span::styled("[!] LOCKED", Style::default().fg(Color::Red))
                    } else if entry.delete_safety == DeleteSafety::System {
                        Span::styled(
                            "🔴 SYSTEM",
                            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                        )
                    } else {
                        Span::raw("")
                    }
                }
            };

            // Size with color gradient
            let entry_size = entry.display_size(app.apparent_size);
            let size_style = if entry_size >= 10 * 1024 * 1024 * 1024 {
                Style::default()
                    .fg(Color::LightRed)
                    .add_modifier(Modifier::BOLD)
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
            let bar_text = if col_mode == 0 {
                let bl = 8;
                let fl = ((percent / 100.0) * bl as f64).round() as usize;
                format!(
                    "[{}{}] {:>5.1}%",
                    "█".repeat(fl.min(bl)),
                    "░".repeat(bl.saturating_sub(fl)),
                    percent
                )
            } else if col_mode == 1 {
                let bl = 6;
                let fl = ((percent / 100.0) * bl as f64).round() as usize;
                format!(
                    "[{}{}] {:>3.0}%",
                    "█".repeat(fl.min(bl)),
                    "░".repeat(bl.saturating_sub(fl)),
                    percent
                )
            } else {
                let bl = 5;
                let fl = ((percent / 100.0) * bl as f64).round() as usize;
                format!(
                    "[{}{}] {:>3.0}%",
                    "█".repeat(fl.min(bl)),
                    "░".repeat(bl.saturating_sub(fl)),
                    percent
                )
            };

            let bar_style = if percent > 50.0 {
                Style::default().fg(Color::Red)
            } else if percent > 20.0 {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::Green)
            };
            let bar_span = Span::styled(bar_text, bar_style);

            // Item count
            let items_str = if entry.is_dir {
                format_count_short(entry.items_count)
            } else {
                String::new()
            };
            let items_span = Span::styled(items_str, Style::default().fg(Color::DarkGray));

            let row_style = if is_cursor {
                Style::default().bg(Color::Rgb(30, 35, 45))
            } else {
                Style::default()
            };

            match col_mode {
                0 => Row::new(vec![
                    Line::from(marker_span),
                    Line::from(name_span),
                    Line::from(badge_span),
                    Line::from(size_span),
                    Line::from(bar_span),
                    Line::from(items_span),
                ])
                .style(row_style),
                1 => Row::new(vec![
                    Line::from(marker_span),
                    Line::from(name_span),
                    Line::from(badge_span),
                    Line::from(size_span),
                    Line::from(bar_span),
                    Line::from(items_span),
                ])
                .style(row_style),
                2 => Row::new(vec![
                    Line::from(marker_span),
                    Line::from(name_span),
                    Line::from(size_span),
                    Line::from(bar_span),
                ])
                .style(row_style),
                _ => Row::new(vec![
                    Line::from(marker_span),
                    Line::from(name_span),
                    Line::from(size_span),
                ])
                .style(row_style),
            }
        })
        .collect();

    let (widths, header_row) = match col_mode {
        0 => (
            vec![
                Constraint::Length(5),  // Marker & Sel
                Constraint::Min(20),    // Name (absorbs remaining space)
                Constraint::Length(14), // Category & Safety
                Constraint::Length(11), // Size
                Constraint::Length(18), // Usage Bar
                Constraint::Length(8),  // Items
            ],
            Row::new(vec![
                "Sel",
                "Name",
                "Category",
                "Size",
                "Usage Bar",
                "Items",
            ]),
        ),
        1 => (
            vec![
                Constraint::Length(5),  // Marker & Sel
                Constraint::Min(16),    // Name
                Constraint::Length(13), // Category & Safety
                Constraint::Length(10), // Size
                Constraint::Length(14), // Bar
                Constraint::Length(7),  // Items
            ],
            Row::new(vec!["Sel", "Name", "Category", "Size", "Bar", "Items"]),
        ),
        2 => (
            vec![
                Constraint::Length(5),  // Marker & Sel
                Constraint::Min(14),    // Name
                Constraint::Length(10), // Size
                Constraint::Length(13), // Bar
            ],
            Row::new(vec!["Sel", "Name", "Size", "Bar"]),
        ),
        _ => (
            vec![
                Constraint::Length(3), // Marker
                Constraint::Min(10),   // Name
                Constraint::Length(9), // Size
            ],
            Row::new(vec!["", "Name", "Size"]),
        ),
    };

    let header_row = header_row.style(
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );

    let scroll_indicator = if total_items > viewport_height {
        if scroll_offset > 0 && scroll_offset + viewport_height < total_items {
            format!(" [{}/{} ↕] ", app.cursor_index + 1, total_items)
        } else if scroll_offset > 0 {
            format!(" [{}/{} ▲] ", app.cursor_index + 1, total_items)
        } else {
            format!(" [{}/{} ▼] ", app.cursor_index + 1, total_items)
        }
    } else if total_items > 0 {
        format!(" [{}/{}] ", app.cursor_index + 1, total_items)
    } else {
        " [0 items] ".to_string()
    };

    let table_block = if has_border {
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::DarkGray))
            .title(Line::from(vec![
                Span::styled(
                    " Directory Contents",
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(scroll_indicator, Style::default().fg(Color::LightYellow)),
            ]))
    } else {
        Block::default()
    };

    let table = Table::new(rows, widths)
        .header(header_row)
        .block(table_block);

    f.render_widget(table, table_area);

    if total_items > viewport_height && table_area.width >= 45 && has_border {
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
            Span::styled(
                " Search: ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
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
            Span::styled(
                "🐳 Docker Reclaimable Storage",
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::raw("2. "),
            Span::styled(
                "👻 Deleted-Open Ghost Files (/proc/*/fd)",
                Style::default().add_modifier(Modifier::BOLD),
            ),
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
            Line::from(Span::styled(
                "Docker Daemon Unavailable",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(err_msg, Style::default().fg(Color::DarkGray))),
            Line::from(""),
            Line::from(Span::styled(
                "Make sure Docker service is started: `systemctl start docker`",
                Style::default().fg(Color::Gray),
            )),
        ])
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded),
        );
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
                Span::styled(
                    "✔ RECLAIMABLE",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Span::styled("IN USE", Style::default().fg(Color::DarkGray))
            };

            let row_style = if is_cursor {
                Style::default().bg(Color::Rgb(30, 35, 45))
            } else {
                Style::default()
            };

            Row::new(vec![
                Line::from(vec![
                    Span::raw(cursor),
                    Span::styled(item.category, cat_style),
                ]),
                Line::from(Span::styled(
                    &item.id_or_name,
                    Style::default().fg(Color::White),
                )),
                Line::from(Span::styled(
                    format_size(item.size),
                    Style::default().fg(Color::Cyan),
                )),
                Line::from(recl_span),
                Line::from(Span::styled(
                    &item.details,
                    Style::default().fg(Color::Gray),
                )),
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

    let header = Row::new(vec!["Category", "ID / Name", "Size", "Status", "Details"]).style(
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );

    let scroll_indicator = if total_items > viewport_height {
        format!(" [{}/{}] ↕ ", app.ghost_cursor_index + 1, total_items)
    } else if total_items > 0 {
        format!(" [{}/{}] ", app.ghost_cursor_index + 1, total_items)
    } else {
        String::new()
    };

    let table = Table::new(rows, widths).header(header).block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(Line::from(vec![
                Span::styled(
                    " Docker Artifacts & Volumes",
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
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
        Span::styled(
            format!(" Total Docker: {}", format_size(total_docker)),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" │ "),
        Span::styled(
            format!(
                "Reclaimable Ghost Space: {}",
                format_size(total_reclaimable)
            ),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" │ "),
        Span::styled(
            "[p] Prune Dangling Resources",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
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
                Line::from(Span::styled(
                    &item.process_name,
                    Style::default().fg(Color::LightYellow),
                )),
                Line::from(Span::styled(
                    format_size(item.size),
                    Style::default()
                        .fg(Color::LightRed)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(
                    &item.original_path,
                    Style::default().fg(Color::White),
                )),
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

    let header = Row::new(vec![
        "PID",
        "Process",
        "Held Size",
        "Original File Path (deleted)",
    ])
    .style(
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );

    let scroll_indicator = if total_items > viewport_height {
        format!(" [{}/{}] ↕ ", app.ghost_cursor_index + 1, total_items)
    } else if total_items > 0 {
        format!(" [{}/{}] ", app.ghost_cursor_index + 1, total_items)
    } else {
        String::new()
    };

    let table = Table::new(rows, widths).header(header).block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(Line::from(vec![
                Span::styled(
                    " Open Unlinked Files Holding Space",
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
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
        Span::styled(
            format!(" Total Ghost Space Held: {}", format_size(total_held)),
            Style::default()
                .fg(Color::LightRed)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" │ "),
        Span::styled(
            format!("Ghost Files: {}", app.deleted_open_files.len()),
            Style::default().fg(Color::White),
        ),
        Span::raw(" │ "),
        Span::styled(
            "To reclaim: restart or stop the corresponding process",
            Style::default().fg(Color::DarkGray),
        ),
    ]);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::DarkGray));

    f.render_widget(Paragraph::new(line).block(block), area);
}

fn render_top_files(f: &mut Frame, app: &App, area: Rect) {
    let header = Row::new(vec!["Rank", "Size", "Safety", "Path"]);
    let path_budget = area.width.saturating_sub(28) as usize;
    let rows: Vec<Row> = app
        .top_files
        .iter()
        .enumerate()
        .map(|(index, top)| {
            let size = if app.apparent_size {
                format_size(top.size)
            } else {
                format_size(top.disk_usage)
            };
            Row::new(vec![
                format!("{:>4}", index + 1),
                size,
                top.safety.badge().to_string(),
                truncate_path(&top.path.to_string_lossy(), path_budget.max(8)),
            ])
        })
        .collect();
    let widths = [
        Constraint::Length(6),
        Constraint::Length(12),
        Constraint::Length(9),
        Constraint::Min(10),
    ];
    let table = Table::new(rows, widths)
        .header(header)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(Color::LightCyan))
                .title(
                    " 🏆 Top 50 Largest Files (Enter: jump │ t: trash │ d: delete │ Esc: back) ",
                ),
        )
        .row_highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");
    let mut state = ratatui::widgets::TableState::default();
    state.select(Some(app.top_cursor));
    f.render_stateful_widget(table, area, &mut state);
}

fn render_janitor(f: &mut Frame, app: &App, area: Rect) {
    let scope_label = match app.janitor_scope {
        crate::ui::app::JanitorScope::Current => "current folder",
        crate::ui::app::JanitorScope::Global => "whole scan",
    };
    let title = format!(
        " 🧹 System Janitor — {} (Tab: scope │ Space: toggle │ Enter: trash │ d: delete) — selected {} ",
        scope_label,
        format_size(app.janitor_selected_bytes),
    );
    // Viewport window around the cursor; only visible rows are constructed,
    // so large scans cost O(page) per redraw instead of O(tree).
    let page = area.height.saturating_sub(5).max(1) as usize;
    let total = app.janitor_row_count();
    let start = compute_scroll_window(app.janitor_cursor, app.janitor_offset.get(), page, total);
    app.janitor_offset.set(start);
    let mut rows: Vec<Row> = Vec::new();
    // Start at the indexed category containing the window; stop once filled.
    let offsets = app.janitor_offsets();
    let first_cat = offsets
        .partition_point(|&index| index <= start)
        .saturating_sub(1);
    // Rows before the window are skipped by index, never constructed. `flat`
    // resumes at `start` so the push macro stays correct.
    let mut flat = start;
    let mut skip_items = start - offsets.get(first_cat).copied().unwrap_or(0);
    let show_first_header = skip_items == 0;
    skip_items = skip_items.saturating_sub(1); // The hidden header row.
                                               // Windowed push: only visible rows are constructed (O(page) per redraw).
    macro_rules! push_row {
        ($row:expr) => {
            if flat >= start && rows.len() < page {
                rows.push($row);
            }
            flat += 1;
        };
    }
    for (cat_idx, cat) in app.janitor_cats.iter().enumerate().skip(first_cat) {
        if rows.len() >= page {
            break;
        }
        let marker = if cat.items.is_empty() {
            "  "
        } else if cat.expanded {
            "▾ "
        } else {
            "▸ "
        };
        // The first category may start mid-items; its header is already above.
        let header_visible = cat_idx > first_cat || show_first_header;
        if header_visible {
            push_row!(Row::new(vec![
                "".to_string(),
                format!("{marker}{}", cat.title),
                if cat.items.is_empty() {
                    "—".to_string()
                } else {
                    format_size(cat.total)
                },
            ]));
        }
        if cat.expanded {
            let skip = if cat_idx == first_cat { skip_items } else { 0 };
            for item in cat.items.iter().skip(skip) {
                if rows.len() >= page {
                    break;
                }
                let checkbox = if item.selected { "[x]" } else { "[ ]" };
                push_row!(Row::new(vec![
                    checkbox.to_string(),
                    format!("    {}", item.display),
                    format_size(item.size),
                ]));
            }
        }
    }
    if total == 0 {
        rows.push(Row::new(vec![
            "".to_string(),
            "No cleanable items in scope".to_string(),
            "".to_string(),
        ]));
    }
    let widths = [
        Constraint::Length(5),
        Constraint::Min(10),
        Constraint::Length(12),
    ];
    let table = Table::new(rows, widths)
        .header(Row::new(vec!["Sel", "Category / Folder", "Size"]))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(Color::LightCyan))
                .title(title),
        )
        .row_highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");
    let mut state = ratatui::widgets::TableState::default();
    state.select(Some(app.janitor_cursor.saturating_sub(start)));
    f.render_stateful_widget(table, area, &mut state);
}

fn render_confirm_modal(f: &mut Frame, app: &App, screen: Rect) {
    let action = match &app.pending_action {
        Some(a) => a,
        None => return,
    };

    // Multi-target batches get an inspectable frozen target list; the window
    // shows a page with scroll hints instead of rebuilding per frame.
    let show_targets = !app.action_safety_blocked && app.action_targets.len() > 1;
    let popup_width = 65.min(screen.width.saturating_sub(4));
    let popup_height = if show_targets { 21 } else { 14 }.min(screen.height.saturating_sub(4));

    let area = Rect {
        x: (screen.width.saturating_sub(popup_width)) / 2,
        y: (screen.height.saturating_sub(popup_height)) / 2,
        width: popup_width,
        height: popup_height,
    };

    f.render_widget(Clear, area);

    let (title, border_color, prompt_line, mut info_lines) = if app.action_safety_blocked {
        let title = " ⛔  DELETION BLOCKED — SYSTEM PROTECTION ";
        let border_color = Color::Red;

        let prompt = Line::from(vec![Span::styled(
            " [Esc / n] Dismiss & Cancel ",
            Style::default()
                .bg(Color::Red)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )]);

        let mut infos = vec![
            Line::from(vec![Span::styled(
                "CRITICAL SYSTEM SAFETY GUARD ACTIVATED",
                Style::default()
                    .fg(Color::LightRed)
                    .add_modifier(Modifier::BOLD),
            )]),
            Line::from(""),
            Line::from(Span::styled(
                "⛔ Selected target is a critical system directory or file.",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "   Deleting core system paths (/usr, /etc, /boot, /bin, etc.)",
                Style::default().fg(Color::Yellow),
            )),
            Line::from(Span::styled(
                "   will severely damage or brick your operating system.",
                Style::default().fg(Color::Yellow),
            )),
            Line::from(Span::styled(
                "⛔ ghostdu actively blocks deletion of protected system files.",
                Style::default().fg(Color::White),
            )),
            Line::from(""),
        ];

        if let Some(first) = app.action_targets.first() {
            infos.push(Line::from(Span::styled(
                format!("Protected Path: {}", first.to_string_lossy()),
                Style::default().fg(Color::LightYellow),
            )));
        }

        (title, border_color, prompt, infos)
    } else {
        match action {
            ConfirmAction::MoveToTrash => {
                let count = app.action_targets.len();
                let sz = format_size(app.action_total_size);
                let title = " 🗑️  MOVE TO WASTEBIN (TRASH) ";
                let border_color = Color::Green;

                let prompt = Line::from(vec![
                    Span::styled(
                        " [y] Move to Wastebin ",
                        Style::default()
                            .bg(Color::Green)
                            .fg(Color::Black)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw("    "),
                    Span::styled(
                        " [n / Esc] Cancel ",
                        Style::default().bg(Color::DarkGray).fg(Color::White),
                    ),
                ]);

                let mut infos = vec![
                    Line::from(vec![Span::styled(
                        format!("Move {} item(s) (total {}) to Wastebin?", count, sz),
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD),
                    )]),
                    Line::from(""),
                ];

                if app.action_has_recheck {
                    infos.push(Line::from(Span::styled(
                        "🟡 CAUTION: Targets include dependencies, models, or VM images.",
                        Style::default()
                            .fg(Color::LightYellow)
                            .add_modifier(Modifier::BOLD),
                    )));
                    infos.push(Line::from(Span::styled(
                        "   These can be regenerated, but will take bandwidth or time to restore.",
                        Style::default().fg(Color::DarkGray),
                    )));
                } else {
                    infos.push(Line::from(Span::styled(
                        "🟢 SAFE: Selected items are temporary caches or trash.",
                        Style::default().fg(Color::LightGreen),
                    )));
                    infos.push(Line::from(Span::styled(
                        "✔ Restore anytime using Dolphin, Nautilus, or trash-restore",
                        Style::default().fg(Color::DarkGray),
                    )));
                }
                infos.push(Line::from(""));

                if let Some(first) = app.action_targets.first() {
                    infos.push(Line::from(Span::styled(
                        format!("Target: {}", first.to_string_lossy()),
                        Style::default().fg(Color::Yellow),
                    )));
                }

                (title, border_color, prompt, infos)
            }
            ConfirmAction::PermanentDelete => {
                let count = app.action_targets.len();
                let sz = format_size(app.action_total_size);
                let title = " ⚠️  PERMANENT DESTRUCTION (CANNOT BE UNDONE) ";
                let border_color = Color::Red;

                let prompt = Line::from(vec![
                    Span::styled(
                        " [y] PERMANENTLY REMOVE ",
                        Style::default()
                            .bg(Color::Red)
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw("    "),
                    Span::styled(
                        " [n / Esc] Cancel ",
                        Style::default().bg(Color::DarkGray).fg(Color::White),
                    ),
                ]);

                let mut infos = vec![
                    Line::from(vec![Span::styled(
                        format!("PERMANENTLY ERASE {} item(s) (total {})?", count, sz),
                        Style::default()
                            .fg(Color::LightRed)
                            .add_modifier(Modifier::BOLD),
                    )]),
                    Line::from(""),
                    Line::from(Span::styled(
                        "❌ WARNING: This bypasses Wastebin and deletes data FOREVER!",
                        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                    )),
                ];

                if app.action_has_recheck {
                    infos.push(Line::from(Span::styled(
                        "🟡 CAUTION: Targets include dependencies, models, or VM images.",
                        Style::default()
                            .fg(Color::LightYellow)
                            .add_modifier(Modifier::BOLD),
                    )));
                    infos.push(Line::from(Span::styled(
                        "   These will require redownload or rebuilding if deleted.",
                        Style::default().fg(Color::DarkGray),
                    )));
                } else {
                    infos.push(Line::from(Span::styled(
                        "🟢 SAFE: Selected items are temporary caches or crash files.",
                        Style::default().fg(Color::LightGreen),
                    )));
                }
                infos.push(Line::from(""));

                if let Some(first) = app.action_targets.first() {
                    infos.push(Line::from(Span::styled(
                        format!("Target: {}", first.to_string_lossy()),
                        Style::default().fg(Color::Yellow),
                    )));
                }

                (title, border_color, prompt, infos)
            }
            ConfirmAction::DockerPrune => {
                let sz = format_size(app.action_total_size);
                let title = " 🐳  DOCKER SYSTEM PRUNE ";
                let border_color = Color::Yellow;

                let prompt = Line::from(vec![
                    Span::styled(
                        " [y] Prune Dangling Resources ",
                        Style::default()
                            .bg(Color::Yellow)
                            .fg(Color::Black)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw("    "),
                    Span::styled(
                        " [n / Esc] Cancel ",
                        Style::default().bg(Color::DarkGray).fg(Color::White),
                    ),
                ]);

                let infos = vec![
                    Line::from(Span::styled(
                        format!("Reclaim up to {} of Docker ghost data?", sz),
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD),
                    )),
                    Line::from(""),
                    Line::from(Span::styled(
                        "🟢 SAFE: Only removes stopped containers, dangling images & build cache.",
                        Style::default().fg(Color::LightGreen),
                    )),
                    Line::from(Span::styled(
                        "Running containers and named volumes will NOT be affected.",
                        Style::default().fg(Color::DarkGray),
                    )),
                    Line::from(""),
                ];

                (title, border_color, prompt, infos)
            }
            ConfirmAction::KillProcess { pid, name, .. } => {
                let title = " ☠️  TERMINATE PROCESS (GHOST FILE) ";
                let border_color = Color::Red;

                let prompt = Line::from(vec![
                    Span::styled(
                        " [1] SIGTERM ",
                        Style::default()
                            .bg(Color::Yellow)
                            .fg(Color::Black)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw("    "),
                    Span::styled(
                        " [2] SIGKILL ",
                        Style::default()
                            .bg(Color::Red)
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw("    "),
                    Span::styled(
                        " [n / Esc] Cancel ",
                        Style::default().bg(Color::DarkGray).fg(Color::White),
                    ),
                ]);

                let infos = vec![
                    Line::from(Span::styled(
                        format!("Terminate '{name}' (PID {pid})?"),
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD),
                    )),
                    Line::from(""),
                    Line::from(Span::styled(
                        "Freed ghost space returns to disk once it exits.",
                        Style::default().fg(Color::LightGreen),
                    )),
                    Line::from(Span::styled(
                        "Prefer SIGTERM: SIGKILL on databases risks corruption.",
                        Style::default().fg(Color::Yellow),
                    )),
                    Line::from(""),
                ];

                (title, border_color, prompt, infos)
            }
        }
    };

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(4), Constraint::Length(2)])
        .margin(1)
        .split(area);

    if show_targets {
        const PAGE: usize = 6;
        let total = app.action_targets.len();
        let offset = app.confirm_list_offset.min(total.saturating_sub(1));
        let scope = if app.previous_view == ActiveView::Janitor {
            match app.janitor_scope {
                crate::ui::app::JanitorScope::Current => " — scope: current folder",
                crate::ui::app::JanitorScope::Global => " — scope: whole scan",
            }
        } else {
            ""
        };
        info_lines.push(Line::from(Span::styled(
            format!("Targets ({total}){scope} — ↑↓ to inspect:"),
            Style::default()
                .fg(Color::LightCyan)
                .add_modifier(Modifier::BOLD),
        )));
        if offset > 0 {
            info_lines.push(Line::from(Span::styled(
                format!("  … {} more above", offset),
                Style::default().fg(Color::DarkGray),
            )));
        }
        let budget = (popup_width as usize).saturating_sub(10).max(8);
        for target in app.action_targets.iter().skip(offset).take(PAGE) {
            info_lines.push(Line::from(format!(
                "  • {}",
                truncate_path(&target.to_string_lossy(), budget)
            )));
        }
        let shown = offset + PAGE.min(total.saturating_sub(offset));
        if shown < total {
            info_lines.push(Line::from(Span::styled(
                format!("  … {} more below", total - shown),
                Style::default().fg(Color::DarkGray),
            )));
        }
    }

    let modal_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(border_color))
        .title(title);

    f.render_widget(modal_block, area);
    f.render_widget(Paragraph::new(info_lines), layout[0]);
    f.render_widget(
        Paragraph::new(prompt_line).alignment(Alignment::Center),
        layout[1],
    );
}

fn render_help_modal(f: &mut Frame, screen: Rect) {
    let is_wide = screen.width >= 72;
    let popup_width = if is_wide {
        72.min(screen.width.saturating_sub(2))
    } else {
        screen.width.saturating_sub(2)
    };
    let popup_height = if is_wide {
        18.min(screen.height.saturating_sub(2))
    } else {
        22.min(screen.height.saturating_sub(2))
    };

    let area = Rect {
        x: (screen.width.saturating_sub(popup_width)) / 2,
        y: (screen.height.saturating_sub(popup_height)) / 2,
        width: popup_width,
        height: popup_height,
    };

    f.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::LightCyan))
        .title(" 👻 ghostdu Keyboard Shortcuts (? or Esc to close) ");

    if is_wide && area.height >= 14 {
        let inner = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .margin(1)
            .split(area);

        let left_col = vec![
            Line::from(Span::styled(
                "NAVIGATION & BROWSING",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from("  j / Down     Cursor down"),
            Line::from("  k / Up       Cursor up"),
            Line::from("  PgDn / PgUp  Page down / up"),
            Line::from("  Enter / l    Enter folder"),
            Line::from("  Bksp / h     Parent folder (to /)"),
            Line::from("  \\  /  ~      Root (/) / Home (~)"),
            Line::from("  Home / End   Top / Bottom"),
            Line::from(""),
            Line::from("  i            Item & Disk info"),
            Line::from("  T / J        Top 50 files / Janitor"),
            Line::from("  ! / o / y    Shell here / Open / Copy path"),
            Line::from("  s            Cycle sort order"),
            Line::from("  A            Toggle Apparent size"),
            Line::from("  c            Toggle Safe-only filter (🟢)"),
            Line::from("  /            Live search / filter"),
            Line::from("  q / Ctrl+C   Quit ghostdu"),
        ];

        let right_col = vec![
            Line::from(Span::styled(
                "SELECTION & REMOVAL",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from("  Space        Select / deselect item"),
            Line::from("  a            Select all visible"),
            Line::from("  t / w        Move to Wastebin"),
            Line::from("  d / D        Permanent delete"),
            Line::from(""),
            Line::from(Span::styled(
                "SAFETY TIERS & GHOST",
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from("  🟢 Safe      Caches, Trash, Coredumps"),
            Line::from("  🟡 Recheck   Deps, AI models, ISOs"),
            Line::from("  ⚪ User      Code & personal files"),
            Line::from("  🔴 System    Protected (locked delete)"),
            Line::from("  Tab / g      Ghost / Docker view"),
            Line::from("  ?            Toggle this help"),
        ];

        f.render_widget(block, area);
        f.render_widget(Paragraph::new(left_col), inner[0]);
        f.render_widget(Paragraph::new(right_col), inner[1]);
    } else {
        let help_text = vec![
            Line::from(Span::styled(
                "NAV: ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from("  j/k: Down/Up │ Enter: Enter │ Bksp: Up │ \\: Root"),
            Line::from("  r: Refresh folder │ R / F5: Full rescan"),
            Line::from(""),
            Line::from(Span::styled(
                "ACTIONS: ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from("  Space: Sel │ t/w: Trash │ d/D: Delete │ i: Info"),
            Line::from("  !: Shell │ o: Open │ y: Copy │ K: Kill (ghost)"),
            Line::from(""),
            Line::from(Span::styled(
                "SAFETY & MODES: ",
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from("  🟢 Safe │ 🟡 Recheck │ ⚪ User │ 🔴 System"),
            Line::from("  c: Safe-Only │ s: Sort │ A: Apparent │ q: Quit"),
        ];
        let widget = Paragraph::new(help_text)
            .block(block)
            .wrap(Wrap { trim: false });
        f.render_widget(widget, area);
    }
}

fn render_item_info_modal(f: &mut Frame, app: &App, screen: Rect) {
    let popup_width = 76.min(screen.width.saturating_sub(2));
    let popup_height = 20.min(screen.height.saturating_sub(2));

    let area = Rect {
        x: (screen.width.saturating_sub(popup_width)) / 2,
        y: (screen.height.saturating_sub(popup_height)) / 2,
        width: popup_width,
        height: popup_height,
    };

    f.render_widget(Clear, area);

    let info = match &app.item_info {
        Some(i) => i,
        None => return,
    };

    let is_compact = popup_height < 18;
    let mut lines = Vec::new();

    // Section 1: Item Identification
    let type_badge = if info.is_symlink {
        Span::styled(
            " SYMLINK ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
    } else if info.is_dir {
        Span::styled(
            " DIRECTORY ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Blue)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(
            " FILE ",
            Style::default().fg(Color::White).bg(Color::DarkGray),
        )
    };

    let max_p = (popup_width as usize).saturating_sub(10);
    let trunc_p = truncate_path(&info.full_path.to_string_lossy(), max_p);

    lines.push(Line::from(vec![
        Span::styled(
            "Name: ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            &info.name,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        type_badge,
    ]));

    lines.push(Line::from(vec![
        Span::styled(
            "Path: ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(trunc_p, Style::default().fg(Color::White)),
    ]));

    if info.ghost_kind.is_ghost() {
        lines.push(Line::from(vec![
            Span::styled(
                "Category: ",
                Style::default()
                    .fg(Color::LightMagenta)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                info.ghost_kind.label(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  ["),
            Span::styled(
                info.ghost_kind.badge(),
                Style::default().fg(Color::LightCyan),
            ),
            Span::raw("]"),
        ]));
    }

    let safety_color = match info.delete_safety {
        DeleteSafety::Safe => Color::LightGreen,
        DeleteSafety::Recheck => Color::LightYellow,
        DeleteSafety::System => Color::LightRed,
        DeleteSafety::UserData => Color::White,
    };

    lines.push(Line::from(vec![
        Span::styled(
            "Safety:   ",
            Style::default()
                .fg(safety_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(
                "{} {}",
                info.delete_safety.glyph(),
                info.delete_safety.label()
            ),
            Style::default()
                .fg(safety_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" — {}", info.delete_safety.description()),
            Style::default().fg(Color::DarkGray),
        ),
    ]));

    if !is_compact {
        lines.push(Line::from(""));
    }

    // Section 2: Size & Items
    lines.push(Line::from(vec![
        Span::styled(
            "Disk Usage:    ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(
                "{} ({} B, {} blks)",
                format_size(info.disk_usage),
                info.disk_usage,
                info.blocks_512
            ),
            Style::default().fg(Color::White),
        ),
    ]));

    lines.push(Line::from(vec![
        Span::styled(
            "Apparent Size: ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(
                "{} ({} B)",
                format_size(info.apparent_size),
                info.apparent_size
            ),
            Style::default().fg(Color::White),
        ),
    ]));

    if info.is_dir {
        lines.push(Line::from(vec![
            Span::styled(
                "Items Inside:  ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format_count(info.items_count),
                Style::default().fg(Color::White),
            ),
        ]));
    }

    if !is_compact {
        lines.push(Line::from(""));
    }

    // Section 3: Metadata & Permissions
    lines.push(Line::from(vec![
        Span::styled("Mode: ", Style::default().fg(Color::Green)),
        Span::styled(
            format!("{} ({:04o})", info.mode_str, info.mode_octal),
            Style::default().fg(Color::White),
        ),
        Span::raw(" │ "),
        Span::styled(
            format!("UID {}/GID {}", info.uid, info.gid),
            Style::default().fg(Color::White),
        ),
        Span::raw(" │ "),
        Span::styled(
            format!("Ino {}", info.ino),
            Style::default().fg(Color::White),
        ),
    ]));

    if !is_compact {
        lines.push(Line::from(vec![
            Span::styled("Modified: ", Style::default().fg(Color::Green)),
            Span::styled(&info.modified_str, Style::default().fg(Color::White)),
        ]));
    }

    // Section 4: Filesystem Details
    if let Some(ref fs) = info.fs_info {
        if !is_compact {
            lines.push(Line::from(""));
        }
        lines.push(Line::from(vec![
            Span::styled(
                "FS: ",
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(&fs.device, Style::default().fg(Color::White)),
            Span::styled(
                format!(" ({}) on {}", fs.fs_type, fs.mount_point.display()),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
        lines.push(Line::from(vec![
            Span::styled("FS Cap: ", Style::default().fg(Color::Magenta)),
            Span::styled(
                format_size(fs.total_bytes),
                Style::default().fg(Color::White),
            ),
            Span::raw(" │ "),
            Span::styled(
                format!(
                    "Used: {} ({:.0}%)",
                    format_size(fs.used_bytes),
                    fs.use_percent
                ),
                Style::default().fg(Color::White),
            ),
            Span::raw(" │ "),
            Span::styled("Free: ", Style::default().fg(Color::Magenta)),
            Span::styled(
                format_size(fs.avail_bytes),
                Style::default()
                    .fg(Color::LightGreen)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
    }

    if !is_compact {
        lines.push(Line::from(""));
    }
    lines.push(Line::from(Span::styled(
        "Press 'i', 'q', 'r', Enter, or Esc to close / refresh",
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC),
    )));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" ℹ Item & Filesystem Information ");

    let widget = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    f.render_widget(widget, area);
}

fn render_footer(f: &mut Frame, app: &App, area: Rect) {
    if area.height == 0 || area.width == 0 {
        return;
    }

    if let Some(status) = app.current_status() {
        // " 📢 " occupies 4 terminal cells (1 space + 2-wide emoji + 1 space)
        let max_status_len = (area.width as usize).saturating_sub(4);
        let display_status = crate::fs::entry::truncate_end_by_width(status, max_status_len);
        let status_line = Line::from(vec![
            Span::styled(" 📢 ", Style::default().fg(Color::Yellow)),
            Span::styled(
                display_status,
                Style::default()
                    .fg(Color::LightGreen)
                    .add_modifier(Modifier::BOLD),
            ),
        ]);
        f.render_widget(Paragraph::new(status_line), area);
        return;
    }

    let footer_spans: Vec<Span> = match app.active_view {
        ActiveView::Filesystem => {
            let candidate_keys: Vec<(&str, Color, Color)> = if area.width >= 115 {
                vec![
                    ("[?] Help", Color::White, Color::DarkGray),
                    ("[i] Info", Color::Black, Color::Cyan),
                    ("[r] Refresh", Color::Black, Color::Green),
                    ("[R] Rescan", Color::White, Color::Rgb(70, 70, 90)),
                    ("[Bksp] Up", Color::White, Color::Rgb(60, 60, 75)),
                    ("[\\] Root", Color::White, Color::Rgb(75, 60, 60)),
                    ("[Tab] Ghost", Color::Black, Color::Magenta),
                    ("[G] Filter", Color::Black, Color::LightCyan),
                    ("[Space] Sel", Color::White, Color::Blue),
                    ("[t] Trash", Color::Black, Color::Green),
                    ("[d] Del", Color::White, Color::Red),
                    ("[s] Sort", Color::White, Color::DarkGray),
                    ("[q] Quit", Color::White, Color::DarkGray),
                ]
            } else if area.width >= 80 {
                vec![
                    ("[?] Help", Color::White, Color::DarkGray),
                    ("[i] Info", Color::Black, Color::Cyan),
                    ("[r] Refresh", Color::Black, Color::Green),
                    ("[Bksp] Up", Color::White, Color::Rgb(60, 60, 75)),
                    ("[Space] Sel", Color::White, Color::Blue),
                    ("[t] Trash", Color::Black, Color::Green),
                    ("[d] Del", Color::White, Color::Red),
                    ("[Tab] Ghost", Color::Black, Color::Magenta),
                    ("[q] Quit", Color::White, Color::DarkGray),
                ]
            } else if area.width >= 55 {
                vec![
                    ("[?] Help", Color::White, Color::DarkGray),
                    ("[r] Refresh", Color::Black, Color::Green),
                    ("[Bksp] Up", Color::White, Color::Rgb(60, 60, 75)),
                    ("[t] Trash", Color::Black, Color::Green),
                    ("[d] Del", Color::White, Color::Red),
                    ("[q] Quit", Color::White, Color::DarkGray),
                ]
            } else {
                vec![
                    ("[?] Help", Color::White, Color::DarkGray),
                    ("[r] Ref", Color::Black, Color::Green),
                    ("[t] Trash", Color::Black, Color::Green),
                    ("[q] Quit", Color::White, Color::DarkGray),
                ]
            };

            let mut spans = Vec::new();
            for (idx, (label, fg, bg)) in candidate_keys.into_iter().enumerate() {
                if idx > 0 {
                    spans.push(Span::raw(" "));
                }
                spans.push(Span::styled(
                    format!(" {} ", label),
                    Style::default().fg(fg).bg(bg),
                ));
            }
            spans
        }
        ActiveView::GhostInspector => {
            let candidate_keys: Vec<(&str, Color, Color)> = if area.width >= 80 {
                vec![
                    ("[Tab] Explorer", Color::Black, Color::LightCyan),
                    ("[1/2] Tab", Color::White, Color::DarkGray),
                    ("[p] Prune Docker", Color::Black, Color::Yellow),
                    ("[K] Kill proc", Color::Black, Color::Red),
                    ("[r] Refresh", Color::Black, Color::Green),
                    ("[q] Back", Color::White, Color::DarkGray),
                ]
            } else {
                vec![
                    ("[Tab] Explorer", Color::Black, Color::LightCyan),
                    ("[p] Prune", Color::Black, Color::Yellow),
                    ("[K] Kill", Color::Black, Color::Red),
                    ("[r] Ref", Color::Black, Color::Green),
                    ("[q] Back", Color::White, Color::DarkGray),
                ]
            };
            let mut spans = Vec::new();
            for (idx, (label, fg, bg)) in candidate_keys.into_iter().enumerate() {
                if idx > 0 {
                    spans.push(Span::raw(" "));
                }
                spans.push(Span::styled(
                    format!(" {} ", label),
                    Style::default().fg(fg).bg(bg),
                ));
            }
            spans
        }
        ActiveView::TopFiles => {
            let candidate_keys: Vec<(&str, Color, Color)> = if area.width >= 80 {
                vec![
                    ("[Enter] Jump to file", Color::Black, Color::LightCyan),
                    ("[t] Trash", Color::Black, Color::Yellow),
                    ("[d] Delete", Color::Black, Color::Red),
                    ("[Esc] Back", Color::White, Color::DarkGray),
                ]
            } else {
                vec![
                    ("[Enter] Jump", Color::Black, Color::LightCyan),
                    ("[t/d] Act", Color::Black, Color::Yellow),
                    ("[Esc] Back", Color::White, Color::DarkGray),
                ]
            };
            let mut spans = Vec::new();
            for (idx, (label, fg, bg)) in candidate_keys.into_iter().enumerate() {
                if idx > 0 {
                    spans.push(Span::raw(" "));
                }
                spans.push(Span::styled(
                    format!(" {} ", label),
                    Style::default().fg(fg).bg(bg),
                ));
            }
            spans
        }
        ActiveView::Janitor => {
            let candidate_keys: Vec<(&str, Color, Color)> = if area.width >= 90 {
                vec![
                    ("[Space] Toggle", Color::Black, Color::LightCyan),
                    ("[Tab] Scope", Color::White, Color::DarkGray),
                    ("[Enter] Trash", Color::Black, Color::Yellow),
                    ("[d] Delete", Color::Black, Color::Red),
                    ("[Esc] Back", Color::White, Color::DarkGray),
                ]
            } else {
                vec![
                    ("[Spc] Tog", Color::Black, Color::LightCyan),
                    ("[Tab] Scope", Color::White, Color::DarkGray),
                    ("[Enter] Trash", Color::Black, Color::Yellow),
                    ("[Esc] Back", Color::White, Color::DarkGray),
                ]
            };
            let mut spans = Vec::new();
            for (idx, (label, fg, bg)) in candidate_keys.into_iter().enumerate() {
                if idx > 0 {
                    spans.push(Span::raw(" "));
                }
                spans.push(Span::styled(
                    format!(" {} ", label),
                    Style::default().fg(fg).bg(bg),
                ));
            }
            spans
        }
        ActiveView::ConfirmModal => {
            // The kill confirmation answers 1/2, not y.
            if matches!(&app.pending_action, Some(ConfirmAction::KillProcess { .. })) {
                if area.width >= 80 {
                    vec![
                        Span::styled(
                            " [1] SIGTERM ",
                            Style::default().fg(Color::Black).bg(Color::Yellow),
                        ),
                        Span::raw(" "),
                        Span::styled(
                            " [2] SIGKILL ",
                            Style::default().fg(Color::White).bg(Color::Red),
                        ),
                        Span::raw(" "),
                        Span::styled(
                            " [n / Esc] Cancel ",
                            Style::default().fg(Color::White).bg(Color::DarkGray),
                        ),
                    ]
                } else {
                    vec![
                        Span::styled(
                            " [1] TERM ",
                            Style::default().fg(Color::Black).bg(Color::Yellow),
                        ),
                        Span::raw(" "),
                        Span::styled(
                            " [2] KILL ",
                            Style::default().fg(Color::White).bg(Color::Red),
                        ),
                        Span::raw(" "),
                        Span::styled(
                            " [Esc] ",
                            Style::default().fg(Color::White).bg(Color::DarkGray),
                        ),
                    ]
                }
            } else {
                vec![
                    Span::styled(
                        " [y] Confirm Action ",
                        Style::default().fg(Color::Black).bg(Color::Yellow),
                    ),
                    Span::raw(" "),
                    Span::styled(
                        " [n / Esc] Cancel ",
                        Style::default().fg(Color::White).bg(Color::DarkGray),
                    ),
                ]
            }
        }
        ActiveView::HelpModal => vec![Span::styled(
            " [? / Esc] Close Help ",
            Style::default().fg(Color::White).bg(Color::DarkGray),
        )],
        ActiveView::ItemInfoModal => vec![
            Span::styled(
                " [i / q / Esc] Close Info ",
                Style::default().fg(Color::White).bg(Color::DarkGray),
            ),
            Span::raw(" "),
            Span::styled(
                " [r] Refresh ",
                Style::default().fg(Color::Black).bg(Color::Green),
            ),
        ],
    };

    f.render_widget(Paragraph::new(Line::from(footer_spans)), area);
}

#[cfg(test)]
mod render_sweep_tests {
    use super::*;
    use crate::fs::scanner::scan_directory;
    use crate::ghost::{DeletedOpenFile, DockerDiskInfo};
    use crate::ghost::docker::DockerItemSummary;
    use crate::ui::app::{GhostFilterMode, SortMode};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::sync::{atomic::AtomicBool, Arc};

    struct Fixture {
        _dir: tempfile::TempDir,
        app: App,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(dir.path().join("a.txt"), "aaaaaaaaaa").unwrap();
        std::fs::write(sub.join("b.txt"), "b").unwrap();
        // Safe-classified content exercises safe totals, badges, and filters.
        let cache = dir.path().join(".cache").join("thumbnails");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("thumb.png"), vec![0u8; 1000]).unwrap();
        std::os::unix::fs::symlink("a.txt", dir.path().join("link.txt")).unwrap();
        let root = scan_directory(dir.path(), None, Arc::new(AtomicBool::new(false))).unwrap();
        Fixture {
            app: App::new(root),
            _dir: dir,
        }
    }

    fn render(app: &App, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render_ui(f, app)).unwrap();
        terminal.backend().to_string()
    }

    fn available_docker() -> DockerDiskInfo {
        DockerDiskInfo {
            is_available: true,
            error_message: None,
            images_total_size: 100,
            images_reclaimable_size: 50,
            images_count: 2,
            containers_total_size: 30,
            containers_reclaimable_size: 10,
            containers_count: 1,
            volumes_total_size: 20,
            volumes_reclaimable_size: 0,
            volumes_count: 1,
            build_cache_total_size: 40,
            build_cache_reclaimable_size: 40,
            items: vec![
                DockerItemSummary {
                    category: "Image",
                    id_or_name: "abc123".to_string(),
                    size: 60,
                    is_reclaimable: true,
                    details: "dangling".to_string(),
                },
                DockerItemSummary {
                    category: "BuildCache",
                    id_or_name: "bc1".to_string(),
                    size: 40,
                    is_reclaimable: true,
                    details: "layer".to_string(),
                },
                DockerItemSummary {
                    category: "Container",
                    id_or_name: "c1".to_string(),
                    size: 30,
                    is_reclaimable: false,
                    details: "running".to_string(),
                },
            ],
        }
    }

    #[test]
    fn filesystem_sizes_and_filters() {
        let mut fx = fixture();
        let out = render(&fx.app, 100, 30);
        assert!(out.contains("a.txt"));
        // Rich state so conditional spans appear at every header size.
        fx.app.toggle_selection();
        fx.app.safe_only_filter = true;
        fx.app.search_query = "a".to_string();
        // Synthetic rows of every ghost kind + error/symlink/dir shapes, so
        // the badge match and name styles render deterministically.
        {
            use crate::fs::entry::{DeleteSafety, FileEntry, GhostKind};
            use crate::ghost::classify_safety;
            use std::path::PathBuf;
            let kinds = [
                GhostKind::None,
                GhostKind::DockerOverlay,
                GhostKind::DockerVolume,
                GhostKind::DockerContainer,
                GhostKind::DockerBuildkit,
                GhostKind::DockerUser,
                GhostKind::PodmanUser,
                GhostKind::BuildCache,
                GhostKind::PackageCache,
                GhostKind::DeletedOpen,
                GhostKind::Trash,
                GhostKind::LogFiles,
                GhostKind::Flatpak,
                GhostKind::SnapPackage,
                GhostKind::DependencyTree,
                GhostKind::GamingCompat,
                GhostKind::AiModel,
                GhostKind::VmOrIso,
                GhostKind::BrowserCache,
                GhostKind::CoreDump,
                GhostKind::SystemSnapshot,
            ];
            for (i, kind) in kinds.iter().enumerate() {
                let path = PathBuf::from(format!("/synth/k{i}"));
                let safety = classify_safety(&path, *kind);
                let mut row = FileEntry::new_file(
                    format!("k{i}"),
                    path,
                    100,
                    4096,
                    false,
                    1,
                    1000 + i as u64,
                    *kind,
                    safety,
                );
                if i == 0 {
                    row.has_err = true;
                }
                fx.app.root_entry.children.push(row);
            }
            fx.app.root_entry.children.push(FileEntry::new_file(
                "link".to_string(),
                PathBuf::from("/synth/link"),
                10,
                10,
                true,
                1,
                2000,
                GhostKind::None,
                DeleteSafety::UserData,
            ));
            // A System-safety row exercises the protected-name embedding.
            fx.app.root_entry.children.push(FileEntry::new_file(
                "hosts".to_string(),
                PathBuf::from("/etc/hosts"),
                10,
                10,
                false,
                1,
                2001,
                GhostKind::None,
                DeleteSafety::System,
            ));
            // Render again unfiltered so every synthetic row is visible.
            fx.app.safe_only_filter = false;
            fx.app.search_query.clear();
            fx.app.is_searching = false;
            for (width, height) in [(100, 30), (40, 20), (100, 12)] {
                let _ = render(&fx.app, width, height);
            }
        }
        for (width, height) in [
            (100, 30),
            (40, 20),
            (100, 12),
            (60, 7),
            (100, 4),
            (30, 30),
            (50, 30),
            (70, 30),
            (140, 30),
        ] {
            let _ = render(&fx.app, width, height);
        }
        // Narrow, short, and tiny terminals take compact paths.
        let tiny = render(&fx.app, 20, 2);
        assert!(tiny.contains("Terminal too small"));
        // Display modes each change the render.
        let app = &mut fx.app;
        app.safe_only_filter = false;
        app.ghost_filter = GhostFilterMode::HideGhost;
        let _ = render(app, 100, 30);
        app.ghost_filter = GhostFilterMode::GhostOnly;
        let _ = render(app, 100, 30);
        app.ghost_filter = GhostFilterMode::ShowAll;
        app.apparent_size = true;
        let _ = render(app, 100, 30);
        app.apparent_size = false;
        for mode in [
            SortMode::BySizeDesc,
            SortMode::BySizeAsc,
            SortMode::ByName,
            SortMode::ByItems,
        ] {
            app.sort_mode = mode;
            let _ = render(app, 100, 30);
        }
        app.is_searching = true;
        app.search_query = "a".to_string();
        let _ = render(app, 100, 30);
        app.is_searching = false;
        app.set_status("hello status");
        let out = render(app, 100, 30);
        assert!(out.contains("hello status"));
        // Cursor deep past the list exercises scroll clamping.
        app.cursor_index = 10_000;
        let _ = render(app, 100, 30);
        app.cursor_index = 0;
        // Stale scroll offset above the cursor scrolls back into view.
        app.scroll_offset.set(10_000);
        let _ = render(app, 100, 30);
        app.scroll_offset.set(0);
        // Near-full disk exercises the warning bar colors.
        if let Some(ref mut fs) = app.fs_info {
            fs.use_percent = 95.0;
        }
        let _ = render(app, 100, 30);
        if let Some(ref mut fs) = app.fs_info {
            fs.use_percent = 80.0;
        }
        let _ = render(app, 100, 30);
    }

    #[test]
    fn ghost_inspector_tabs() {
        let mut fx = fixture();
        let app = &mut fx.app;
        app.active_view = ActiveView::GhostInspector;
        // Unavailable daemon branch (typical in CI).
        let _ = render(app, 100, 30);
        // Unavailable daemon without an error message takes the fallback.
        app.docker_info = DockerDiskInfo {
            is_available: false,
            error_message: None,
            ..Default::default()
        };
        app.ghost_tab_index = 0;
        let _ = render(app, 100, 30);
        // Available daemon with items.
        app.docker_info = available_docker();
        app.ghost_cursor_index = 1;
        let out = render(app, 100, 30);
        assert!(out.contains("abc123"));
        // Deleted-open tab: empty, then fabricated rows.
        app.ghost_tab_index = 1;
        app.ghost_cursor_index = 0;
        app.deleted_open_files = Vec::new();
        let _ = render(app, 100, 30);
        app.deleted_open_files = vec![DeletedOpenFile {
            pid: 1234,
            process_name: "testproc".to_string(),
            original_path: "/tmp/gone".to_string(),
            size: 4096,
            fd: "3".to_string(),
            start_time: Some(999),
        }];
        let out = render(app, 100, 30);
        assert!(out.contains("testproc"));
        let _ = render(app, 40, 20);
    }

    #[test]
    fn top_files_states() {
        let mut fx = fixture();
        let app = &mut fx.app;
        app.open_top_files();
        let out = render(app, 100, 30);
        assert!(out.contains("Top 50"));
        app.apparent_size = true;
        let _ = render(app, 100, 30);
        app.apparent_size = false;
        app.top_cursor = 1;
        let _ = render(app, 40, 20);
    }

    #[test]
    fn janitor_states() {
        let mut fx = fixture();
        let app = &mut fx.app;
        app.open_janitor();
        let _ = render(app, 100, 30);
        // Collapse a non-empty category and move the cursor.
        let target = app
            .janitor_cats
            .iter()
            .position(|cat| !cat.items.is_empty())
            .unwrap_or(0);
        app.janitor_cats[target].expanded = false;
        app.janitor_cursor = 2;
        let _ = render(app, 100, 30);
        let _ = render(app, 40, 20);
        // Current-folder scope exercises the scope label.
        app.janitor_scope = crate::ui::app::JanitorScope::Current;
        let _ = render(app, 100, 30);
    }

    #[test]
    fn confirm_modals() {
        let mut fx = fixture();
        let app = &mut fx.app;
        // Trash confirm, safe and recheck variants.
        app.prompt_move_to_trash();
        let _ = render(app, 100, 30);
        app.action_has_recheck = true;
        let _ = render(app, 100, 30);
        app.action_has_recheck = false;
        app.cancel_modal();
        // Permanent delete confirm.
        app.prompt_permanent_delete();
        let _ = render(app, 100, 30);
        app.cancel_modal();
        // Multi-target list with scroll offset in both directions, from the
        // Janitor view so the scope label renders too.
        app.action_targets = vec![
            std::path::PathBuf::from("/a/0"),
            std::path::PathBuf::from("/a/1"),
            std::path::PathBuf::from("/a/2"),
            std::path::PathBuf::from("/a/3"),
            std::path::PathBuf::from("/a/4"),
            std::path::PathBuf::from("/a/5"),
            std::path::PathBuf::from("/a/6"),
            std::path::PathBuf::from("/a/7"),
        ];
        app.action_total_size = 8;
        app.pending_action = Some(ConfirmAction::MoveToTrash);
        app.previous_view = ActiveView::Janitor;
        app.active_view = ActiveView::ConfirmModal;
        let _ = render(app, 100, 30);
        app.janitor_scope = crate::ui::app::JanitorScope::Current;
        let _ = render(app, 100, 30);
        app.janitor_scope = crate::ui::app::JanitorScope::Global;
        app.confirm_list_offset = 2;
        let out = render(app, 100, 30);
        assert!(out.contains("more"));
        app.cancel_modal();
        // Blocked-system variant.
        app.action_safety_blocked = true;
        app.pending_action = Some(ConfirmAction::MoveToTrash);
        app.active_view = ActiveView::ConfirmModal;
        let _ = render(app, 100, 30);
        app.action_targets = vec![std::path::PathBuf::from("/etc/ssh")];
        let _ = render(app, 100, 30);
        app.cancel_modal();
        app.action_safety_blocked = false;
        // Confirm modal over every backdrop view.
        for previous in [
            ActiveView::GhostInspector,
            ActiveView::TopFiles,
            ActiveView::Janitor,
        ] {
            app.prompt_move_to_trash();
            app.previous_view = previous;
            let _ = render(app, 100, 30);
            app.cancel_modal();
        }
        // Missing fs info takes the fallback header branch.
        app.fs_info = None;
        let _ = render(app, 100, 30);
        // Docker prune + kill confirms, both footer widths.
        app.pending_action = Some(ConfirmAction::DockerPrune);
        app.action_total_size = 42;
        app.active_view = ActiveView::ConfirmModal;
        let _ = render(app, 100, 30);
        app.pending_action = Some(ConfirmAction::KillProcess {
            pid: 99,
            name: "sleep".to_string(),
            start_time: 1,
            pidfd: None,
        });
        app.active_view = ActiveView::ConfirmModal;
        let _ = render(app, 100, 30);
        let _ = render(app, 40, 20);
        app.cancel_modal();
    }

    #[test]
    fn help_and_item_info() {
        let mut fx = fixture();
        let app = &mut fx.app;
        app.previous_view = ActiveView::Filesystem;
        app.active_view = ActiveView::HelpModal;
        let _ = render(app, 100, 30);
        let _ = render(app, 40, 20);
        app.active_view = ActiveView::Filesystem;
        app.open_item_info();
        assert_eq!(app.active_view, ActiveView::ItemInfoModal);
        let _ = render(app, 100, 30);
        let _ = render(app, 40, 20);
        // Compact modal layout on short terminals.
        let _ = render(app, 60, 12);
        app.cancel_modal();
        // Every safety tier and entry shape gets an info modal.
        for name in ["sub", "link.txt", "hosts"] {
            if let Some(index) = app
                .visible_children()
                .iter()
                .position(|e| e.name == name)
            {
                app.cursor_index = index;
                app.open_item_info();
                assert_eq!(app.active_view, ActiveView::ItemInfoModal);
                let _ = render(app, 100, 30);
                app.cancel_modal();
            }
        }
    }
}

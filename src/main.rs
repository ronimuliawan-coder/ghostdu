use ghostdu::{fs, ui};

use clap::Parser;
use crossbeam_channel::unbounded;
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use fs::{
    format_count, format_size, scan_directory_with_options, truncate_end_by_width,
    truncate_start_by_width, ScanProgress, ScannerOptions,
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph},
    Terminal,
};
use std::{
    io::{self, stdout},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};
use ui::{handle_key_event, render_ui, App, EventResult, GhostFilterMode, SortMode};
use unicode_width::UnicodeWidthStr;

use std::io::IsTerminal;

/// ghostdu: Modern, ultra-fast native Linux disk usage & ghost file analyzer
#[derive(Parser, Debug)]
#[command(name = "ghostdu", author = "Ron", version)]
#[command(
    about = "Modern, ultra-fast native Linux disk usage & ghost file analyzer with wastebin support"
)]
struct Cli {
    /// Directory to scan (defaults to current directory)
    #[arg(default_value = ".")]
    path: PathBuf,

    /// Non-interactive summary report (auto-enabled if not running in an interactive terminal)
    #[arg(short, long)]
    summary: bool,

    /// Scan across mount boundaries instead of stopping at them
    #[arg(long, visible_alias = "cm")]
    cross_mounts: bool,

    /// Skip any path containing this substring (repeatable)
    #[arg(long, value_name = "PATTERN")]
    exclude: Vec<String>,

    /// Limit scan descent to N levels (1 = top level only)
    #[arg(long, value_name = "N")]
    depth: Option<usize>,

    /// Start with only safe-to-clean items shown
    #[arg(long)]
    safe_only: bool,

    /// Start showing ghost files only
    #[arg(long, conflicts_with = "hide_ghost")]
    ghost_only: bool,

    /// Start with ghost files hidden
    #[arg(long, conflicts_with = "ghost_only")]
    hide_ghost: bool,

    /// Show apparent file sizes instead of disk usage
    #[arg(long)]
    apparent_size: bool,

    /// Initial sort order
    #[arg(long, value_enum, value_name = "MODE")]
    sort: Option<SortArg>,

    /// Write the scan tree as JSON to FILE and exit
    #[arg(long, value_name = "FILE")]
    export: Option<PathBuf>,
}

#[derive(Copy, Clone, Debug, clap::ValueEnum)]
enum SortArg {
    /// Largest first
    Size,
    /// Smallest first
    #[value(name = "size-asc")]
    SizeAsc,
    /// Alphabetical
    Name,
    /// Most items first
    Items,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let target_path = cli.path.clone();

    if !target_path.exists() {
        eprintln!("Error: Path {:?} does not exist", target_path);
        std::process::exit(1);
    }

    let scan_options = ScannerOptions {
        cross_mounts: cli.cross_mounts,
        excludes: cli.exclude.clone(),
        max_depth: cli.depth,
    };

    if let Some(ref export_file) = cli.export {
        return export_scan(target_path, &scan_options, export_file);
    }

    // Auto-detect non-interactive terminal (e.g. piped or redirected)
    let is_interactive = io::stdout().is_terminal() && io::stdin().is_terminal() && !cli.summary;

    if !is_interactive {
        run_headless_summary(target_path, &scan_options)?;
        return Ok(());
    }

    // Set panic hook to cleanly restore terminal if something crashes
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), LeaveAlternateScreen);
        original_hook(panic_info);
    }));

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let app_result = run_app(&mut terminal, target_path, &cli, &scan_options);

    // Cleanly restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    if let Err(err) = app_result {
        eprintln!("ghostdu error: {}", err);
        std::process::exit(1);
    }

    Ok(())
}

fn run_app<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    mut target_path: PathBuf,
    cli: &Cli,
    scan_options: &ScannerOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut saved_current_path: Option<PathBuf> = None;

    loop {
        // Step 1: Progressive Scanning with Live Progress UI
        let (progress_tx, progress_rx) = unbounded::<ScanProgress>();
        let stop_signal = Arc::new(AtomicBool::new(false));
        let stop_clone = stop_signal.clone();

        let scan_path = target_path.clone();
        let thread_options = scan_options.clone();
        let scan_handle = thread::spawn(move || {
            scan_directory_with_options(&scan_path, Some(progress_tx), stop_clone, thread_options)
        });

        let mut last_progress = ScanProgress {
            files_scanned: 0,
            bytes_scanned: 0,
            current_path: target_path.clone(),
            is_finished: false,
        };

        let scan_start = Instant::now();
        let mut spinner_idx = 0;
        let spinners = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

        let root_entry = loop {
            while let Ok(prog) = progress_rx.try_recv() {
                last_progress = prog;
            }

            if scan_handle.is_finished() {
                break match scan_handle.join() {
                    Ok(res) => res?,
                    Err(_) => return Err("Scan thread panicked".into()),
                };
            }

            // Draw scanning progress screen
            let spinner = spinners[spinner_idx % spinners.len()];
            spinner_idx += 1;
            let elapsed = scan_start.elapsed().as_secs_f32();

            terminal.draw(|f| {
                let size = f.area();
                let area = centered_rect(60, 10, size);

                let files_str = format_count(last_progress.files_scanned as usize);
                let bytes_str = format_size(last_progress.bytes_scanned);
                let path_str = last_progress.current_path.to_string_lossy();

                let lines = vec![
                    Line::from(vec![
                        Span::styled(
                            format!("{} ", spinner),
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            "Analyzing disk usage & ghost files...",
                            Style::default()
                                .fg(Color::White)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled("Files Scanned: ", Style::default().fg(Color::DarkGray)),
                        Span::styled(
                            files_str,
                            Style::default()
                                .fg(Color::LightGreen)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::raw("   "),
                        Span::styled("Total Size: ", Style::default().fg(Color::DarkGray)),
                        Span::styled(
                            bytes_str,
                            Style::default()
                                .fg(Color::LightCyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::raw("   "),
                        Span::styled(
                            format!("({:.1}s)", elapsed),
                            Style::default().fg(Color::DarkGray),
                        ),
                    ]),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled("Scanning: ", Style::default().fg(Color::DarkGray)),
                        Span::styled(
                            truncate_start_by_width(&path_str, 44),
                            Style::default().fg(Color::Yellow),
                        ),
                    ]),
                    Line::from(""),
                    Line::from(Span::styled(
                        "Press 'q' or Ctrl+C to cancel",
                        Style::default().fg(Color::DarkGray),
                    )),
                ];

                let block = Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(Color::LightCyan))
                    .title(" 👻 ghostdu Scanner ");

                f.render_widget(
                    Paragraph::new(lines)
                        .block(block)
                        .alignment(Alignment::Left),
                    area,
                );
            })?;

            if event::poll(Duration::from_millis(60))? {
                if let Event::Key(key) = event::read()? {
                    if key.code == KeyCode::Char('q')
                        || (key.modifiers.contains(KeyModifiers::CONTROL)
                            && key.code == KeyCode::Char('c'))
                    {
                        stop_signal.store(true, Ordering::Relaxed);
                        return Ok(());
                    }
                }
            }
        };

        // Step 2: Main interactive loop
        let mut app = App::new(root_entry);
        app.scan_options = scan_options.clone();
        if cli.safe_only {
            app.safe_only_filter = true;
        }
        if cli.ghost_only {
            app.ghost_filter = GhostFilterMode::GhostOnly;
        } else if cli.hide_ghost {
            app.ghost_filter = GhostFilterMode::HideGhost;
        }
        if cli.apparent_size {
            app.apparent_size = true;
        }
        if let Some(sort) = cli.sort {
            app.sort_mode = match sort {
                SortArg::Size => SortMode::BySizeDesc,
                SortArg::SizeAsc => SortMode::BySizeAsc,
                SortArg::Name => SortMode::ByName,
                SortArg::Items => SortMode::ByItems,
            };
        }
        if let Some(ref saved) = saved_current_path.take() {
            app.navigate_to_path(saved);
            app.set_status("⚡ Rescanned entire tree from root");
        }

        let rescan_needed = loop {
            terminal.draw(|f| render_ui(f, &app))?;

            if event::poll(Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    match handle_key_event(&mut app, key) {
                        EventResult::Exit => return Ok(()),
                        EventResult::Continue => {}
                        EventResult::RescanRequested => {
                            saved_current_path = Some(app.current_dir_entry().path.clone());
                            target_path = app.root_entry.path.clone();
                            break true;
                        }
                        EventResult::RescanPath(new_path) => {
                            target_path = new_path;
                            break true;
                        }
                    }
                }
            }
        };

        if !rescan_needed {
            break;
        }
    }

    Ok(())
}

fn centered_rect(width: u16, height: u16, r: Rect) -> Rect {
    let popup_width = width.min(r.width.saturating_sub(2));
    let popup_height = height.min(r.height.saturating_sub(2));

    Rect {
        x: (r.width.saturating_sub(popup_width)) / 2,
        y: (r.height.saturating_sub(popup_height)) / 2,
        width: popup_width,
        height: popup_height,
    }
}

fn export_scan(
    target_path: PathBuf,
    scan_options: &ScannerOptions,
    export_file: &PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    let stop_signal = Arc::new(AtomicBool::new(false));
    let root_entry =
        scan_directory_with_options(&target_path, None, stop_signal, scan_options.clone())?;
    let json = serde_json::to_string_pretty(&root_entry)?;
    std::fs::write(export_file, json)?;
    println!(
        "Exported {} ({} apparent) to {}",
        format_count(root_entry.items_count),
        format_size(root_entry.size),
        export_file.display()
    );
    Ok(())
}

fn run_headless_summary(
    target_path: PathBuf,
    scan_options: &ScannerOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    println!(
        "👻 ghostdu: Analyzing disk usage & ghost files for {:?}...",
        target_path
    );

    let stop_signal = Arc::new(AtomicBool::new(false));
    let root_entry =
        scan_directory_with_options(&target_path, None, stop_signal, scan_options.clone())?;

    let docker_info = ghostdu::ghost::fetch_docker_disk_info();
    let deleted_open = ghostdu::ghost::scan_deleted_open_files();
    let fs_info = ghostdu::fs::query_fs_info(&root_entry.path);

    println!();
    println!("════════════════════════════════════════════════════════════════════════════════");
    println!("  📂 PATH: {}", root_entry.path.to_string_lossy());
    if let Some(ref fs) = fs_info {
        let percent = fs.use_percent;
        let bar_len = 10;
        let filled_len = ((percent / 100.0) * bar_len as f64).round() as usize;
        let bar_filled = "█".repeat(filled_len.min(bar_len));
        let bar_empty = "░".repeat(bar_len.saturating_sub(filled_len));
        println!(
            "  💾 FILESYSTEM: {} ({} on {})",
            fs.device,
            fs.fs_type,
            fs.mount_point.display()
        );
        println!(
            "     Capacity: {} | Used: {} [{}{}] {:.1}% | Free Space: {}",
            format_size(fs.total_bytes),
            format_size(fs.used_bytes),
            bar_filled,
            bar_empty,
            percent,
            format_size(fs.avail_bytes),
        );
    }
    println!(
        "  📊 TOTAL DISK USAGE: {} (Apparent: {})",
        format_size(root_entry.disk_usage),
        format_size(root_entry.size)
    );
    println!("  📦 TOTAL ITEMS: {}", format_count(root_entry.items_count));
    println!("════════════════════════════════════════════════════════════════════════════════");
    println!(
        "{:<4} {:<40} {:<12} {:<18} {:<12}",
        "SEL", "NAME", "SIZE", "USAGE BAR", "CATEGORY"
    );
    println!("────────────────────────────────────────────────────────────────────────────────");

    let parent_size = root_entry.disk_usage.max(1);
    for entry in root_entry.children.iter().take(20) {
        let percent = ((entry.disk_usage as f64 / parent_size as f64) * 100.0).clamp(0.0, 100.0);
        let bar_len = 10;
        let filled_len = ((percent / 100.0) * bar_len as f64).round() as usize;
        let bar_filled = "█".repeat(filled_len.min(bar_len));
        let bar_empty = "░".repeat(bar_len.saturating_sub(filled_len));
        let bar_text = format!("[{}{}] {:>5.1}%", bar_filled, bar_empty, percent);

        let icon = if entry.is_dir { "📁 " } else { "📄 " };
        let display_name = format!("{}{}", icon, entry.name);
        let name = truncate_end_by_width(&display_name, 40);
        let padding = " ".repeat(40usize.saturating_sub(name.width()));
        let badge = entry.ghost_kind.badge();

        println!(
            "     {}{} {:<12} {:<18} {:<12}",
            name,
            padding,
            format_size(entry.disk_usage),
            bar_text,
            badge
        );
    }

    if root_entry.children.len() > 20 {
        println!("     ... and {} more items", root_entry.children.len() - 20);
    }

    // Ghost & Docker Summary
    println!();
    println!("════════════════════════════════════════════════════════════════════════════════");
    println!("  🐳 DOCKER RECLAIMABLE STORAGE");
    println!("════════════════════════════════════════════════════════════════════════════════");
    if docker_info.is_available {
        let total_docker = docker_info.images_total_size
            + docker_info.containers_total_size
            + docker_info.volumes_total_size
            + docker_info.build_cache_total_size;

        let total_reclaimable = docker_info.images_reclaimable_size
            + docker_info.containers_reclaimable_size
            + docker_info.volumes_reclaimable_size
            + docker_info.build_cache_reclaimable_size;

        println!("  Total Docker Space:       {}", format_size(total_docker));
        println!("  Reclaimable Ghost Space:  {} (Images: {}, Containers: {}, Volumes: {}, BuildCache: {})",
            format_size(total_reclaimable),
            format_size(docker_info.images_reclaimable_size),
            format_size(docker_info.containers_reclaimable_size),
            format_size(docker_info.volumes_reclaimable_size),
            format_size(docker_info.build_cache_reclaimable_size),
        );
        println!(
            "  Images: {} | Containers: {} | Local Volumes: {}",
            docker_info.images_count, docker_info.containers_count, docker_info.volumes_count
        );
    } else {
        println!(
            "  Docker daemon: {}",
            docker_info
                .error_message
                .as_deref()
                .unwrap_or("Not running")
        );
    }

    println!();
    println!("════════════════════════════════════════════════════════════════════════════════");
    println!("  👻 OPEN UNLINKED GHOST FILES (/proc/*/fd)");
    println!("════════════════════════════════════════════════════════════════════════════════");
    if deleted_open.is_empty() {
        println!("  No open unlinked files currently holding significant disk space.");
    } else {
        let total_held: u64 = deleted_open.iter().map(|f| f.size).sum();
        println!(
            "  Total Ghost Space Held: {} ({} files)",
            format_size(total_held),
            deleted_open.len()
        );
        for item in deleted_open.iter().take(5) {
            println!(
                "  PID {:<7} | {:<16} | {:<10} | {}",
                item.pid,
                item.process_name,
                format_size(item.size),
                item.original_path
            );
        }
    }

    println!("════════════════════════════════════════════════════════════════════════════════");
    Ok(())
}

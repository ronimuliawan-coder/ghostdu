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

    /// Limit scan descent to N levels (1 = top level only, 0 = root only)
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

    /// Print shell completions for SHELL and exit
    #[arg(long, value_name = "SHELL")]
    print_completions: Option<clap_complete::Shell>,

    /// Print a man page to stdout and exit
    #[arg(long)]
    print_manpage: bool,
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

fn main() {
    let cli = Cli::parse();
    if let Err(error) = run(cli) {
        eprintln!("ghostdu error: {error}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    use clap::CommandFactory;

    if let Some(shell) = cli.print_completions {
        let mut command = Cli::command();
        clap_complete::generate(shell, &mut command, "ghostdu", &mut io::stdout());
        return Ok(());
    }
    if cli.print_manpage {
        let command = Cli::command();
        let man = clap_mangen::Man::new(command);
        man.render(&mut io::stdout())?;
        return Ok(());
    }

    let target_path = cli.path.clone();

    if !target_path.exists() {
        return Err(format!("Path {target_path:?} does not exist").into());
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

    app_result
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
        apply_cli_display(&mut app, cli);
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
                        EventResult::Subshell(dir) => {
                            if let Err(error) = run_subshell(terminal, &dir) {
                                app.set_status(error.to_string());
                            } else if app.refresh_path(&dir) {
                                app.set_status("Subshell exited; directory refreshed");
                            } else {
                                app.set_status("Subshell exited; refresh failed");
                            }
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

/// Suspend the TUI, run an interactive shell in `dir`, then restore the TUI.
/// Restoration runs even when the shell cannot start.
fn run_subshell<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    dir: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    use crossterm::terminal::{disable_raw_mode, enable_raw_mode};

    disable_raw_mode()?;
    execute!(stdout(), LeaveAlternateScreen)?;
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    let result = std::process::Command::new(shell).current_dir(dir).status();
    execute!(stdout(), EnterAlternateScreen)?;
    enable_raw_mode()?;
    terminal.clear()?;
    if let Err(error) = result {
        return Err(format!("Cannot start shell: {error}").into());
    }
    Ok(())
}

/// Apply CLI display presets to a fresh App. Split out for unit testing.
fn apply_cli_display(app: &mut App, cli: &Cli) {
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
    use std::io::{BufWriter, Write};
    use std::os::unix::fs::OpenOptionsExt;

    let stop_signal = Arc::new(AtomicBool::new(false));
    let root_entry =
        scan_directory_with_options(&target_path, None, stop_signal, scan_options.clone())?;
    let items = root_entry.items_count;
    let apparent = root_entry.size;
    let envelope = fs::ExportEnvelope::wrap(root_entry);
    // Stage through a private temp file and rename: a failed export never leaves
    // a truncated destination, and the listing is never world-readable mid-write.
    // Exclusive creation fails closed if the staging name already exists (even as
    // a planted symlink) instead of following it and truncating its target.
    let mut temp = export_file.clone().into_os_string();
    temp.push(format!(".tmp-{}", std::process::id()));
    let temp_path = PathBuf::from(temp);
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temp_path)?;
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let mut writer = BufWriter::new(file);
        // Stream serialization instead of buffering the whole JSON string.
        serde_json::to_writer_pretty(&mut writer, &envelope)?;
        writer.flush()?;
        drop(writer);
        std::fs::rename(&temp_path, export_file)?;
        Ok(())
    })();
    // Remove only the staging file this invocation created; the final
    // destination is untouched unless the rename succeeded.
    if let Err(err) = result {
        let _ = std::fs::remove_file(&temp_path);
        return Err(err);
    }
    println!(
        "Exported {} ({} apparent) to {}",
        format_count(items),
        format_size(apparent),
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

#[cfg(test)]
mod cli_tests {
    use super::*;
    use clap::Parser;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).expect("valid CLI args")
    }

    #[test]
    fn cli_defaults_and_full_matrix() {
        let cli = parse(&["ghostdu"]);
        assert_eq!(cli.path, PathBuf::from("."));
        assert!(!cli.summary && !cli.cross_mounts && !cli.safe_only);
        assert!(!cli.ghost_only && !cli.hide_ghost && !cli.apparent_size);
        assert!(cli.sort.is_none() && cli.export.is_none());
        assert!(cli.exclude.is_empty() && cli.depth.is_none());

        let cli = parse(&["ghostdu", "/tmp", "-s", "--cm", "--safe-only"]);
        assert_eq!(cli.path, PathBuf::from("/tmp"));
        assert!(cli.summary && cli.cross_mounts && cli.safe_only);

        let cli = parse(&[
            "ghostdu",
            "--exclude",
            "a",
            "--exclude",
            "b",
            "--depth",
            "3",
        ]);
        assert_eq!(cli.exclude, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(cli.depth, Some(3));

        let cli = parse(&[
            "ghostdu",
            "--ghost-only",
            "--apparent-size",
            "--sort",
            "name",
        ]);
        assert!(cli.ghost_only && cli.apparent_size);
        assert!(matches!(cli.sort, Some(SortArg::Name)));

        let cli = parse(&["ghostdu", "--hide-ghost", "--sort", "size-asc"]);
        assert!(cli.hide_ghost);
        assert!(matches!(cli.sort, Some(SortArg::SizeAsc)));

        let cli = parse(&["ghostdu", "--sort", "size"]);
        assert!(matches!(cli.sort, Some(SortArg::Size)));
        let cli = parse(&["ghostdu", "--sort", "items"]);
        assert!(matches!(cli.sort, Some(SortArg::Items)));
        let cli = parse(&["ghostdu", "--export", "/tmp/x.json"]);
        assert_eq!(cli.export, Some(PathBuf::from("/tmp/x.json")));
        let cli = parse(&["ghostdu", "--print-completions", "bash"]);
        assert!(matches!(
            cli.print_completions,
            Some(clap_complete::Shell::Bash)
        ));
        let cli = parse(&["ghostdu", "--print-manpage"]);
        assert!(cli.print_manpage);

        assert!(Cli::try_parse_from(["ghostdu", "--ghost-only", "--hide-ghost"]).is_err());
        assert!(Cli::try_parse_from(["ghostdu", "--sort", "bogus"]).is_err());
        assert!(Cli::try_parse_from(["ghostdu", "--print-completions", "bogus"]).is_err());
    }

    #[test]
    fn run_rejects_missing_path() {
        let cli = parse(&["ghostdu", "/definitely/not/here-12345"]);
        assert!(run(cli).is_err());
    }

    #[test]
    fn run_headless_summary_and_export() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "x").unwrap();
        let path = dir.path().to_string_lossy().into_owned();

        let cli = parse(&["ghostdu", "--summary", &path]);
        run(cli).expect("headless summary");

        let out = dir.path().join("tree.json");
        let out_str = out.to_string_lossy().into_owned();
        let cli = parse(&["ghostdu", "--export", &out_str, &path]);
        run(cli).expect("export");
        assert!(out.exists());

        let cli = parse(&[
            "ghostdu",
            "--export",
            "/definitely/not/here-12345/tree.json",
            &path,
        ]);
        assert!(run(cli).is_err());
    }

    #[test]
    fn run_generators_exit_cleanly() {
        assert!(run(parse(&["ghostdu", "--print-completions", "bash"])).is_ok());
        assert!(run(parse(&["ghostdu", "--print-completions", "zsh"])).is_ok());
        assert!(run(parse(&["ghostdu", "--print-completions", "fish"])).is_ok());
        assert!(run(parse(&["ghostdu", "--print-manpage"])).is_ok());
    }

    #[test]
    fn apply_cli_display_covers_every_branch() {
        let dir = tempfile::tempdir().unwrap();
        let root = ghostdu::fs::scan_directory(dir.path(), None, stop_signal()).unwrap();
        let mut app = App::new(root);
        apply_cli_display(&mut app, &parse(&["ghostdu"]));
        assert!(!app.safe_only_filter && !app.apparent_size);

        let cli = parse(&[
            "ghostdu",
            "--safe-only",
            "--ghost-only",
            "--apparent-size",
            "--sort",
            "items",
        ]);
        apply_cli_display(&mut app, &cli);
        assert!(app.safe_only_filter);
        assert_eq!(app.ghost_filter, GhostFilterMode::GhostOnly);
        assert!(app.apparent_size);
        assert_eq!(app.sort_mode, SortMode::ByItems);

        for (flag, sort) in [
            ("--hide-ghost", SortArg::Size),
            ("--hide-ghost", SortArg::SizeAsc),
            ("--hide-ghost", SortArg::Name),
        ] {
            let flag = flag.to_string();
            let cli = parse(&["ghostdu", &flag, "--sort", sort_name(sort)]);
            apply_cli_display(&mut app, &cli);
            assert_eq!(app.ghost_filter, GhostFilterMode::HideGhost);
        }
        assert_eq!(app.sort_mode, SortMode::ByName);
    }

    fn sort_name(sort: SortArg) -> &'static str {
        match sort {
            SortArg::Size => "size",
            SortArg::SizeAsc => "size-asc",
            SortArg::Name => "name",
            SortArg::Items => "items",
        }
    }

    fn stop_signal() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(false))
    }

    #[test]
    fn centered_rect_clamps_to_small_terminals() {
        let full = centered_rect(60, 10, Rect::new(0, 0, 100, 40));
        assert_eq!((full.width, full.height), (60, 10));
        let tiny = centered_rect(60, 10, Rect::new(0, 0, 20, 5));
        assert!(tiny.width <= 20 && tiny.height <= 5);
    }

    #[test]
    fn run_subshell_reports_without_tty() {
        use ratatui::backend::TestBackend;
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        // No TTY in tests: exercises the early restore-and-report path.
        let _ = run_subshell(&mut terminal, std::path::Path::new("/tmp"));
    }
}



#[cfg(test)]
mod pty_session_tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::fs::OpenOptionsExt;
    use std::sync::{Arc, Mutex};

    /// One pty-backed session shared by every phase of the test. crossterm
    /// keeps a process-global event reader bound to the first pty it sees, so
    /// a second session in the same test binary would hang: everything runs
    /// through this single live pty instead. The master stays open until
    /// teardown (dropped there), so the session never observes a premature
    /// hangup; each send is a detached short write that cannot block.
    struct PtySession {
        master: Option<Arc<Mutex<std::fs::File>>>,
        slave: std::fs::File,
        saved_stdin: std::fs::File,
    }

    impl PtySession {
        fn open() -> Self {
            // Save real stdin; restored in teardown.
            let saved = unsafe { libc::dup(0) };
            assert!(saved >= 0);
            // SAFETY: owned fd, closed in teardown.
            let saved_stdin = unsafe { std::fs::File::from_raw_fd(saved) };

            let master = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY) };
            assert!(master >= 0);
            assert_eq!(unsafe { libc::grantpt(master) }, 0);
            assert_eq!(unsafe { libc::unlockpt(master) }, 0);
            let slave_name = unsafe {
                let ptr = libc::ptsname(master);
                assert!(!ptr.is_null());
                std::ffi::CStr::from_ptr(ptr).to_string_lossy().into_owned()
            };
            let slave = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_NOCTTY)
                .open(&slave_name)
                .unwrap();
            // SAFETY: owned by this struct until teardown.
            let master = unsafe { std::fs::File::from_raw_fd(master) };

            // Route stdin through the pty slave for the session.
            assert_eq!(unsafe { libc::dup2(slave.as_raw_fd(), 0) }, 0, "stdin reroute");
            enable_raw_mode().expect("raw mode on pty slave");
            Self {
                master: Some(Arc::new(Mutex::new(master))),
                slave,
                saved_stdin,
            }
        }

        fn send_later(&self, bytes: &'static [u8], delay: std::time::Duration) {
            let master = self.master.clone().expect("session torn down");
            std::thread::spawn(move || {
                std::thread::sleep(delay);
                use std::io::Write;
                if let Ok(mut master) = master.lock() {
                    let _ = master.write_all(bytes);
                    let _ = master.flush();
                }
            });
        }

        fn teardown(self) {
            drop(self.master);
            let _ = disable_raw_mode();
            use std::os::unix::io::AsFd;
            assert_eq!(
                unsafe { libc::dup2(self.saved_stdin.as_fd().as_raw_fd(), 0) },
                0,
                "stdin restore"
            );
        }
    }

    fn test_cli(path: PathBuf) -> (Cli, ScannerOptions) {
        (
            Cli {
                path,
                summary: false,
                cross_mounts: false,
                exclude: Vec::new(),
                depth: None,
                safe_only: false,
                ghost_only: false,
                hide_ghost: false,
                apparent_size: false,
                sort: None,
                export: None,
                print_completions: None,
                print_manpage: false,
            },
            ScannerOptions {
                cross_mounts: false,
                excludes: Vec::new(),
                max_depth: None,
            },
        )
    }

    #[test]
    fn interactive_session_cancels_rescans_and_quits() {
        use std::time::Duration;
        let session = PtySession::open();
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();

        // Phase 1: `q` lands mid-scan of a slow tree (50k files take seconds
        // even locally, far longer under coverage) and cancels it cleanly.
        let big = tempfile::tempdir().unwrap();
        for d in 0..100 {
            let sub = big.path().join(format!("d{d}"));
            std::fs::create_dir(&sub).unwrap();
            for i in 0..500 {
                std::fs::write(sub.join(format!("f{i}.txt")), "x").unwrap();
            }
        }
        let (cli, options) = test_cli(big.path().to_path_buf());
        session.send_later(b"q", Duration::from_millis(50));
        let result = run_app(&mut terminal, big.path().to_path_buf(), &cli, &options);
        assert!(result.is_ok(), "cancelled scan exits Ok: {result:?}");

        // Phase 2: full rescan, path rescan via Backspace at the root, quit.
        let small = tempfile::tempdir().unwrap();
        std::fs::write(small.path().join("f.txt"), "x").unwrap();
        let (cli, options) = test_cli(small.path().to_path_buf());
        session.send_later(b"R", Duration::from_secs(2));
        session.send_later(b"\x7f", Duration::from_secs(4));
        session.send_later(b"q", Duration::from_secs(6));
        let result = run_app(&mut terminal, small.path().to_path_buf(), &cli, &options);
        assert!(result.is_ok(), "session exits on q: {result:?}");

        session.teardown();
    }
}

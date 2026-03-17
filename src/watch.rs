use std::io;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::execute;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Row, Table, Paragraph};
use ratatui::Terminal;

use crate::config::ClusterConfig;
use crate::status::poll_vm_statuses;

/// Run the TUI dashboard.
pub async fn run<S>(config: &ClusterConfig<S>, interval_secs: u64) -> anyhow::Result<()> {
    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_loop(&mut terminal, config, interval_secs).await;

    terminal::disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

async fn run_loop<S>(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    config: &ClusterConfig<S>,
    interval_secs: u64,
) -> anyhow::Result<()> {
    let ssh = config.ssh_client();

    loop {
        let statuses = poll_vm_statuses(&config.vms, &ssh, &config.state_dir, &config.binary_name).await;
        let last_refresh = chrono::Local::now().format("%H:%M:%S").to_string();

        let running = statuses.iter().filter(|s| s.vm_running).count();
        let ssh_ok = statuses.iter().filter(|s| s.ssh_ok).count();
        let steam = statuses.iter().filter(|s| s.steam_running).count();
        let game = statuses.iter().filter(|s| s.game_running).count();
        let total = statuses.len();

        let message = format!(
            "VMs: {running}/{total} up | SSH: {ssh_ok}/{total} | Steam: {steam}/{total} | Game: {game}/{total}"
        );

        terminal.draw(|f| {
            let chunks = Layout::vertical([
                Constraint::Length(3),
                Constraint::Min(5),
                Constraint::Length(3),
            ])
            .split(f.area());

            let header = Paragraph::new(Line::from(vec![
                Span::styled(" cluster-ctl watch ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                Span::raw(" │ "),
                Span::raw(&message),
            ]))
            .block(Block::default().borders(Borders::ALL).title(" Cluster Dashboard "));
            f.render_widget(header, chunks[0]);

            let header_row = Row::new(vec![
                Cell::from("VM").style(Style::default().add_modifier(Modifier::BOLD)),
                Cell::from("IP").style(Style::default().add_modifier(Modifier::BOLD)),
                Cell::from("VM").style(Style::default().add_modifier(Modifier::BOLD)),
                Cell::from("SSH").style(Style::default().add_modifier(Modifier::BOLD)),
                Cell::from("Steam").style(Style::default().add_modifier(Modifier::BOLD)),
                Cell::from("Game").style(Style::default().add_modifier(Modifier::BOLD)),
            ]);

            let rows: Vec<Row> = statuses
                .iter()
                .map(|s| {
                    Row::new(vec![
                        Cell::from(s.name.as_str()),
                        Cell::from(s.ip.as_str()),
                        status_cell(s.vm_running),
                        status_cell(s.ssh_ok),
                        status_cell(s.steam_running),
                        status_cell(s.game_running),
                    ])
                })
                .collect();

            let table = Table::new(
                std::iter::once(header_row).chain(rows),
                [
                    Constraint::Length(8),
                    Constraint::Length(16),
                    Constraint::Length(6),
                    Constraint::Length(6),
                    Constraint::Length(8),
                    Constraint::Length(8),
                ],
            )
            .block(Block::default().borders(Borders::ALL).title(" VM Status "));
            f.render_widget(table, chunks[1]);

            let refresh_info = format!(" │ Refresh: {interval_secs}s │ ");
            let footer = Paragraph::new(Line::from(vec![
                Span::raw(" Last refresh: "),
                Span::styled(&last_refresh, Style::default().fg(Color::Yellow)),
                Span::raw(&refresh_info),
                Span::styled("q", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
                Span::raw(" quit  "),
                Span::styled("r", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
                Span::raw(" refresh"),
            ]))
            .block(Block::default().borders(Borders::ALL));
            f.render_widget(footer, chunks[2]);
        })?;

        let timeout = Duration::from_secs(interval_secs);
        let start = std::time::Instant::now();
        loop {
            let remaining = timeout.saturating_sub(start.elapsed());
            if remaining.is_zero() {
                break;
            }
            if event::poll(remaining.min(Duration::from_millis(250)))? {
                if let Event::Key(key) = event::read()? {
                    match key.code {
                        KeyCode::Char('q') => return Ok(()),
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return Ok(()),
                        KeyCode::Char('r') => break,
                        _ => {}
                    }
                }
            }
        }
    }
}

fn status_cell(ok: bool) -> Cell<'static> {
    if ok {
        Cell::from("OK").style(Style::default().fg(Color::Green))
    } else {
        Cell::from("─").style(Style::default().fg(Color::DarkGray))
    }
}

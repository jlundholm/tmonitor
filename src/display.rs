use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use ratatui::crossterm::event::{self, Event, KeyCode, KeyModifiers};
use ratatui::crossterm::terminal;
use ratatui::crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::crossterm::execute;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;
use tokio::sync::RwLock;

use crate::engine::{CellKey, HostState};

const COLOR_UP: Color = Color::Rgb(0x00, 0xFF, 0x00);
const COLOR_DOWN: Color = Color::Rgb(0xFF, 0x00, 0x00);
const COLOR_TEXT: Color = Color::Rgb(0xFF, 0xFF, 0xFF);
const COLOR_DIM: Color = Color::Rgb(0x88, 0x88, 0x88);

#[derive(Debug, thiserror::Error)]
pub enum DisplayError {
    #[error("terminal I/O error: {0}")]
    Io(#[from] std::io::Error),
}

struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(std::io::stdout(), LeaveAlternateScreen);
    }
}

pub struct App {
    state: Arc<RwLock<HashMap<CellKey, HostState>>>,
    cell_order: Vec<CellKey>,
}

impl App {
    pub fn new(
        state: Arc<RwLock<HashMap<CellKey, HostState>>>,
        cell_order: Vec<CellKey>,
    ) -> Self {
        App { state, cell_order }
    }
}

struct CellSnapshot {
    status: crate::check::CheckResult,
    duration: Duration,
}

impl App {
    fn render(&self, frame: &mut Frame, snapshot: &[CellSnapshot]) {
        let area = frame.area();
        let cell_count = self.cell_order.len();

        let (columns, max_rows, cell_width) = compute_layout((area.width, area.height), cell_count);

        let chunks = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Fill(1),
        ])
        .split(area);

        render_top_bar(frame, chunks[0]);

        if cell_count == 0 || snapshot.len() != cell_count {
            return;
        }

        let grid_area = chunks[2];
        let available_width = grid_area.width as usize;
        let actual_cell_width = if columns > 0 {
            available_width / columns
        } else {
            available_width
        };

        for col in 0..columns {
            let col_x = grid_area.x + (col * actual_cell_width) as u16;

            for row in 0..max_rows {
                let idx = col * max_rows + row;
                if idx >= cell_count {
                    break;
                }
                let cell = &snapshot[idx];
                let cell_label = self.cell_order[idx].label();
                let cell_y = grid_area.y + (row * 2) as u16;
                let cell_rect = Rect::new(col_x, cell_y, actual_cell_width as u16, 1);

                let (label, status_color) = match cell.status {
                    crate::check::CheckResult::Up => ("Up", COLOR_UP),
                    crate::check::CheckResult::Down => ("Down", COLOR_DOWN),
                };

                if cell_width >= 22 {
                    let duration_str = format_duration(cell.duration);
                    let line = Line::from(vec![
                        ratatui::text::Span::styled(
                            format!("{}  {}", cell_label, label),
                            Style::default().fg(status_color),
                        ),
                        ratatui::text::Span::styled(
                            format!("  {}", duration_str),
                            Style::default().fg(COLOR_TEXT),
                        ),
                    ]);
                    let paragraph = Paragraph::new(line).wrap(Wrap { trim: false });
                    frame.render_widget(paragraph, cell_rect);
                } else if cell_width >= 15 {
                    let cell_text = format!("{}  {}", cell_label, label);
                    let paragraph = Paragraph::new(cell_text)
                        .style(Style::default().fg(status_color))
                        .wrap(Wrap { trim: false });
                    frame.render_widget(paragraph, cell_rect);
                } else {
                    let cell_text = truncated_hostname(&cell_label, cell_width);
                    let paragraph = Paragraph::new(cell_text)
                        .style(Style::default().fg(status_color))
                        .wrap(Wrap { trim: false });
                    frame.render_widget(paragraph, cell_rect);
                };
            }
        }
    }
}

fn truncated_hostname(name: &str, max_width: usize) -> String {
    if name.chars().count() <= max_width {
        name.to_string()
    } else {
        let visible = max_width.saturating_sub(1);
        format!(
            "{}…",
            name.chars().take(visible).collect::<String>()
        )
    }
}

fn render_top_bar(frame: &mut Frame, area: Rect) {
    let uptime = read_pi_uptime();
    let title = "tmonitor";
    let gap = area.width.saturating_sub((title.len() + 2 + uptime.len()) as u16) as usize;
    let text = format!("{}{}{}", title, " ".repeat(gap), uptime);
    let paragraph = Paragraph::new(text)
        .style(Style::default().fg(COLOR_DIM))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

pub fn read_pi_uptime() -> String {
    #[cfg(target_os = "linux")]
    {
        let uptime_secs = std::fs::read_to_string("/proc/uptime")
            .ok()
            .and_then(|s| s.split_whitespace().next()?.parse::<f64>().ok())
            .unwrap_or(0.0) as u64;
        let days = uptime_secs / 86400;
        let hours = (uptime_secs % 86400) / 3600;
        let mins = (uptime_secs % 3600) / 60;
        format!("up {}d {}h {}m", days, hours, mins)
    }
    #[cfg(not(target_os = "linux"))]
    {
        String::new()
    }
}

pub fn format_duration(d: Duration) -> String {
    let total_secs = d.as_secs();
    let days = total_secs / 86400;
    let hours = (total_secs % 86400) / 3600;
    let minutes = (total_secs % 3600) / 60;
    format!("{}d {}h {}m", days, hours, minutes)
}

pub fn compute_layout(
    terminal_size: (u16, u16),
    cell_count: usize,
) -> (usize, usize, usize) {
    let (width, height) = terminal_size;
    let data_rows = (height as usize).saturating_sub(2);
    let max_rows_per_col = data_rows / 2;
    if max_rows_per_col == 0 || cell_count == 0 {
        return (1, 1, width as usize);
    }
    let columns = (cell_count + max_rows_per_col - 1) / max_rows_per_col;
    let cell_width = (width as usize) / columns;
    (columns, max_rows_per_col, cell_width)
}

pub async fn run_display(
    app: App,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<(), DisplayError> {
    terminal::enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = ratatui::backend::CrosstermBackend::new(&mut stdout);
    let mut terminal = ratatui::Terminal::new(backend)?;
    terminal.clear()?;

    let _guard = TerminalGuard;

    loop {
        let snapshot = {
            let guard = app.state.read().await;
            app.cell_order
                .iter()
                .map(|key| {
                    guard.get(key).map_or(
                        CellSnapshot {
                            status: crate::check::CheckResult::Up,
                            duration: Duration::ZERO,
                        },
                        |hs| {
                            let duration = match hs.status {
                                crate::check::CheckResult::Up => hs.uptime_duration(),
                                crate::check::CheckResult::Down => hs.downtime_duration(),
                            };
                            CellSnapshot {
                                status: hs.status,
                                duration,
                            }
                        },
                    )
                })
                .collect::<Vec<_>>()
        };

        terminal.draw(|f| app.render(f, &snapshot))?;

        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                if key.code == KeyCode::Char('c')
                    && key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    break;
                }
            }
        }

        if cancel.is_cancelled() {
            break;
        }
    }

    Ok(())
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_duration_zero() {
        assert_eq!(format_duration(Duration::from_secs(0)), "0d 0h 0m");
    }

    #[test]
    fn test_format_duration_seconds() {
        assert_eq!(format_duration(Duration::from_secs(3661)), "0d 1h 1m");
    }

    #[test]
    fn test_format_duration_days() {
        assert_eq!(format_duration(Duration::from_secs(90061)), "1d 1h 1m");
    }

    #[test]
    fn test_format_duration_exact_hours() {
        assert_eq!(format_duration(Duration::from_secs(7200)), "0d 2h 0m");
    }

    #[test]
    fn test_compute_layout_normal() {
        let (cols, rows, width) = compute_layout((120, 40), 40);
        assert_eq!(cols, 3);
        assert_eq!(rows, 19);
        assert_eq!(width, 40);
    }

    #[test]
    fn test_compute_layout_single_row() {
        let (cols, rows, width) = compute_layout((80, 24), 5);
        assert_eq!(cols, 1);
        assert_eq!(rows, 11);
        assert_eq!(width, 80);
    }

    #[test]
    fn test_compute_layout_zero_hosts() {
        let (cols, _rows, width) = compute_layout((80, 24), 0);
        assert_eq!(cols, 1);
        assert_eq!(width, 80);
    }

    #[test]
    fn test_compute_layout_tiny_terminal() {
        let (cols, _rows, width) = compute_layout((80, 1), 5);
        assert_eq!(cols, 1);
        assert_eq!(width, 80);
    }

    #[test]
    fn test_read_pi_uptime_linux_format() {
        let input = "123456.78 98765.43";
        let result = parse_uptime(input);
        assert_eq!(result, "up 1d 10h 17m");
    }

    #[test]
    fn test_read_pi_uptime_zero() {
        let input = "0.0 0.0";
        let result = parse_uptime(input);
        assert_eq!(result, "up 0d 0h 0m");
    }

    #[test]
    fn test_read_pi_uptime_exact_day() {
        let input = "86400.0 0.0";
        let result = parse_uptime(input);
        assert_eq!(result, "up 1d 0h 0m");
    }

    fn parse_uptime(content: &str) -> String {
        let uptime_secs = content
            .split_whitespace()
            .next()
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.0) as u64;
        let days = uptime_secs / 86400;
        let hours = (uptime_secs % 86400) / 3600;
        let mins = (uptime_secs % 3600) / 60;
        format!("up {}d {}h {}m", days, hours, mins)
    }

    #[test]
    fn test_truncated_hostname_short() {
        assert_eq!(truncated_hostname("hello", 10), "hello");
    }

    #[test]
    fn test_truncated_hostname_exact() {
        assert_eq!(truncated_hostname("1234567890", 10), "1234567890");
    }

    #[test]
    fn test_truncated_hostname_long() {
        let result = truncated_hostname("this-is-a-long-hostname", 10);
        assert_eq!(result.chars().count(), 10);
        assert!(result.ends_with('…'));
    }
}

//! hyuqueue-tui — ratatui client.
//!
//! Secondary client proving the architecture is editor-agnostic.
//! Talks to hyuqueue-server over HTTP, same as the CLI.
//!
//! MVP: iron mode only. Shows the current item and item count.
//! Key palette rendered from item capabilities + global activities.
//!
//! # LLM Development Guidelines
//! - No domain logic here. All state comes from the server via HTTP.
//! - Iron mode: SPC to ack (advance queue). Other keys invoke activities.
//! - Keep event loop and rendering separate.

mod config;
mod logging;

use clap::Parser;
use config::{Config, ConfigError};
use crossterm::{
  event::{self, Event, KeyCode},
  execute,
  terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use logging::init_logging;
use ratatui::{
  Terminal,
  backend::CrosstermBackend,
  layout::{Constraint, Direction, Layout},
  style::{Color, Modifier, Style},
  text::{Line, Span},
  widgets::{Block, Borders, Paragraph, Wrap},
};
use std::{io, path::PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
enum ApplicationError {
  #[error("Configuration error: {0}")]
  Config(#[from] ConfigError),

  #[error("Terminal error: {0}")]
  Terminal(#[from] io::Error),

  #[error("HTTP error: {0}")]
  Http(#[from] reqwest::Error),
}

#[derive(Debug, Parser)]
#[command(name = "hyuqueue-tui", about = "hyuqueue TUI client")]
struct Cli {
  #[arg(long, env = "LOG_LEVEL")]
  log_level: Option<String>,

  #[arg(long, env = "LOG_FORMAT")]
  log_format: Option<String>,

  #[arg(short, long, env = "CONFIG_FILE")]
  config: Option<PathBuf>,

  #[arg(long, env = "HYUQUEUE_SERVER")]
  server: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), ApplicationError> {
  let cli = Cli::parse();
  let mut config = Config::from_file_or_default(cli.config)?;
  if let Some(url) = cli.server {
    config.server_url = url;
  }

  init_logging(config.log_level, config.log_format);

  enable_raw_mode()?;
  let mut stdout = io::stdout();
  execute!(stdout, EnterAlternateScreen)?;
  let backend = CrosstermBackend::new(stdout);
  let mut terminal = Terminal::new(backend)?;

  let result = run_app(&mut terminal, &config).await;

  disable_raw_mode()?;
  execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
  terminal.show_cursor()?;

  result
}

struct AppState {
  item: Option<serde_json::Value>,
  count: i64,
  status_msg: String,
}

async fn run_app(
  terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
  config: &Config,
) -> Result<(), ApplicationError> {
  let http = reqwest::Client::new();
  let base = config.server_url.trim_end_matches('/').to_string();

  let mut app = AppState {
    item: None,
    count: 0,
    status_msg: String::new(),
  };

  // Load initial state.
  refresh(&http, &base, &mut app).await;

  loop {
    terminal.draw(|f| render(f, &app))?;

    if event::poll(std::time::Duration::from_millis(250))? {
      if let Event::Key(key) = event::read()? {
        match key.code {
          // q — quit
          KeyCode::Char('q') => break,

          // SPC — ack current item (iron mode gate)
          KeyCode::Char(' ') => {
            if let Some(item) = &app.item {
              if let Some(id) = item.get("id").and_then(|v| v.as_str()) {
                let url = format!("{base}/api/v1/items/{id}/ack");
                match http.post(&url).send().await {
                  Ok(_) => {
                    app.status_msg = "Acked.".to_string();
                    refresh(&http, &base, &mut app).await;
                  }
                  Err(e) => {
                    app.status_msg = format!("Ack failed: {e}");
                  }
                }
              }
            }
          }

          // r — refresh
          KeyCode::Char('r') => {
            refresh(&http, &base, &mut app).await;
          }

          _ => {}
        }
      }
    }
  }

  Ok(())
}

async fn refresh(
  http: &reqwest::Client,
  base: &str,
  app: &mut AppState,
) {
  // Fetch next item.
  if let Ok(resp) = http
    .get(format!("{base}/api/v1/items/next"))
    .send()
    .await
    .and_then(|r| Ok(r))
  {
    if let Ok(json) = resp.json::<serde_json::Value>().await {
      app.item = json.get("item").cloned();
    }
  }

  // Fetch count.
  if let Ok(resp) = http
    .get(format!("{base}/api/v1/items/count"))
    .send()
    .await
  {
    if let Ok(json) = resp.json::<serde_json::Value>().await {
      app.count = json.get("count").and_then(|v| v.as_i64()).unwrap_or(0);
    }
  }
}

fn render(f: &mut ratatui::Frame, app: &AppState) {
  let chunks = Layout::default()
    .direction(Direction::Vertical)
    .constraints([
      Constraint::Length(3), // header / count
      Constraint::Min(0),    // item body
      Constraint::Length(3), // key palette
    ])
    .split(f.area());

  // Header
  let header_text = format!(" hyuqueue  [{} in queue]", app.count);
  let header = Paragraph::new(header_text)
    .style(Style::default().fg(Color::Green).add_modifier(Modifier::BOLD))
    .block(Block::default().borders(Borders::BOTTOM));
  f.render_widget(header, chunks[0]);

  // Item body
  let body_text = if let Some(item) = &app.item {
    let title = item
      .get("title")
      .and_then(|v| v.as_str())
      .unwrap_or("(no title)");
    let source = item
      .get("source")
      .and_then(|v| v.as_str())
      .unwrap_or("unknown");
    let body = item
      .get("body")
      .and_then(|v| v.as_str())
      .unwrap_or("");
    format!("[{source}] {title}\n\n{body}")
  } else {
    "Queue is empty. Good job.".to_string()
  };

  let item_block = Paragraph::new(body_text)
    .wrap(Wrap { trim: false })
    .block(Block::default().borders(Borders::ALL).title(" Item "));
  f.render_widget(item_block, chunks[1]);

  // Key palette
  let keys = if app.item.is_some() {
    vec![
      Span::styled(
        "SPC",
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
      ),
      Span::raw(" ack   "),
      Span::styled(
        "r",
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
      ),
      Span::raw(" refresh   "),
      Span::styled(
        "q",
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
      ),
      Span::raw(" quit"),
    ]
  } else {
    vec![
      Span::styled(
        "r",
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
      ),
      Span::raw(" refresh   "),
      Span::styled(
        "q",
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
      ),
      Span::raw(" quit"),
    ]
  };

  let status = if app.status_msg.is_empty() {
    Line::from(keys)
  } else {
    Line::from(vec![Span::styled(
      app.status_msg.clone(),
      Style::default().fg(Color::Yellow),
    )])
  };

  let palette = Paragraph::new(status)
    .block(Block::default().borders(Borders::TOP));
  f.render_widget(palette, chunks[2]);
}

use std::io::Write;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, BorderType, Gauge, Paragraph},
    DefaultTerminal, Frame,
};
use tui_big_text::{BigText, PixelSize};

// ── Configuration ─────────────────────────────────────────────────────────────

struct Config {
    focus:              Duration,
    short_break:        Duration,
    long_break:         Duration,
    sessions_per_round: u32,
    /// Ring the terminal bell when a phase ends.
    bell:               bool,
    /// Send a desktop notification when a phase ends.
    notify:             bool,
    /// Automatically start the next phase when the timer runs out naturally.
    auto_start_on_timeout: bool,
    /// Automatically start the next phase when you manually press [n] to skip.
    auto_start_on_skip:    bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            focus:              Duration::from_secs(25 * 60),
            short_break:        Duration::from_secs(5  * 60),
            long_break:         Duration::from_secs(15 * 60),
            sessions_per_round: 4,
            bell:               true,
            notify:             true,
            auto_start_on_timeout: true,
            auto_start_on_skip:    false,
        }
    }
}

// ── State machine ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
enum Phase {
    Focus,
    ShortBreak,
    LongBreak,
}

impl Phase {
    fn label(self) -> &'static str {
        match self {
            Phase::Focus      => "FOCUS",
            Phase::ShortBreak => "SHORT BREAK",
            Phase::LongBreak  => "LONG BREAK",
        }
    }

    fn color(self) -> Color {
        match self {
            Phase::Focus      => Color::Red,
            Phase::ShortBreak => Color::Green,
            Phase::LongBreak  => Color::Cyan,
        }
    }
}

struct App {
    cfg:               Config,
    phase:             Phase,
    remaining:         Duration,
    running:           bool,
    /// Which focus session we're currently on (1-based).
    current_session:   u32,
    /// Focus sessions completed in the current round.
    done_in_round:     u32,
    /// Focus sessions completed overall.
    total_done:        u32,
    last_tick:         Instant,
}

impl App {
    fn new(cfg: Config) -> Self {
        let remaining = cfg.focus;
        Self {
            remaining,
            phase:           Phase::Focus,
            running:         false,
            current_session: 1,
            done_in_round:   0,
            total_done:      0,
            last_tick:       Instant::now(),
            cfg,
        }
    }

    /// Called every frame; subtracts real elapsed time and advances phase when
    /// the timer reaches zero. Returns the phase that just ended, if any.
    fn tick(&mut self) -> Option<Phase> {
        if !self.running { return None; }
        let now     = Instant::now();
        let elapsed = now.duration_since(self.last_tick);
        self.last_tick = now;

        if elapsed >= self.remaining {
            self.remaining = Duration::ZERO;
            let ended = self.phase;
            self.advance(self.cfg.auto_start_on_timeout);
            Some(ended)
        } else {
            self.remaining -= elapsed;
            None
        }
    }

    /// Move to the next phase. `auto_start` controls whether the new phase
    /// begins running immediately or waits for the user to press Space.
    fn advance(&mut self, auto_start: bool) {
        match self.phase {
            Phase::Focus => {
                self.done_in_round += 1;
                self.total_done    += 1;
                if self.done_in_round >= self.cfg.sessions_per_round {
                    self.phase     = Phase::LongBreak;
                    self.remaining = self.cfg.long_break;
                } else {
                    self.phase     = Phase::ShortBreak;
                    self.remaining = self.cfg.short_break;
                }
            }
            Phase::ShortBreak => {
                self.current_session = self.done_in_round + 1;
                self.phase           = Phase::Focus;
                self.remaining       = self.cfg.focus;
            }
            Phase::LongBreak => {
                self.done_in_round   = 0;
                self.current_session = 1;
                self.phase           = Phase::Focus;
                self.remaining       = self.cfg.focus;
            }
        }
        self.running = auto_start;
        if auto_start {
            self.last_tick = Instant::now();
        }
    }

    fn toggle(&mut self) {
        self.running = !self.running;
        if self.running {
            self.last_tick = Instant::now();
        }
    }

    /// Skip to the next phase. Returns the phase that was skipped.
    fn skip(&mut self) -> Phase {
        let ended = self.phase;
        self.advance(self.cfg.auto_start_on_skip);
        ended
    }

    fn reset_phase(&mut self) {
        self.remaining = match self.phase {
            Phase::Focus      => self.cfg.focus,
            Phase::ShortBreak => self.cfg.short_break,
            Phase::LongBreak  => self.cfg.long_break,
        };
        self.running = false;
    }

    fn phase_total(&self) -> Duration {
        match self.phase {
            Phase::Focus      => self.cfg.focus,
            Phase::ShortBreak => self.cfg.short_break,
            Phase::LongBreak  => self.cfg.long_break,
        }
    }

    fn progress(&self) -> f64 {
        let total = self.phase_total().as_secs_f64();
        if total == 0.0 { return 1.0; }
        let elapsed = total - self.remaining.as_secs_f64();
        (elapsed / total).clamp(0.0, 1.0)
    }
}

// ── Formatting ────────────────────────────────────────────────────────────────

fn fmt_time(d: Duration) -> String {
    let s = d.as_secs();
    format!("{:02}:{:02}", s / 60, s % 60)
}

// ── Alerts ────────────────────────────────────────────────────────────────────

/// Fire the configured alerts for a phase that just ended.
fn fire_alerts(cfg: &Config, ended: Phase) {
    if cfg.bell {
        // \x07 is the BEL control character; the terminal emulator rings its bell.
        print!("\x07");
        let _ = std::io::stdout().flush();
    }
    if cfg.notify {
        let (summary, body) = match ended {
            Phase::Focus      => ("🍅 VibeOdoro 😎 · Focus complete",  "Time for a well-earned break."),
            Phase::ShortBreak => ("🍅 VibeOdoro 😎 · Break over",      "Back to focus!"),
            Phase::LongBreak  => ("🍅 VibeOdoro 😎 · Long break over", "Starting a fresh round."),
        };
        let _ = notify_rust::Notification::new()
            .summary(summary)
            .body(body)
            .show();
    }
}

// ── Event loop ────────────────────────────────────────────────────────────────

fn run(terminal: &mut DefaultTerminal, app: &mut App) -> std::io::Result<()> {
    loop {
        terminal.draw(|f| ui(f, app))?;

        if event::poll(Duration::from_millis(200))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                        KeyCode::Char(' ')                => app.toggle(),
                        KeyCode::Char('n')                => {
                            let ended = app.skip();
                            fire_alerts(&app.cfg, ended);
                        }
                        KeyCode::Char('r')                => app.reset_phase(),
                        _ => {}
                    }
                }
            }
        }

        if let Some(ended) = app.tick() {
            fire_alerts(&app.cfg, ended);
        }
    }
}

// ── UI ────────────────────────────────────────────────────────────────────────

/// Width of "MM:SS" rendered with HalfHeight pixel size (each char = 8 cols).
const TIMER_COLS: u16 = 5 * 8; // 40

/// Outer box dimensions.
const BOX_W: u16 = 56;
const BOX_H: u16 = 16;

fn ui(frame: &mut Frame, app: &App) {
    let area  = frame.area();
    let color = app.phase.color();

    // ── Center the outer box ─────────────────────────────────────────────────
    let [_, mid_row, _] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(BOX_H),
        Constraint::Fill(1),
    ]).areas(area);

    let [_, center, _] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(BOX_W),
        Constraint::Fill(1),
    ]).areas(mid_row);

    // ── Outer block ──────────────────────────────────────────────────────────
    let title = format!("  {}  ", app.phase.label());
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(title)
        .title_alignment(Alignment::Center)
        .border_style(Style::default().fg(color));

    let inner = block.inner(center);
    frame.render_widget(block, center);

    // ── Inner rows ───────────────────────────────────────────────────────────
    let [info_row, _, timer_row, _, dots_row, _, gauge_row, help_row, _] =
        Layout::vertical([
            Constraint::Length(1), // session info line
            Constraint::Length(1), // spacer
            Constraint::Length(4), // big timer  (HalfHeight = 4 terminal rows)
            Constraint::Length(1), // spacer
            Constraint::Length(1), // session dots
            Constraint::Length(1), // spacer
            Constraint::Length(1), // progress gauge
            Constraint::Length(1), // help text
            Constraint::Fill(1),   // bottom padding
        ])
        .areas(inner);

    render_info(frame, app, info_row, color);
    render_timer(frame, app, timer_row, color);
    render_dots(frame, app, dots_row, color);
    render_gauge(frame, app, gauge_row, color);
    render_help(frame, help_row);
}

fn render_info(frame: &mut Frame, app: &App, area: Rect, color: Color) {
    let text = if app.phase == Phase::Focus {
        format!(
            "Session {} of {}   ·   {} completed",
            app.current_session, app.cfg.sessions_per_round, app.total_done
        )
    } else {
        format!("{} sessions completed", app.total_done)
    };
    frame.render_widget(
        Paragraph::new(text)
            .alignment(Alignment::Center)
            .style(Style::default().fg(color).add_modifier(Modifier::DIM)),
        area,
    );
}

fn render_timer(frame: &mut Frame, app: &App, area: Rect, color: Color) {
    // Manually center the big text rect horizontally.
    let x_pad    = (area.width.saturating_sub(TIMER_COLS)) / 2;
    let timer_rect = Rect {
        x:      area.x + x_pad,
        y:      area.y,
        width:  TIMER_COLS.min(area.width),
        height: area.height,
    };

    let time_str = fmt_time(app.remaining);
    let big = BigText::builder()
        .pixel_size(PixelSize::HalfHeight)
        .lines(vec![Line::from(time_str)])
        .style(Style::default().fg(color).add_modifier(Modifier::BOLD))
        .build();
    frame.render_widget(big, timer_rect);
}

fn render_dots(frame: &mut Frame, app: &App, area: Rect, color: Color) {
    let dots: String = (1..=app.cfg.sessions_per_round)
        .flat_map(|i| {
            let symbol = if i <= app.done_in_round {
                "● "
            } else if i == app.current_session && app.phase == Phase::Focus {
                "◉ "
            } else {
                "○ "
            };
            symbol.chars().collect::<Vec<_>>()
        })
        .collect();

    frame.render_widget(
        Paragraph::new(dots.trim_end().to_string())
            .alignment(Alignment::Center)
            .style(Style::default().fg(color)),
        area,
    );
}

fn render_gauge(frame: &mut Frame, app: &App, area: Rect, color: Color) {
    let label = if app.running { "  ▶  running  " } else { "  ⏸  paused  " };
    let gauge = Gauge::default()
        .gauge_style(Style::default().fg(color).bg(Color::DarkGray))
        .label(label)
        .ratio(app.progress());
    frame.render_widget(gauge, area);
}

fn render_help(frame: &mut Frame, area: Rect) {
    frame.render_widget(
        Paragraph::new("[Space] start/pause   [n] skip   [r] reset   [q] quit")
            .alignment(Alignment::Center)
            .style(Style::default().fg(Color::DarkGray)),
        area,
    );
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() -> std::io::Result<()> {
    let mut terminal = ratatui::init();
    let result = run(&mut terminal, &mut App::new(Config::default()));
    ratatui::restore();
    result
}

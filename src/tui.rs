//! Interactive statistics dashboard (ratatui + crossterm).
//!
//! Rendering is split from terminal I/O: [`draw`] is a pure function of the
//! [`App`] state and a [`Frame`], which lets it be tested with a
//! `TestBackend`.

use std::io::{self, Stdout};
use std::time::Duration;

use anyhow::{Context, Result};
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Bar, BarChart, Block, Borders, Cell, List, ListItem, Paragraph, Row, Sparkline, Table, Tabs,
    Wrap,
};
use ratatui::{Frame, Terminal};

use biblio::stats::Stats;

use crate::{fmt_int, human_bytes};

/// The dashboard tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Overview,
    Timeline,
    People,
    Venues,
    Health,
}

impl Tab {
    const ALL: [Tab; 5] = [
        Tab::Overview,
        Tab::Timeline,
        Tab::People,
        Tab::Venues,
        Tab::Health,
    ];

    fn title(self) -> &'static str {
        match self {
            Tab::Overview => "Overview",
            Tab::Timeline => "Timeline",
            Tab::People => "People",
            Tab::Venues => "Venues",
            Tab::Health => "Health",
        }
    }

    fn index(self) -> usize {
        Tab::ALL.iter().position(|t| *t == self).unwrap_or(0)
    }
}

/// Dashboard state.
pub struct App {
    stats: Stats,
    db_label: String,
    tab: Tab,
    scroll: u16,
    show_help: bool,
    pub should_quit: bool,
}

impl App {
    pub fn new(stats: Stats, db_label: String) -> Self {
        Self {
            stats,
            db_label,
            tab: Tab::Overview,
            scroll: 0,
            show_help: false,
            should_quit: false,
        }
    }

    fn switch(&mut self, tab: Tab) {
        self.tab = tab;
        self.scroll = 0;
    }

    fn next_tab(&mut self) {
        let i = (self.tab.index() + 1) % Tab::ALL.len();
        self.switch(Tab::ALL[i]);
    }

    fn prev_tab(&mut self) {
        let i = (self.tab.index() + Tab::ALL.len() - 1) % Tab::ALL.len();
        self.switch(Tab::ALL[i]);
    }

    /// Number of scrollable rows on the current tab.
    fn row_count(&self) -> usize {
        match self.tab {
            Tab::Timeline => self.stats.by_year.len(),
            Tab::People => self.stats.top_authors.len(),
            Tab::Health => {
                3 + self.stats.anomalies.implausible_years.len()
                    + self.stats.anomalies.duplicate_dois.len()
            }
            _ => 0,
        }
    }

    /// Largest useful scroll offset on the current tab.
    fn max_scroll(&self) -> u16 {
        self.row_count().saturating_sub(1).min(u16::MAX as usize) as u16
    }

    /// Handle a key press.
    pub fn on_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Char('?') => self.show_help = !self.show_help,
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => self.next_tab(),
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => self.prev_tab(),
            KeyCode::Char(c @ '1'..='5') => {
                let i = (c as usize) - ('1' as usize);
                self.switch(Tab::ALL[i]);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.scroll = self.scroll.saturating_add(1).min(self.max_scroll());
            }
            KeyCode::Up | KeyCode::Char('k') => self.scroll = self.scroll.saturating_sub(1),
            _ => {}
        }
    }
}

/// Enter the alternate screen and run the dashboard until the user quits.
pub fn run(stats: Stats, db_label: String) -> Result<()> {
    let guard = TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend).context("creating terminal")?;
    let mut app = App::new(stats, db_label);

    let result = event_loop(&mut terminal, &mut app);
    guard.restore()?;
    result
}

fn event_loop(terminal: &mut Terminal<CrosstermBackend<Stdout>>, app: &mut App) -> Result<()> {
    while !app.should_quit {
        terminal.draw(|frame| draw(frame, app))?;
        if event::poll(Duration::from_millis(250))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            app.on_key(key);
        }
    }
    Ok(())
}

/// Ensure the terminal is restored on exit and on panic.
struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("enabling raw mode")?;
        execute!(io::stdout(), EnterAlternateScreen).context("entering alternate screen")?;
        Ok(Self)
    }

    fn restore(&self) -> Result<()> {
        let _ = disable_raw_mode();
        execute!(io::stdout(), LeaveAlternateScreen).context("leaving alternate screen")?;
        Ok(())
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

// --- rendering ----------------------------------------------------------

fn draw(frame: &mut Frame, app: &App) {
    let areas = Layout::vertical([
        Constraint::Length(4),
        Constraint::Length(3),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .split(frame.area());

    draw_header(frame, app, areas[0]);
    draw_tabs(frame, app, areas[1]);
    match app.tab {
        Tab::Overview => draw_overview(frame, app, areas[2]),
        Tab::Timeline => draw_timeline(frame, app, areas[2]),
        Tab::People => draw_people(frame, app, areas[2]),
        Tab::Venues => draw_venues(frame, app, areas[2]),
        Tab::Health => draw_health(frame, app, areas[2]),
    }
    draw_footer(frame, areas[3]);

    if app.show_help {
        draw_help(frame, frame.area());
    }
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let s = &app.stats;
    let text = vec![
        Line::from(vec![
            Span::styled(
                "biblio",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!("  ·  {}", app.db_label)),
        ]),
        Line::from(format!(
            "{} papers   {} authors   {} DOIs   {} arXiv   {} pages   {}",
            fmt_int(s.total_papers),
            fmt_int(s.distinct_authors),
            fmt_int(s.distinct_dois),
            fmt_int(s.distinct_arxiv),
            fmt_int(s.total_pages),
            human_bytes(s.total_bytes),
        )),
    ];
    let block = Block::default().borders(Borders::ALL).title(" catalog ");
    frame.render_widget(Paragraph::new(text).block(block), area);
}

fn draw_tabs(frame: &mut Frame, app: &App, area: Rect) {
    let titles: Vec<Line> = Tab::ALL.iter().map(|t| Line::from(t.title())).collect();
    let tabs = Tabs::new(titles)
        .select(app.tab.index())
        .highlight_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .block(Block::default().borders(Borders::ALL));
    frame.render_widget(tabs, area);
}

fn draw_footer(frame: &mut Frame, area: Rect) {
    let hints = " q quit   Tab/←→ switch   1-5 tabs   j/k scroll   ? help ";
    frame.render_widget(
        Paragraph::new(hints).style(Style::default().fg(Color::DarkGray)),
        area,
    );
}

fn draw_overview(frame: &mut Frame, app: &App, area: Rect) {
    let rows = Layout::vertical([Constraint::Length(3), Constraint::Min(0)]).split(area);
    let line = Line::from(format!(
        "categories: {}   file types: {}   distinct titles: {}   size: {}",
        app.stats.by_category.len(),
        app.stats.by_filetype.len(),
        fmt_int(app.stats.total_papers),
        human_bytes(app.stats.total_bytes),
    ));
    frame.render_widget(
        Paragraph::new(line).block(Block::default().borders(Borders::ALL).title(" summary ")),
        rows[0],
    );

    let cols =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).split(rows[1]);
    draw_category_chart(frame, &app.stats, cols[0]);
    draw_coverage(frame, &app.stats, cols[1]);
}

fn draw_category_chart(frame: &mut Frame, stats: &Stats, area: Rect) {
    let bars: Vec<Bar> = stats
        .by_category
        .iter()
        .map(|c| Bar::with_label(c.label.clone(), c.value.max(0) as u64))
        .collect();
    // Horizontal bars reserve a column sized to the longest label, so labels
    // are fully legible (vertical bars clamp each label to the bar width).
    let chart = BarChart::horizontal(bars)
        .bar_gap(0)
        .bar_style(Style::default().fg(Color::Green))
        .value_style(Style::default().fg(Color::White))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" by category "),
        );
    frame.render_widget(chart, area);
}

fn draw_coverage(frame: &mut Frame, stats: &Stats, area: Rect) {
    let lines: Vec<Line> = stats
        .coverage
        .iter()
        .map(|c| {
            let ratio = if c.total > 0 {
                c.present as f64 / c.total as f64
            } else {
                0.0
            };
            Line::from(format!(
                "{:<10} {:>4}/{:<4} {:>3}% {}",
                c.field,
                c.present,
                c.total,
                (ratio * 100.0).round() as i64,
                meter(ratio, 12),
            ))
        })
        .collect();
    frame.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" coverage ")),
        area,
    );
}

fn draw_timeline(frame: &mut Frame, app: &App, area: Rect) {
    let rows = Layout::vertical([Constraint::Min(0), Constraint::Length(6)]).split(area);

    let all = &app.stats.by_year;
    let visible = (rows[0].height.saturating_sub(2) as usize).max(1);
    let max_start = all.len().saturating_sub(visible);
    let start = (app.scroll as usize).min(max_start);
    let end = (start + visible).min(all.len());
    let window = &all[start..end];

    let bars: Vec<Bar> = window
        .iter()
        .map(|c| Bar::with_label(c.label.clone(), c.value.max(0) as u64))
        .collect();
    let title = if all.is_empty() {
        " papers per year ".to_string()
    } else {
        format!(" papers per year ({}-{} / {}) ", start + 1, end, all.len())
    };
    let chart = BarChart::horizontal(bars)
        .bar_gap(0)
        .bar_style(Style::default().fg(Color::Blue))
        .value_style(Style::default().fg(Color::White))
        .block(Block::default().borders(Borders::ALL).title(title));
    frame.render_widget(chart, rows[0]);

    let spark: Vec<u64> = app
        .stats
        .growth
        .iter()
        .map(|c| c.value.max(0) as u64)
        .collect();
    let sparkline = Sparkline::default()
        .data(spark)
        .style(Style::default().fg(Color::Magenta))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" papers first seen per month "),
        );
    frame.render_widget(sparkline, rows[1]);
}

fn draw_people(frame: &mut Frame, app: &App, area: Rect) {
    draw_rank_table(
        frame,
        &app.stats.top_authors,
        " top authors ",
        app.scroll,
        area,
    );
}

fn draw_venues(frame: &mut Frame, app: &App, area: Rect) {
    let bars: Vec<Bar> = app
        .stats
        .top_venues
        .iter()
        .map(|c| Bar::with_label(c.label.clone(), c.value.max(0) as u64))
        .collect();
    let chart = BarChart::horizontal(bars)
        .bar_gap(0)
        .bar_style(Style::default().fg(Color::Yellow))
        .value_style(Style::default().fg(Color::White))
        .block(Block::default().borders(Borders::ALL).title(" top venues "));
    frame.render_widget(chart, area);
}

fn draw_health(frame: &mut Frame, app: &App, area: Rect) {
    let a = &app.stats.anomalies;
    let mut items: Vec<ListItem> = Vec::new();
    items.push(ListItem::new(Line::from(format!(
        "implausible years : {}",
        a.implausible_years.len()
    ))));
    for y in &a.implausible_years {
        items.push(ListItem::new(Line::from(format!(
            "    {:>6}  {}",
            y.year, y.path
        ))));
    }
    items.push(ListItem::new(Line::from(format!(
        "missing title+doi : {}",
        a.missing_title_doi
    ))));
    items.push(ListItem::new(Line::from(format!(
        "duplicate DOIs    : {}",
        a.duplicate_dois.len()
    ))));
    for d in &a.duplicate_dois {
        items.push(ListItem::new(Line::from(format!(
            "    x{:<3} {}",
            d.value, d.label
        ))));
    }

    let visible_rows = (area.height.saturating_sub(2) as usize).max(1);
    let start = (app.scroll as usize).min(items.len().saturating_sub(visible_rows));
    let visible: Vec<ListItem> = items.into_iter().skip(start).collect();
    frame.render_widget(
        List::new(visible).block(Block::default().borders(Borders::ALL).title(" health ")),
        area,
    );
}

fn draw_rank_table(
    frame: &mut Frame,
    rows: &[biblio::stats::Count],
    title: &str,
    scroll: u16,
    area: Rect,
) {
    let header =
        Row::new(["#", "name", "papers"]).style(Style::default().add_modifier(Modifier::BOLD));
    // Reserve two rows for borders and one for the header.
    let visible_rows = (area.height.saturating_sub(3) as usize).max(1);
    let start = (scroll as usize).min(rows.len().saturating_sub(visible_rows));
    let body: Vec<Row> = rows
        .iter()
        .enumerate()
        .skip(start)
        .map(|(i, c)| {
            Row::new([
                Cell::from((i + 1).to_string()),
                Cell::from(c.label.clone()),
                Cell::from(c.value.to_string()),
            ])
        })
        .collect();
    let table = Table::new(
        body,
        [
            Constraint::Length(4),
            Constraint::Min(20),
            Constraint::Length(8),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(title.to_string()),
    );
    frame.render_widget(table, area);
}

fn draw_help(frame: &mut Frame, area: Rect) {
    let text = vec![
        Line::from("biblio tui - help").style(Style::default().add_modifier(Modifier::BOLD)),
        Line::from(""),
        Line::from("q / Esc     quit"),
        Line::from("Tab, ← →    next / previous tab"),
        Line::from("1 - 5       jump to a tab"),
        Line::from("j / k       scroll down / up"),
        Line::from("?           toggle this help"),
    ];
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" help ")
        .style(Style::default().bg(Color::Black));
    let popup = centered(area, 44, (text.len() as u16) + 2);
    frame.render_widget(ratatui::widgets::Clear, popup);
    frame.render_widget(
        Paragraph::new(text).block(block).wrap(Wrap { trim: true }),
        popup,
    );
}

/// A rectangle centered in `area` with the given width/height (clamped).
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

fn meter(ratio: f64, width: usize) -> String {
    let filled = (ratio.clamp(0.0, 1.0) * width as f64).round() as usize;
    format!("{}{}", "█".repeat(filled), "░".repeat(width - filled))
}

#[cfg(test)]
mod tests {
    use super::*;
    use biblio::stats::{Anomalies, Count, Coverage, ProvenanceCount, YearAnomaly};
    use ratatui::backend::TestBackend;

    fn sample_stats() -> Stats {
        Stats {
            total_papers: 12,
            distinct_authors: 30,
            distinct_dois: 8,
            distinct_arxiv: 5,
            total_bytes: 2 * 1024 * 1024,
            total_pages: 240,
            by_category: vec![
                Count {
                    label: "journal".into(),
                    value: 7,
                },
                Count {
                    label: "preprints".into(),
                    value: 5,
                },
            ],
            by_filetype: vec![Count {
                label: "pdf".into(),
                value: 12,
            }],
            by_year: vec![
                Count {
                    label: "2024".into(),
                    value: 7,
                },
                Count {
                    label: "2025".into(),
                    value: 5,
                },
            ],
            coverage: vec![Coverage {
                field: "title".into(),
                present: 10,
                total: 12,
            }],
            top_authors: vec![Count {
                label: "Ada Lovelace".into(),
                value: 3,
            }],
            top_venues: vec![Count {
                label: "NeurIPS".into(),
                value: 4,
            }],
            provenance: vec![ProvenanceCount {
                field: "title".into(),
                source: "embedded".into(),
                count: 8,
            }],
            growth: vec![Count {
                label: "2026-10".into(),
                value: 12,
            }],
            anomalies: Anomalies {
                implausible_years: vec![YearAnomaly {
                    path: "a.pdf".into(),
                    year: 2097,
                }],
                missing_title_doi: 2,
                duplicate_dois: vec![Count {
                    label: "10.1/x".into(),
                    value: 2,
                }],
            },
        }
    }

    fn render(app: &App) -> String {
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn every_tab_renders_its_label() {
        for tab in Tab::ALL {
            let mut app = App::new(sample_stats(), "test.db".to_string());
            app.switch(tab);
            let text = render(&app);
            assert!(
                text.contains(tab.title()),
                "tab {} did not render its label",
                tab.title()
            );
        }
    }

    #[test]
    fn overview_shows_totals_and_coverage() {
        let app = App::new(sample_stats(), "test.db".to_string());
        let text = render(&app);
        assert!(text.contains("papers"));
        assert!(text.contains("title"));
        assert!(text.contains("by category"));
    }

    #[test]
    fn category_chart_shows_full_labels() {
        let app = App::new(sample_stats(), "test.db".to_string());
        let text = render(&app);
        // Horizontal bars reserve a label column, so full labels appear.
        assert!(text.contains("journal"), "full category label missing");
        assert!(text.contains("preprints"), "full category label missing");
    }

    #[test]
    fn timeline_shows_full_year_labels_and_scrolls() {
        let mut stats = sample_stats();
        stats.by_year = (2000..2030)
            .map(|y| Count {
                label: y.to_string(),
                value: 1,
            })
            .collect();
        let mut app = App::new(stats, "test.db".to_string());
        app.switch(Tab::Timeline);

        let first = render(&app);
        assert!(first.contains("2000"), "expected first year visible");

        app.scroll = 6;
        let scrolled = render(&app);
        assert!(
            !scrolled.contains("2000"),
            "first year should have scrolled off"
        );
        assert!(scrolled.contains("2020"), "later year should be visible");
    }

    #[test]
    fn health_shows_anomalies() {
        let mut app = App::new(sample_stats(), "test.db".to_string());
        app.switch(Tab::Health);
        let text = render(&app);
        assert!(text.contains("implausible years"));
        assert!(text.contains("10.1/x"));
    }

    #[test]
    fn keys_change_tab_and_quit() {
        let mut app = App::new(sample_stats(), "test.db".to_string());
        app.on_key(KeyEvent::from(KeyCode::Char('3')));
        assert_eq!(app.tab, Tab::People);
        app.on_key(KeyEvent::from(KeyCode::Char('q')));
        assert!(app.should_quit);
    }
}

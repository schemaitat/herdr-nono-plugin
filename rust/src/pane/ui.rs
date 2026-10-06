//! The overlay's screen, drawn with ratatui: a title bar, the agents table, then
//! either the selected agent's two sandboxes (with their processes) and its
//! details, or the profiles view; a status line and the key hints at the
//! bottom. Pure: everything it shows comes from the [`ViewState`].

use std::path::PathBuf;

use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Padding, Paragraph, Row as TableRow, Table};
use ratatui::Frame;
use serde_json::Value;

use super::model::{
    fit, is_stale, network_detail, network_label, profile_name, profile_source, session_cell,
    short_command, short_path, Collected, Configured, Row, Tone, TreeProcess,
};
use crate::util::{local_clock, parse_iso_ms};

/// Which view fills the area below the agents table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Sandboxes,
    Profiles,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    Ok,
    Error,
    Info,
}

/// A line under the tables: what just happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub text: String,
    pub kind: StatusKind,
}

impl Status {
    pub fn new(kind: StatusKind, text: impl Into<String>) -> Self {
        Status {
            text: text.into(),
            kind,
        }
    }
}

/// Where the profiles view says things live.
#[derive(Debug, Clone, Default)]
pub struct ProfilesInfo {
    pub plugin_root: Option<String>,
    pub config_file: Option<String>,
    pub user_profiles_dir: PathBuf,
}

/// Everything the screen shows.
#[derive(Debug, Clone)]
pub struct ViewState {
    pub rows: Vec<Row>,
    pub session_error: Option<String>,
    pub pane_ids: Option<Vec<String>>,
    pub selected: usize,
    pub status: Option<Status>,
    pub prompt: Option<String>,
    pub mode: Mode,
    /// The key reference is on screen.
    pub help: bool,
    pub configured: Option<Configured>,
    pub profiles: ProfilesInfo,
}

impl ViewState {
    pub fn new(profiles: ProfilesInfo) -> Self {
        ViewState {
            rows: Vec::new(),
            session_error: None,
            pane_ids: None,
            selected: 0,
            status: None,
            prompt: None,
            mode: Mode::Sandboxes,
            help: false,
            configured: None,
            profiles,
        }
    }

    /// Replaces the data with a fresh frame's, keeping the selection on the same pane when it still exists.
    pub fn apply(&mut self, data: Collected) {
        let keep = self.rows.get(self.selected).map(|row| row.pane_id.clone());
        self.rows = data.rows;
        self.session_error = data.session_error;
        self.pane_ids = data.pane_ids;
        self.configured = data.configured;
        let again = keep.and_then(|pane| self.rows.iter().position(|row| row.pane_id == pane));
        self.selected =
            again.unwrap_or_else(|| self.selected.min(self.rows.len().saturating_sub(1)));
    }

    pub fn selected_row(&self) -> Option<&Row> {
        self.rows.get(self.selected)
    }
}

/// What changes between draws without being part of the data.
#[derive(Debug, Clone)]
pub struct DrawContext {
    pub clock: String,
    pub interval_secs: u64,
    pub home: String,
}

fn color_of(tone: Tone) -> Style {
    match tone {
        Tone::Plain => Style::new(),
        Tone::Gray => Style::new().fg(Color::DarkGray),
        Tone::Green => Style::new().fg(Color::Green),
        Tone::Yellow => Style::new().fg(Color::Yellow),
        Tone::Red => Style::new().fg(Color::Red),
        Tone::Cyan => Style::new().fg(Color::Cyan),
        Tone::Magenta => Style::new().fg(Color::Magenta),
        Tone::Blue => Style::new().fg(Color::Blue),
    }
}

fn bold(style: Style) -> Style {
    style.add_modifier(Modifier::BOLD)
}

fn span(text: impl Into<String>, style: Style) -> Span<'static> {
    Span::styled(text.into(), style)
}

/// A block title that reads `┌─ Title ───`.
fn titled(text: &str, style: Style) -> Line<'static> {
    Line::from(vec![
        span("─ ", Style::new()),
        span(text.to_string(), bold(style)),
        span(" ", Style::new()),
    ])
}

fn framed(title: Line<'static>, border: Tone) -> Block<'static> {
    Block::new()
        .borders(Borders::ALL)
        .border_style(color_of(border))
        .title(title)
        .padding(Padding::horizontal(1))
}

fn clock_of(iso: Option<&Value>) -> String {
    iso.and_then(Value::as_str)
        .and_then(parse_iso_ms)
        .map_or_else(|| "-".to_string(), local_clock)
}

fn entry_text(entry: &crate::state::Entry, key: &str) -> Option<String> {
    entry
        .get(key)
        .filter(|value| !value.is_null())
        .map(crate::util::js_string)
}

/// Draws the whole screen into `frame`.
pub fn draw(frame: &mut Frame, view: &ViewState, ctx: &DrawContext) {
    let area = frame.area();
    let width = area.width as usize;
    let height = area.height as usize;
    let rows = &view.rows;
    let selected = if rows.is_empty() {
        None
    } else {
        Some(view.selected.min(rows.len() - 1))
    };
    let row = selected.map(|index| &rows[index]);
    let y = std::cell::Cell::new(0u16);
    let mut take = |lines: usize| -> Rect {
        let top = y.get().min(area.height);
        let available = (area.height - top) as usize;
        let taken = lines.min(available);
        y.set(top + taken as u16);
        Rect {
            x: area.x,
            y: area.y + top,
            width: area.width,
            height: taken as u16,
        }
    };

    // Title bar.
    let running = rows
        .iter()
        .filter(|item| item.running == Some(true))
        .count();
    let failed = rows
        .iter()
        .filter(|item| item.verified == Some(false))
        .count();
    let stale = rows.iter().filter(|item| is_stale(item)).count();
    let right = format!(
        "{} agent{} · {running} running{}{} · {} ",
        rows.len(),
        if rows.len() == 1 { "" } else { "s" },
        if failed > 0 {
            format!(" · {failed} FAILED")
        } else {
            String::new()
        },
        if stale > 0 {
            format!(" · {stale} stale")
        } else {
            String::new()
        },
        ctx.clock
    );
    let left = " nono sandboxes ";
    let gap = width
        .saturating_sub(left.chars().count() + right.chars().count())
        .max(1);
    let title: String = format!("{left}{}{right}", " ".repeat(gap))
        .chars()
        .take(width)
        .collect();
    frame.render_widget(
        Paragraph::new(title).style(Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD)),
        take(1),
    );

    // Space: the table gets up to six rows, the sandbox boxes what is left after
    // the details panel; on a short screen the details panel goes first.
    let footer_lines = 2usize;
    let table_rows = rows.len().clamp(1, 6);
    let table_height = table_rows + 3;
    let mut detail_height: usize = if row.is_some() { 9 } else { 0 };
    let remaining = |details: usize| {
        (height as i64) - 1 - table_height as i64 - footer_lines as i64 - details as i64 - 1
    };
    let mut sandbox_height = remaining(detail_height);
    if row.is_some() && sandbox_height < 5 {
        detail_height = 0;
        sandbox_height = remaining(0);
    }

    // Agents table.
    let table_area = take(table_height);
    draw_agents(frame, table_area, view, selected, table_rows, ctx);

    if view.help {
        let room = (height as i64
            - footer_lines as i64
            - y.get() as i64
            - 2
            - i64::from(view.session_error.is_some()))
        .max(1) as usize;
        let mut body = help_lines();
        body.truncate(room);
        let panel_area = take(room.min(body.len()) + 2);
        frame.render_widget(
            Paragraph::new(body).block(framed(titled("Keys", Style::new()), Tone::Yellow)),
            panel_area,
        );
    } else if view.mode == Mode::Profiles {
        let room = (height as i64
            - footer_lines as i64
            - y.get() as i64
            - 2
            - i64::from(view.session_error.is_some()))
        .max(1) as usize;
        let inner_width = width.saturating_sub(4);
        let body = profile_lines(view, row, inner_width, &ctx.home);
        let shown: Vec<Line<'static>> = if body.len() <= room {
            body
        } else {
            let mut shown: Vec<Line<'static>> = body[..room - 1].to_vec();
            shown.push(Line::from(span(
                fit(
                    &format!("… {} more lines; enlarge the pane", body.len() - room + 1),
                    inner_width,
                    false,
                ),
                color_of(Tone::Gray),
            )));
            shown
        };
        let title_text = row.map_or_else(
            || "Profiles · next launch".to_string(),
            |row| format!("Profiles · agent {}", row.pane_id),
        );
        let panel_area = take(room.max(shown.len()) + 2);
        frame.render_widget(
            Paragraph::new(shown).block(framed(titled(&title_text, Style::new()), Tone::Yellow)),
            panel_area,
        );
    } else if let Some(row) = row {
        draw_flow(frame, take(1), row, width);
        draw_sandboxes(frame, &mut take, row, width, sandbox_height);
        if detail_height > 0 {
            let details_area = take(detail_height);
            draw_details(frame, details_area, row, &ctx.home);
        }
    }
    if let Some(error) = &view.session_error {
        let area = take(1);
        frame.render_widget(
            Paragraph::new(fit(&format!(" nono ps failed: {error}"), width, false))
                .style(color_of(Tone::Red)),
            area,
        );
    }

    // Status line and key hints, directly under the content so a pane that is
    // taller than the visible area still shows them; with no room they sit at the bottom.
    if height >= footer_lines {
        let top = (y.get() as usize + 1).min(height - footer_lines);
        let status_area = Rect {
            x: area.x,
            y: area.y + top as u16,
            width: area.width,
            height: 1,
        };
        let keys_area = Rect {
            x: area.x,
            y: area.y + (top + 1) as u16,
            width: area.width,
            height: 1,
        };
        let status_line = if let Some(prompt) = &view.prompt {
            Line::from(span(
                fit(&format!(" ? {prompt}"), width, false),
                bold(color_of(Tone::Yellow)),
            ))
        } else if let Some(status) = &view.status {
            let (mark, tone) = match status.kind {
                StatusKind::Error => ("✖", Tone::Red),
                StatusKind::Ok => ("✔", Tone::Green),
                StatusKind::Info => ("…", Tone::Cyan),
            };
            Line::from(span(
                fit(&format!(" {mark} {}", status.text), width, false),
                color_of(tone),
            ))
        } else {
            Line::from(span(
                fit(
                    &format!(" refreshes every {}s", ctx.interval_secs),
                    width,
                    false,
                ),
                color_of(Tone::Gray),
            ))
        };
        frame.render_widget(Paragraph::new(status_line), status_area);
        frame.render_widget(Paragraph::new(key_hints(view, width)), keys_area);
    }
}

/// The key reference the `?` screen shows: the overlay's keys, then the chords
/// Herdr opens the plugin's actions with.
fn help_lines() -> Vec<Line<'static>> {
    let key = |keys: &str, does: &str| {
        Line::from(vec![
            span(format!(" {keys:<14}"), bold(color_of(Tone::Cyan))),
            span(does.to_string(), Style::new()),
        ])
    };
    let heading = |text: &str| Line::from(span(format!(" {text}"), bold(Style::new())));
    vec![
        heading("In this overlay"),
        key("↑ ↓  j k", "select an agent (PgUp PgDn jump five)"),
        key("enter  g", "jump to the agent's pane and close the overlay"),
        key("v", "verify the agent's confinement now"),
        key("x", "stop the agent and its server (asks first)"),
        key("p", "forget mappings whose pane is gone (asks first)"),
        key(
            "a",
            "clean up all: stop every agent, forget every mapping (asks first)",
        ),
        key("i", "show the profiles, or go back to the sandboxes"),
        key("r", "refresh now"),
        key("?", "show or hide this help"),
        key("q  ctrl+c", "close the overlay"),
        Line::default(),
        heading("From any pane (prefix chords, after install-keybindings)"),
        key("prefix+shift+a", "start-agent: a new sandboxed agent"),
        key("prefix+shift+o", "sandboxes: this overlay"),
        key("prefix+shift+b", "reconnect: resume in fresh sandboxes"),
        key(
            "prefix+shift+s",
            "open-shell: a shell under the server's policy",
        ),
    ]
}

fn key_hints(view: &ViewState, width: usize) -> Line<'static> {
    let mut keys: Vec<(&str, &str)> = if view.help {
        vec![("?/esc", "back"), ("q", "quit")]
    } else if view.prompt.is_some() {
        vec![("y", "confirm"), ("n/esc", "cancel")]
    } else if view.mode == Mode::Profiles {
        vec![
            ("↑↓/jk", "select"),
            ("i", "sandboxes"),
            ("r", "refresh"),
            ("?", "help"),
            ("q", "quit"),
        ]
    } else {
        vec![
            ("↑↓/jk", "select"),
            ("⏎", "jump"),
            ("v", "verify"),
            ("x", "stop"),
            ("p", "prune"),
            ("a", "clean all"),
            ("i", "profiles"),
            ("r", "refresh"),
            ("?", "help"),
            ("q", "quit"),
        ]
    };
    let plain_len = |keys: &[(&str, &str)]| {
        keys.iter()
            .map(|(key, label)| format!(" {key}  {label}"))
            .collect::<Vec<_>>()
            .join("  ")
            .chars()
            .count()
    };
    // On a narrow screen the least needed hints go first; their keys still work.
    for drop in ["r", "a", "p", "v", "x"] {
        if plain_len(&keys) < width {
            break;
        }
        keys.retain(|(key, _)| *key != drop);
    }
    let mut spans = Vec::new();
    for (index, (key, label)) in keys.iter().enumerate() {
        if index > 0 {
            spans.push(span("  ", Style::new()));
        }
        spans.push(span(
            format!(" {key} "),
            Style::new().add_modifier(Modifier::REVERSED),
        ));
        spans.push(span(format!(" {label}"), Style::new()));
    }
    Line::from(spans)
}

fn draw_agents(
    frame: &mut Frame,
    area: Rect,
    view: &ViewState,
    selected: Option<usize>,
    table_rows: usize,
    ctx: &DrawContext,
) {
    let rows = &view.rows;
    let block = framed(titled("Agents", Style::new()), Tone::Gray);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let inner_width = inner.width as usize;
    let widths = [3usize, 10, 14, 9, 8, 19, 9];
    let fixed: usize = widths.iter().sum::<usize>() + 7;
    let dir_width = inner_width.saturating_sub(fixed).max(8);
    let mut constraints: Vec<Constraint> = widths
        .iter()
        .map(|width| Constraint::Length(*width as u16))
        .collect();
    constraints.push(Constraint::Length(dir_width as u16));
    let gray_bold = bold(color_of(Tone::Gray));
    let header = TableRow::new(
        [
            "",
            "PANE",
            "SESSION",
            "AGENT",
            "STATE",
            "NONO",
            "VERIFIED",
            "DIRECTORY",
        ]
        .iter()
        .zip(widths.iter().copied().chain(std::iter::once(dir_width)))
        .map(|(label, width)| Cell::from(span(fit(label, width, false), gray_bold))),
    );
    if rows.is_empty() {
        let table = Table::new(Vec::<TableRow>::new(), constraints)
            .header(header)
            .column_spacing(1);
        frame.render_widget(table, inner);
        let message = Rect {
            x: inner.x,
            y: inner.y + 1,
            width: inner.width,
            height: inner.height.saturating_sub(1).min(1),
        };
        frame.render_widget(
            Paragraph::new(fit(
                "No sandboxed agents. Press ctrl+b, shift+a in a project pane to start one.",
                inner_width,
                false,
            )),
            message,
        );
        return;
    }
    let selected = selected.unwrap_or(0);
    let first = selected
        .saturating_sub(table_rows - 1)
        .min(rows.len().saturating_sub(table_rows));
    let body: Vec<TableRow> = (first..first + table_rows)
        .map(|index| {
            let Some(item) = rows.get(index) else {
                return TableRow::new(vec![Cell::from("")]);
            };
            let is_selected = index == selected;
            let (dot, dot_tone) = if item.running == Some(true) {
                ("●", Tone::Green)
            } else if item.lifecycle_state == "failed" {
                ("✖", Tone::Red)
            } else {
                ("○", Tone::Gray)
            };
            let pane = if item.pane_exists == Some(false) {
                format!("{} ✗", item.pane_id)
            } else {
                item.pane_id.clone()
            };
            let (nono_text, nono_tone) = session_cell(item);
            let (verified_text, verified_style) = match item.verified {
                None => ("-".to_string(), color_of(Tone::Gray)),
                Some(true) => ("✔ ok".to_string(), color_of(Tone::Green)),
                Some(false) => ("✖ FAILED".to_string(), bold(color_of(Tone::Red))),
            };
            let state_tone = match item.lifecycle_state.as_str() {
                "running" => Tone::Green,
                "failed" => Tone::Red,
                "exited" => Tone::Yellow,
                _ => Tone::Plain,
            };
            let short_session = format!("…{}", item.session_name.rsplit('-').next().unwrap_or(""));
            let mark_style = if is_selected {
                bold(color_of(Tone::Cyan))
            } else {
                color_of(dot_tone)
            };
            let cells = [
                (
                    fit(
                        &format!("{}{dot}", if is_selected { "▶" } else { " " }),
                        widths[0],
                        false,
                    ),
                    mark_style,
                ),
                (
                    fit(&pane, widths[1], false),
                    if item.pane_exists == Some(false) {
                        color_of(Tone::Gray)
                    } else {
                        Style::new()
                    },
                ),
                (fit(&short_session, widths[2], false), Style::new()),
                (fit(&item.agent_kind, widths[3], false), Style::new()),
                (
                    fit(&item.lifecycle_state, widths[4], false),
                    color_of(state_tone),
                ),
                (fit(&nono_text, widths[5], false), color_of(nono_tone)),
                (fit(&verified_text, widths[6], false), verified_style),
                (
                    fit(&short_path(&item.local_path, &ctx.home), dir_width, true),
                    color_of(Tone::Gray),
                ),
            ];
            if is_selected {
                // The selected row is one inverse band, without the other colours.
                TableRow::new(cells.into_iter().map(|(text, _)| Cell::from(text)))
                    .style(Style::new().add_modifier(Modifier::REVERSED))
            } else {
                TableRow::new(
                    cells
                        .into_iter()
                        .map(|(text, style)| Cell::from(span(text, style))),
                )
            }
        })
        .collect();
    frame.render_widget(
        Table::new(body, constraints)
            .header(header)
            .column_spacing(1),
        inner,
    );
}

fn has_server(row: &Row) -> bool {
    entry_text(&row.entry, "serverProfile").is_some_and(|text| !text.is_empty())
        || !row.trees.server.is_empty()
}

fn draw_flow(frame: &mut Frame, area: Rect, row: &Row, width: usize) {
    let port = row.entry.get("port").and_then(Value::as_u64);
    let server = has_server(row);
    let tools_egress = if server {
        row.network.server.as_ref()
    } else {
        row.network.client.as_ref()
    }
    .map(|summary| summary.egress.clone());
    let blocked = if tools_egress.as_deref() == Some("open") {
        "  ✖ Herdr ✖ systemd ✖ ssh-agent ! localhost open"
    } else {
        "  ✖ localhost ✖ Herdr ✖ systemd ✖ ssh-agent"
    };
    let mut flow: Vec<(String, Tone)> = if server {
        vec![
            ("client".into(), Tone::Cyan),
            (
                format!(
                    " ──{}──▶ ",
                    port.map_or_else(String::new, |port| format!(":{port}"))
                ),
                Tone::Gray,
            ),
            ("server".into(), Tone::Magenta),
            (" ──▶ ".into(), Tone::Gray),
            (
                (if tools_egress.as_deref() == Some("open") {
                    "direct"
                } else {
                    "nono proxy"
                })
                .into(),
                Tone::Blue,
            ),
            (" ──▶ internet".into(), Tone::Gray),
        ]
    } else {
        vec![
            ("sandbox".into(), Tone::Cyan),
            (" ──▶ ".into(), Tone::Gray),
            (
                tools_egress.clone().unwrap_or_else(|| "network".into()),
                Tone::Gray,
            ),
        ]
    };
    flow.push((blocked.into(), Tone::Red));
    let plain = format!(
        " {}",
        flow.iter()
            .map(|(text, _)| text.as_str())
            .collect::<String>()
    );
    let line = if plain.chars().count() <= width {
        let mut spans = vec![span(" ", Style::new())];
        spans.extend(flow.into_iter().map(|(text, tone)| {
            span(
                text,
                if tone == Tone::Red {
                    color_of(tone)
                } else {
                    bold(color_of(tone))
                },
            )
        }));
        Line::from(spans)
    } else {
        Line::from(span(fit(&plain, width, false), color_of(Tone::Gray)))
    };
    frame.render_widget(Paragraph::new(line), area);
}

fn process_lines(
    tree: &[TreeProcess],
    inner_width: usize,
    client_side: bool,
    empty_text: &str,
) -> Vec<Line<'static>> {
    if tree.is_empty() {
        return vec![Line::from(span(
            fit(empty_text, inner_width, false),
            color_of(Tone::Gray),
        ))];
    }
    tree.iter()
        .map(|process| {
            let role = if client_side {
                if process.depth == 0 {
                    "tui"
                } else {
                    "child"
                }
            } else if process.depth == 0 {
                "server"
            } else {
                "tool"
            };
            let mark = if process.confined { "✔" } else { "✖" };
            let prefix_len = format!("{mark} {:>7} {} ", process.pid, fit(role, 6, false))
                .chars()
                .count();
            let indent = if process.depth > 0 {
                format!("{}└ ", "  ".repeat(process.depth - 1))
            } else {
                String::new()
            };
            let command = fit(
                &format!("{indent}{}", short_command(&process.argv)),
                inner_width.saturating_sub(prefix_len),
                false,
            );
            let role_style = match role {
                "tui" => bold(color_of(Tone::Cyan)),
                "server" => bold(color_of(Tone::Magenta)),
                "tool" => color_of(Tone::Yellow),
                _ => Style::new(),
            };
            Line::from(vec![
                span(
                    mark,
                    color_of(if process.confined {
                        Tone::Green
                    } else {
                        Tone::Red
                    }),
                ),
                span(" ", Style::new()),
                span(format!("{:>7}", process.pid), color_of(Tone::Gray)),
                span(" ", Style::new()),
                span(fit(role, 6, false), role_style),
                span(" ", Style::new()),
                span(
                    command,
                    if process.confined {
                        Style::new()
                    } else {
                        color_of(Tone::Red)
                    },
                ),
            ])
        })
        .collect()
}

/// Draws the selected agent's client and server boxes side by side.
fn draw_sandboxes(
    frame: &mut Frame,
    take: &mut dyn FnMut(usize) -> Rect,
    row: &Row,
    width: usize,
    sandbox_height: i64,
) {
    let port = row.entry.get("port").and_then(Value::as_u64);
    let server = has_server(row);
    let left_width = if server { width / 2 } else { width };
    let right_width = width - left_width;
    let left_label = if server || row.network.client.is_some() {
        network_label(row.network.client.as_ref(), true, port)
    } else {
        "network unknown".to_string()
    };
    let left_title = fit(
        &format!(
            "{} · {left_label}",
            if server { "client" } else { "sandbox" }
        ),
        left_width.saturating_sub(6),
        false,
    )
    .trim_end()
    .to_string();
    let left_inner = left_width.saturating_sub(4);
    let right_inner = right_width.saturating_sub(4);
    let left_empty = if row.running == Some(true) {
        "starting…"
    } else {
        "not running"
    };
    let right_empty = if row.server_running == Some(true) {
        "starting…"
    } else {
        "not running"
    };
    let left_body = process_lines(&row.trees.client, left_inner, true, left_empty);
    let right_body = if server {
        process_lines(&row.trees.server, right_inner, false, right_empty)
    } else {
        Vec::new()
    };
    // The boxes take all the height the table and the details leave, so the screen is filled;
    // a longer process list is clipped with a count.
    let body_height = (sandbox_height - 2).max(1) as usize;
    let clip = |mut body: Vec<Line<'static>>, inner: usize| -> Vec<Line<'static>> {
        if body.len() > body_height {
            let hidden = body.len() - body_height + 1;
            body.truncate(body_height - 1);
            body.push(Line::from(span(
                fit(&format!("… {hidden} more"), inner, false),
                color_of(Tone::Gray),
            )));
        }
        body
    };
    let area = take(body_height + 2);
    let left_area = Rect {
        width: left_width as u16,
        ..area
    };
    frame.render_widget(
        Paragraph::new(clip(left_body, left_inner)).block(framed(
            titled(&left_title, color_of(Tone::Cyan)),
            Tone::Cyan,
        )),
        left_area,
    );
    if server {
        let right_title = fit(
            &format!(
                "server · {}",
                network_label(row.network.server.as_ref(), false, port)
            ),
            right_width.saturating_sub(6),
            false,
        )
        .trim_end()
        .to_string();
        let right_area = Rect {
            x: area.x + left_width as u16,
            width: right_width as u16,
            ..area
        };
        frame.render_widget(
            Paragraph::new(clip(right_body, right_inner)).block(framed(
                titled(&right_title, color_of(Tone::Magenta)),
                Tone::Magenta,
            )),
            right_area,
        );
    }
}

fn draw_details(frame: &mut Frame, area: Rect, row: &Row, home: &str) {
    let inner = area.width.saturating_sub(4) as usize;
    let entry = &row.entry;
    let field = |label: &str, value: String, style: Style| -> Line<'static> {
        Line::from(vec![
            span(fit(label, 12, false), color_of(Tone::Gray)),
            span(fit(&value, inner.saturating_sub(12), false), style),
        ])
    };
    let mut out: Vec<Line<'static>> = vec![field(
        "Session",
        row.session_name.clone(),
        bold(Style::new()),
    )];
    let pane_state = match row.pane_exists {
        Some(false) => "(gone)",
        None => "(?)",
        Some(true) => "(open)",
    };
    let workspace = entry_text(entry, "workspaceId")
        .filter(|id| !id.is_empty())
        .map_or_else(String::new, |id| format!("  workspace {id}"));
    out.push(field(
        "Pane",
        format!("{} {pane_state}{workspace}", row.pane_id),
        Style::new(),
    ));
    let workdir = entry_text(entry, "workdir")
        .filter(|workdir| !workdir.is_empty() && *workdir != row.local_path);
    out.push(field(
        "Directory",
        format!(
            "{}{}",
            short_path(&row.local_path, home),
            workdir.map_or_else(String::new, |workdir| format!(
                "  (starts in {})",
                short_path(&workdir, home)
            ))
        ),
        Style::new(),
    ));
    let profile = |key: &str| {
        entry_text(entry, key)
            .filter(|text| !text.is_empty())
            .map_or_else(|| "-".to_string(), |text| profile_name(&text))
    };
    let server_part = entry_text(entry, "serverProfile")
        .filter(|text| !text.is_empty())
        .map_or_else(String::new, |text| {
            format!(" · server {}", profile_name(&text))
        });
    out.push(field(
        "Profiles",
        format!("client {}{server_part}", profile("profile")),
        Style::new(),
    ));
    let report = entry.get("verification").filter(|value| !value.is_null());
    match report {
        Some(report) => out.push(field(
            "Verified",
            format!(
                "{} ({})",
                row.verification.clone().unwrap_or_default(),
                clock_of(report.get("checkedAt"))
            ),
            color_of(if report.get("ok").and_then(Value::as_bool) == Some(true) {
                Tone::Green
            } else {
                Tone::Red
            }),
        )),
        None => out.push(field("Verified", "not yet".into(), color_of(Tone::Gray))),
    }
    let exit = entry_text(entry, "lastExitCode")
        .map_or_else(String::new, |code| format!(" · exit {code}"));
    let launches = entry
        .get("launchCount")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    out.push(field(
        "Last launch",
        format!(
            "{}{exit} · {} launch{}",
            clock_of(entry.get("lastLaunchAt")),
            crate::util::js_string(&Value::from(launches)),
            if launches == 1.0 { "" } else { "es" }
        ),
        Style::new(),
    ));
    if let Some(error) = entry.get("lastError").filter(|error| !error.is_null()) {
        let show = |key: &str| {
            error
                .get(key)
                .map_or_else(String::new, crate::util::js_string)
        };
        out.push(field(
            "Last error",
            format!("{}: {}", show("kind"), show("message")),
            color_of(Tone::Red),
        ));
    } else if report
        .and_then(|report| report.get("ok"))
        .and_then(Value::as_bool)
        == Some(false)
    {
        let problems = report
            .and_then(|report| report.get("problems"))
            .and_then(Value::as_array);
        let problem = problems
            .and_then(|list| list.get(1).or_else(|| list.first()))
            .and_then(Value::as_str)
            .unwrap_or("");
        out.push(field("Problem", problem.to_string(), color_of(Tone::Red)));
    }
    out.truncate((area.height as usize).saturating_sub(2));
    frame.render_widget(
        Paragraph::new(out).block(framed(titled("Details", Style::new()), Tone::Gray)),
        area,
    );
}

/// The lines of the profiles view: for each sandbox of the selected agent (or of
/// the next launch when nothing is mapped) which profile it runs under, where
/// that profile lives and what it allows; then what the next launch uses and
/// where to change it.
fn profile_lines(
    view: &ViewState,
    row: Option<&Row>,
    inner_width: usize,
    home: &str,
) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    let info = &view.profiles;
    let configured = view.configured.as_ref();
    let field = |out: &mut Vec<Line<'static>>, label: &str, value: String, style: Style| {
        out.push(Line::from(vec![
            span(fit(&format!("  {label}"), 14, false), color_of(Tone::Gray)),
            span(fit(&value, inner_width.saturating_sub(14), false), style),
        ]));
    };
    let heading = |out: &mut Vec<Line<'static>>, text: &str, tone: Tone| {
        out.push(Line::from(span(
            fit(text, inner_width, false),
            bold(color_of(tone)),
        )))
    };
    type Side<'a> = (
        &'static str,
        Option<String>,
        Option<&'a crate::nono::ProfileSummary>,
    );
    let sides: Vec<Side> = if let Some(row) = row {
        let profile = entry_text(&row.entry, "profile");
        match entry_text(&row.entry, "serverProfile").filter(|text| !text.is_empty()) {
            Some(server) => vec![
                ("server", Some(server), row.network.server.as_ref()),
                ("client", profile, row.network.client.as_ref()),
            ],
            None => vec![("sandbox", profile, row.network.client.as_ref())],
        }
    } else if let Some(configured) = configured {
        if configured.server.reference.is_some() {
            vec![
                (
                    "server",
                    configured.server.reference.clone(),
                    configured.server.summary.as_ref(),
                ),
                (
                    "client",
                    configured.client.reference.clone(),
                    configured.client.summary.as_ref(),
                ),
            ]
        } else {
            vec![(
                "sandbox",
                configured.client.reference.clone(),
                configured.client.summary.as_ref(),
            )]
        }
    } else {
        Vec::new()
    };
    if sides.is_empty() {
        out.push(Line::from(span(
            fit(
                "No agent selected and the configured profiles are unknown (see config.json).",
                inner_width,
                false,
            ),
            color_of(Tone::Gray),
        )));
    }
    for (side, reference, summary) in sides {
        let (title, key, tone) = match side {
            "server" => (
                "server sandbox: opencode serve and every tool call",
                "serverProfile",
                Tone::Magenta,
            ),
            "client" => (
                "client sandbox: the OpenCode TUI in the pane",
                "profile",
                Tone::Cyan,
            ),
            _ => ("sandbox: the agent", "profile", Tone::Cyan),
        };
        heading(&mut out, &format!("{title} · config key {key}"), tone);
        let source = profile_source(
            reference.as_deref(),
            info.plugin_root.as_deref(),
            &info.user_profiles_dir,
        );
        let name = summary
            .and_then(|summary| summary.name.clone())
            .unwrap_or_else(|| {
                reference
                    .as_deref()
                    .map_or_else(|| "-".to_string(), profile_name)
            });
        field(
            &mut out,
            "Profile",
            format!("{name} · {}", source.text),
            bold(Style::new()),
        );
        field(
            &mut out,
            "File",
            source.file.as_deref().map_or_else(
                || {
                    format!(
                        "none on disk; nono profile show {}",
                        reference.as_deref().unwrap_or("<name>")
                    )
                },
                |file| short_path(file, home),
            ),
            Style::new(),
        );
        let Some(summary) = summary else {
            field(
                &mut out,
                "Policy",
                format!(
                    "nono could not resolve {}; run doctor",
                    reference.as_deref().unwrap_or("the profile")
                ),
                color_of(Tone::Red),
            );
            continue;
        };
        field(
            &mut out,
            "Extends",
            if summary.extends.is_empty() {
                "nothing".to_string()
            } else {
                summary.extends.join(", ")
            },
            Style::new(),
        );
        field(
            &mut out,
            "Network",
            network_detail(summary, side),
            if summary.loopback {
                color_of(Tone::Red)
            } else {
                Style::new()
            },
        );
        field(
            &mut out,
            "Files",
            format!(
                "{} read-write, {} read-only director{}, plus the workspace root (read-write)",
                summary.read_write_paths.len(),
                summary.read_only_paths.len(),
                if summary.read_only_paths.len() == 1 {
                    "y"
                } else {
                    "ies"
                }
            ),
            Style::new(),
        );
        let mediated = summary.af_unix_mediation == "pathname";
        field(
            &mut out,
            "Sockets",
            if mediated {
                "AF_UNIX pathname mediation: Herdr, D-Bus, SSH and GPG agents blocked".to_string()
            } else {
                format!(
                    "AF_UNIX mediation {}: host sockets reachable",
                    summary.af_unix_mediation
                )
            },
            if mediated {
                Style::new()
            } else {
                color_of(Tone::Red)
            },
        );
        if let Some(description) = &summary.description {
            field(&mut out, "About", description.clone(), color_of(Tone::Gray));
        }
    }
    heading(&mut out, "where to change them", Tone::Yellow);
    if let (Some(row), Some(configured)) = (row, configured) {
        let same = configured.client.reference == entry_text(&row.entry, "profile")
            && configured.server.reference == entry_text(&row.entry, "serverProfile");
        let mut names: Vec<String> = Vec::new();
        if let Some(server) = &configured.server.reference {
            names.push(format!("server {}", profile_name(server)));
        }
        names.push(format!(
            "client {}",
            profile_name(configured.client.reference.as_deref().unwrap_or("null"))
        ));
        let names = names.join(" · ");
        field(
            &mut out,
            "Next launch",
            if same {
                format!("same profiles ({names})")
            } else {
                format!("{names}; prefix+shift+b after quitting switches")
            },
            if same {
                Style::new()
            } else {
                color_of(Tone::Yellow)
            },
        );
    }
    field(
        &mut out,
        "Config",
        format!(
            "{}, keys \"serverProfile\" and \"profile\"",
            info.config_file
                .as_deref()
                .map_or_else(|| "config.json".to_string(), |file| short_path(file, home))
        ),
        Style::new(),
    );
    field(
        &mut out,
        "nono",
        format!(
            "user profiles in {} · nono profile list · nono profile show <name|path>",
            short_path(&info.user_profiles_dir.to_string_lossy(), home)
        ),
        Style::new(),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::super::model::{Networks, Trees};
    use super::super::render_lines;
    use super::*;
    use crate::nono::{summarize_profile, ProfileSummary};
    use crate::state::Entry;
    use serde_json::json;

    const NAME: &str = "herdr-opencode-abc123def456";

    fn ctx() -> DrawContext {
        DrawContext {
            clock: "00:00:00".into(),
            interval_secs: 3,
            home: "/home/u".into(),
        }
    }

    fn proc(pid: u32, depth: usize, confined: bool, argv: &[&str]) -> TreeProcess {
        TreeProcess {
            pid,
            depth,
            confined,
            argv: argv.iter().map(|word| word.to_string()).collect(),
        }
    }

    fn summary(egress: &str, domains: &[&str]) -> ProfileSummary {
        let mut summary = summarize_profile(&json!({}));
        summary.egress = egress.into();
        summary.allow_domains = domains.iter().map(|domain| domain.to_string()).collect();
        summary
    }

    fn sample_rows() -> Vec<Row> {
        let entry: Entry = json!({
            "workspaceId": "w1", "port": 4242, "profile": "/p/profiles/herdr-opencode-client.json", "serverProfile": "/p/profiles/herdr-opencode-server.json",
            "verification": {"ok": false, "checkedAt": "2026-10-03T07:00:00Z", "problems": ["Process 7 is not confined"]},
            "launchCount": 2, "lastLaunchAt": "2026-10-03T06:59:00Z", "lastExitCode": null,
        })
        .as_object()
        .cloned()
        .unwrap();
        vec![
            Row {
                pane_id: "w1:p1".into(),
                pane_exists: Some(true),
                session_name: NAME.into(),
                agent_kind: "opencode".into(),
                lifecycle_state: "running".into(),
                running: Some(true),
                server_running: Some(true),
                shells: Some(1),
                verified: Some(false),
                verification: Some("FAILED: Process 7 is not confined".into()),
                local_path: "/home/u/projects/app".into(),
                trees: Trees {
                    client: vec![proc(
                        101,
                        0,
                        true,
                        &[
                            "/home/u/.opencode/bin/opencode",
                            "--server",
                            "http://127.0.0.1:4242",
                        ],
                    )],
                    server: vec![
                        proc(
                            111,
                            0,
                            true,
                            &[
                                "/home/u/.opencode/bin/opencode",
                                "serve",
                                "--hostname",
                                "127.0.0.1",
                                "--port",
                                "4242",
                            ],
                        ),
                        proc(112, 1, true, &["/usr/bin/bash", "-c", "npm test"]),
                        proc(
                            113,
                            2,
                            false,
                            &[
                                "/usr/bin/node",
                                "/home/u/projects/app/node_modules/.bin/vitest",
                                "run",
                            ],
                        ),
                    ],
                },
                network: Networks {
                    client: Some(summary("blocked", &[])),
                    server: Some(summary("allowlist", &["*"])),
                },
                entry,
            },
            Row {
                pane_id: "w1:p2".into(),
                pane_exists: Some(false),
                session_name: "herdr-opencode-000000000002".into(),
                agent_kind: "opencode".into(),
                lifecycle_state: "exited".into(),
                running: Some(false),
                server_running: Some(false),
                shells: Some(0),
                verified: Some(true),
                verification: Some("confined: 2 processes".into()),
                local_path: "/tmp/x".into(),
                trees: Trees::default(),
                network: Networks::default(),
                entry: Entry::new(),
            },
        ]
    }

    /// The key hint line: the last line that carries the quit key.
    fn hints(lines: &[String]) -> &String {
        lines
            .iter()
            .rev()
            .find(|line| line.contains("q  quit") || line.contains("y  confirm"))
            .expect("the key hints are on screen")
    }

    fn view(rows: Vec<Row>) -> ViewState {
        let mut view = ViewState::new(ProfilesInfo::default());
        view.rows = rows;
        view
    }

    fn screen(view: &ViewState, width: u16, height: u16) -> (Vec<String>, String) {
        let lines = render_lines(view, &ctx(), width, height);
        let text = lines.join("\n");
        (lines, text)
    }

    #[test]
    fn it_draws_the_agents_table_the_two_sandboxes_with_their_processes_and_the_details() {
        let mut state = view(sample_rows());
        state.status = Some(Status::new(StatusKind::Ok, "pruned 1 mapping"));
        let (lines, text) = screen(&state, 110, 32);
        assert_eq!(lines.len(), 32);
        assert!(
            lines.iter().all(|line| line.chars().count() <= 110),
            "nothing is wider than the terminal"
        );
        assert!(
            lines[0].starts_with(" nono sandboxes ")
                && lines[0].ends_with("2 agents · 1 running · 1 FAILED · 1 stale · 00:00:00"),
            "{}",
            lines[0]
        );
        assert!(text.contains("┌─ Agents ─"));
        let has = |needle: &str| assert!(text.contains(needle), "missing {needle:?} in:\n{text}");
        has("│ ▶●  w1:p1      …abc123def456  opencode  running  client+server+1 sh  ✖ FAILED  ~/projects/app");
        has("│  ○  w1:p2 ✗    …000000000002  opencode  exited   -                   ✔ ok      /tmp/x");
        has(" client ──:4242──▶ server ──▶ nono proxy ──▶ internet  ✖ localhost ✖ Herdr ✖ systemd ✖ ssh-agent");
        has("┌─ client · no network but :4242 ");
        has("┌─ server · on :4242, proxy egress + LOCALHOST");
        has("│ ✔     101 tui    opencode --server http://127");
        has("✔     111 server opencode serve --hostname 1");
        has("✔     112 tool   └ bash -c npm test");
        has("✖     113 tool     └ vitest run");
        has("┌─ Details ─");
        has("Session     herdr-opencode-abc123def456");
        has("Profiles    client herdr-opencode-client · server herdr-opencode-server");
        has("Verified    FAILED: Process 7 is not confined (");
        has("Problem     Process 7 is not confined");
        has("✔ pruned 1 mapping");
        assert!(hints(&lines).contains("↑↓/jk  select   ⏎  jump   v  verify   x  stop   p  prune   a  clean all   i  profiles   ?  help   q  quit"), "{}", hints(&lines));
    }

    #[test]
    fn the_selection_follows_the_index_and_a_prompt_replaces_the_status() {
        let mut state = view(sample_rows());
        state.selected = 1;
        state.prompt = Some("Forget 1 mapping? [y/N]".into());
        let (lines, text) = screen(&state, 110, 32);
        assert!(text.contains("? Forget 1 mapping? [y/N]"));
        assert!(
            hints(&lines).contains("y  confirm") && hints(&lines).contains("n/esc  cancel"),
            "{}",
            hints(&lines)
        );
        assert!(text.contains("▶○  w1:p2"), "{text}");
        assert!(
            text.contains("sandbox · network unknown"),
            "an idle agent's sandbox says so"
        );
        assert!(text.contains("not running"));
    }

    #[test]
    fn open_egress_and_the_selected_row_band_are_shown() {
        let mut rows = sample_rows();
        rows[0].network.server = Some(summary("open", &[]));
        let (_, text) = screen(&view(rows), 110, 32);
        assert!(
            text.contains("server · on :4242, OPEN egress + localhost"),
            "{text}"
        );
        assert!(text.contains("! localhost open"), "{text}");
    }

    #[test]
    fn colour_marks_the_server_side_and_the_selection_is_one_inverse_band() {
        let state = view(sample_rows());
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(110, 32)).unwrap();
        terminal.draw(|frame| draw(frame, &state, &ctx())).unwrap();
        let buffer = terminal.backend().buffer();
        let magenta = (0..32)
            .flat_map(|y| (0..110).map(move |x| (x, y)))
            .any(|(x, y)| buffer[(x, y)].fg == Color::Magenta);
        assert!(magenta, "the server side is magenta");
        // Row 3 is the selected agent: every cell of the table band is reversed.
        let reversed = (2..108).all(|x| buffer[(x, 3)].modifier.contains(Modifier::REVERSED));
        assert!(reversed, "the selected row is one inverse band");
        assert!(
            !buffer[(2, 4)].modifier.contains(Modifier::REVERSED),
            "the other row is not"
        );
    }

    #[test]
    fn on_a_short_screen_the_sandboxes_stay_and_the_details_go() {
        let (_, text) = screen(&view(sample_rows()), 110, 20);
        assert!(text.contains("┌─ server"), "{text}");
        assert!(!text.contains("┌─ Details"), "{text}");
    }

    #[test]
    fn it_says_what_to_do_when_nothing_is_mapped_or_nono_fails() {
        let mut state = view(Vec::new());
        state.session_error = Some("nono ps failed (exit 1)".into());
        let (_, text) = screen(&state, 80, 24);
        assert!(
            text.contains("No sandboxed agents. Press ctrl+b, shift+a"),
            "{text}"
        );
        assert!(
            text.contains("nono ps failed: nono ps failed (exit 1)"),
            "{text}"
        );
        let (lines, _) = screen(&view(Vec::new()), 110, 32);
        assert!(lines[0].contains("0 agents · 0 running"), "{}", lines[0]);
        assert!(lines[0].contains("0 agents"), "plural: {}", lines[0]);
        let (single, _) = screen(&view(vec![sample_rows().remove(1)]), 110, 32);
        assert!(single[0].contains("1 agent ·"), "{}", single[0]);
    }

    #[test]
    fn the_key_hints_fit_a_narrow_screen_and_keep_the_profiles_key() {
        let (lines, _) = screen(&view(sample_rows()), 70, 30);
        let last = hints(&lines);
        assert!(last.chars().count() <= 70, "{last}");
        assert!(last.contains("i  profiles"), "{last}");
        assert!(
            !last.contains("refresh") && !last.contains("prune"),
            "refresh and prune hints go first: {last}"
        );
        assert!(
            last.contains("q  quit") && last.contains("?  help"),
            "{last}"
        );
    }

    #[test]
    fn the_help_screen_lists_every_key_and_the_herdr_chords() {
        let mut state = view(sample_rows());
        state.help = true;
        let (lines, text) = screen(&state, 110, 32);
        for wanted in [
            "┌─ Keys",
            "enter  g",
            "clean up all",
            "prefix+shift+o",
            "sandboxes: this overlay",
        ] {
            assert!(text.contains(wanted), "{wanted}: {text}");
        }
        assert!(hints(&lines).contains("?/esc  back"), "{}", hints(&lines));
    }

    fn profile_summary(name: &str, egress: &str) -> ProfileSummary {
        let mut summary = summarize_profile(
            &json!({"name": name, "description": format!("{name} policy"), "extends": ["nolabs-ai/opencode"], "linux": {"af_unix_mediation": "pathname"},
            "filesystem": {"allow": ["a", "b"], "read": ["c"]}}),
        );
        summary.egress = egress.into();
        if egress == "allowlist" {
            summary.allow_domains = vec!["api.githubcopilot.com".into(), "github.com".into()];
        }
        summary
    }

    fn profiles_view(rows: Vec<Row>) -> ViewState {
        use super::super::model::{Configured, ConfiguredProfile};
        let mut state = view(rows);
        state.mode = Mode::Profiles;
        state.profiles = ProfilesInfo {
            plugin_root: Some("/p".into()),
            config_file: Some("/home/u/.config/herdr/plugins/nono.sandbox/config.json".into()),
            user_profiles_dir: "/home/u/.config/nono/profiles".into(),
        };
        state.configured = Some(Configured {
            client: ConfiguredProfile {
                reference: Some("/p/profiles/herdr-opencode-client.json".into()),
                summary: Some(profile_summary("herdr-opencode-client", "blocked")),
            },
            server: ConfiguredProfile {
                reference: Some("/home/u/mine.json".into()),
                summary: Some(profile_summary("herdr-opencode-server", "allowlist")),
            },
        });
        state
    }

    #[test]
    fn the_profiles_view_says_which_profile_each_sandbox_runs_where_it_lives_and_what_it_allows() {
        let mut rows = sample_rows();
        rows[0].network = Networks {
            client: Some(profile_summary("herdr-opencode-client", "blocked")),
            server: Some(profile_summary("herdr-opencode-server", "allowlist")),
        };
        let (lines, text) = screen(&profiles_view(rows), 110, 40);
        assert_eq!(lines.len(), 40);
        assert!(lines.iter().all(|line| line.chars().count() <= 110));
        let has = |needle: &str| assert!(text.contains(needle), "missing {needle:?} in:\n{text}");
        has("┌─ Profiles · agent w1:p1 ─");
        assert!(
            !text.contains("┌─ Details"),
            "the profiles view replaces the sandboxes and details"
        );
        has("server sandbox: opencode serve and every tool call · config key serverProfile");
        has("Profile     herdr-opencode-server · shipped with the plugin");
        has("File        /p/profiles/herdr-opencode-server.json");
        has("Network     nono proxy to 2 hosts: api.githubcopilot.com, github.com");
        has("Network     blocked; the plugin opens its server's port only");
        has("Files       2 read-write, 1 read-only directory, plus the workspace root");
        has("Sockets     AF_UNIX pathname mediation");
        has("About       herdr-opencode-server policy");
        has("Next launch server mine · client herdr-opencode-client; prefix+shift+b after quitting switches");
        has("Config      ~/.config/herdr/plugins/nono.sandbox/config.json");
        has("user profiles in ~/.config/nono/profiles");
        assert!(hints(&lines).contains("i  sandboxes"), "{}", hints(&lines));
    }

    #[test]
    fn the_profiles_view_without_an_agent_describes_the_next_launch() {
        let (_, text) = screen(&profiles_view(Vec::new()), 80, 40);
        assert!(text.contains("Profiles · next launch"), "{text}");
        assert!(
            text.contains("Profile     herdr-opencode-server · your profile file"),
            "{text}"
        );
        let mut unknown = view(Vec::new());
        unknown.mode = Mode::Profiles;
        let (_, text) = screen(&unknown, 80, 30);
        assert!(
            text.contains(
                "No agent selected and the configured profiles are unknown (see config.json)."
            ),
            "{text}"
        );
    }

    #[test]
    fn an_unresolvable_profile_is_called_out_and_a_long_view_is_cut_with_a_note() {
        let mut rows = sample_rows();
        rows[0].network = Networks {
            client: None,
            server: None,
        };
        let (_, text) = screen(&profiles_view(rows.clone()), 110, 40);
        assert!(text.contains("Policy      nono could not resolve /p/profiles/herdr-opencode-server.json; run doctor"), "{text}");
        let (_, short) = screen(&profiles_view(rows), 110, 20);
        assert!(short.contains("more lines; enlarge the pane"), "{short}");
    }

    #[test]
    fn a_scrolled_table_shows_the_selected_row() {
        let many: Vec<Row> = (1..=9)
            .map(|index| {
                let mut row = sample_rows().remove(1);
                row.pane_id = format!("w1:p{index}");
                row.pane_exists = Some(true);
                row.session_name = format!("herdr-opencode-00000000000{index}");
                row
            })
            .collect();
        let mut state = view(many);
        state.selected = 8;
        let (_, text) = screen(&state, 110, 40);
        assert!(text.contains("▶○  w1:p9"), "{text}");
        assert!(
            !text.contains("w1:p1  "),
            "the first rows scrolled away: {text}"
        );
        state.selected = 0;
        let (_, text) = screen(&state, 110, 40);
        assert!(text.contains("▶○  w1:p1"), "{text}");
        assert!(!text.contains("w1:p7"), "only six rows are shown: {text}");
    }

    #[test]
    fn many_processes_are_clipped_with_a_count() {
        let mut rows = sample_rows();
        rows[0].trees.server = (0..20)
            .map(|index| {
                proc(
                    200 + index,
                    usize::from(index > 0),
                    true,
                    &["/bin/sleep", "1"],
                )
            })
            .collect();
        let (_, text) = screen(&view(rows), 110, 32);
        assert!(text.contains("more"), "{text}");
    }

    #[test]
    fn applying_a_frame_keeps_the_selected_pane_and_clamps_otherwise() {
        let mut state = view(sample_rows());
        state.selected = 1;
        state.apply(Collected {
            rows: sample_rows().into_iter().rev().collect(),
            ..Default::default()
        });
        assert_eq!(state.selected_row().unwrap().pane_id, "w1:p2");
        state.apply(Collected {
            rows: vec![sample_rows().remove(0)],
            ..Default::default()
        });
        assert_eq!(state.selected, 0);
        state.apply(Collected::default());
        assert_eq!(state.selected, 0);
        assert!(state.selected_row().is_none());
    }

    #[test]
    fn on_a_tall_screen_the_content_fills_the_height_and_the_hints_stay_on_the_last_row() {
        let (lines, _) = screen(&view(sample_rows()), 110, 60);
        assert!(
            lines.last().unwrap().contains("q  quit"),
            "{}",
            lines.join("\n")
        );
        let details = lines
            .iter()
            .position(|line| line.contains("┌─ Details"))
            .expect("details");
        assert!(
            details > 40,
            "the sandbox boxes grew to fill the screen; details start at line {details}"
        );
        assert!(
            lines[58].contains("refreshes every"),
            "the status line is just above the hints: {}",
            lines[58]
        );
        let blank_run = lines
            .windows(3)
            .filter(|w| w.iter().all(|line| line.is_empty()))
            .count();
        assert_eq!(
            blank_run,
            0,
            "no stretch of blank rows:\n{}",
            lines.join("\n")
        );
    }
}

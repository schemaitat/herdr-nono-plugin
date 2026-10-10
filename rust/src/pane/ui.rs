//! The overlay's screen, drawn with ratatui: a title bar, the key hints and a
//! status line on top, then one box per agent (green running, yellow stopped,
//! red stale). Details, profiles and help are popups. Pure: everything it
//! shows comes from the [`ViewState`].

use std::path::PathBuf;

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph};
use ratatui::Frame;
use serde_json::Value;

use crate::nono::ProfileSummary;
use super::model::{
    fit, health, network_detail, network_label, profile_name, profile_source, session_cell,
    short_command, short_path, worktree_name, Collected, Configured, Health, Row, Tone,
    TreeProcess,
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
    /// The selected agent's details popup is open.
    pub detail: bool,
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
            detail: false,
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
        Tone::Gray => Style::new().fg(Color::DarkGray),
        Tone::Green => Style::new().fg(Color::Green),
        Tone::Yellow => Style::new().fg(Color::Yellow),
        Tone::Red => Style::new().fg(Color::Red),
        Tone::Cyan => Style::new().fg(Color::Cyan),
        Tone::Magenta => Style::new().fg(Color::Magenta),
        Tone::Blue => Style::new().fg(Color::Blue),
    }
}

fn state_color(state: Health) -> Color {
    match state {
        Health::Running => Color::Green,
        Health::Stopped => Color::Yellow,
        Health::Stale => Color::Red,
        Health::Unknown => Color::DarkGray,
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

/// Lines of the title bar, the key hints and the status line above the list.
const HEADER_LINES: u16 = 3;

/// Height of one agent's box: two lines of text and the border.
const BOX_HEIGHT: u16 = 4;

/// A band of `height` lines at row `top` of `area`, clipped to it.
fn band(area: Rect, top: u16, height: u16) -> Rect {
    let top = top.min(area.height);
    Rect {
        x: area.x,
        y: area.y + top,
        width: area.width,
        height: height.min(area.height - top),
    }
}

/// Draws the whole screen into `frame`: the title bar, the key hints and the
/// status line on top (so they stay visible however tall the pane is), then
/// one box per agent; help, profiles and an agent's details are popups.
pub fn draw(frame: &mut Frame, view: &ViewState, ctx: &DrawContext) {
    let area = frame.area();
    let width = area.width as usize;
    let rows = &view.rows;
    let selected = if rows.is_empty() {
        None
    } else {
        Some(view.selected.min(rows.len() - 1))
    };
    let row = selected.map(|index| &rows[index]);

    // Title bar.
    let count = |health: Health| rows.iter().filter(|item| self::health(item) == health).count();
    let failed = rows
        .iter()
        .filter(|item| item.verified == Some(false))
        .count();
    let mut right = format!(
        "{} agent{} · {} running",
        rows.len(),
        if rows.len() == 1 { "" } else { "s" },
        count(Health::Running)
    );
    for (n, word) in [(count(Health::Stopped), "stopped"), (count(Health::Stale), "stale")] {
        if n > 0 {
            right.push_str(&format!(" · {n} {word}"));
        }
    }
    if failed > 0 {
        right.push_str(&format!(" · {failed} FAILED"));
    }
    right.push_str(&format!(" · {} ", ctx.clock));
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
        band(area, 0, 1),
    );
    frame.render_widget(Paragraph::new(key_hints(view, width)), band(area, 1, 1));
    frame.render_widget(Paragraph::new(status_line(view, ctx, width)), band(area, 2, 1));

    // The body: the boxes, with an `nono ps` failure on the last line.
    let mut body = band(area, HEADER_LINES, area.height);
    if let Some(error) = &view.session_error {
        if body.height > 1 {
            body.height -= 1;
            let line = Rect {
                y: body.y + body.height,
                height: 1,
                ..body
            };
            frame.render_widget(
                Paragraph::new(fit(&format!(" nono ps failed: {error}"), width, false))
                    .style(color_of(Tone::Red)),
                line,
            );
        }
    }
    draw_boxes(frame, body, view, selected, ctx);

    let popup = body;
    if view.help || view.mode == Mode::Profiles || (view.detail && row.is_some()) {
        frame.render_widget(Clear, popup);
    }
    if view.help {
        let mut lines = help_lines();
        lines.truncate(popup.height.saturating_sub(2) as usize);
        let shown = Rect {
            height: (lines.len() as u16 + 2).min(popup.height),
            ..popup
        };
        frame.render_widget(Clear, shown);
        frame.render_widget(
            Paragraph::new(lines).block(framed(titled("Keys", Style::new()), Tone::Yellow)),
            shown,
        );
    } else if view.mode == Mode::Profiles {
        draw_profiles(frame, popup, view, row, &ctx.home);
    } else if view.detail {
        if let Some(row) = row {
            draw_detail(frame, popup, row, &ctx.home);
        }
    }
}

/// The line under the key hints: the question being asked, the last result, or
/// the refresh note.
fn status_line(view: &ViewState, ctx: &DrawContext, width: usize) -> Line<'static> {
    if let Some(prompt) = &view.prompt {
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
        key("enter", "details: the client and server sandboxes"),
        key("g", "jump to the agent's pane and close the overlay"),
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
        key("esc", "close the popup"),
        key("q  ctrl+c", "close the overlay"),
        Line::default(),
        heading("Box colours"),
        Line::from(vec![
            span(" green        ".to_string(), color_of(Tone::Green)),
            span("a sandbox of the agent is running".to_string(), Style::new()),
        ]),
        Line::from(vec![
            span(" yellow       ".to_string(), color_of(Tone::Yellow)),
            span("sandboxes stopped, the pane still exists".to_string(), Style::new()),
        ]),
        Line::from(vec![
            span(" red          ".to_string(), color_of(Tone::Red)),
            span("stale: sandboxes down and the pane is gone (p forgets)".to_string(), Style::new()),
        ]),
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
    } else if view.detail {
        vec![
            ("esc/⏎", "close"),
            ("↑↓/jk", "select"),
            ("g", "jump"),
            ("v", "verify"),
            ("x", "stop"),
            ("?", "help"),
            ("q", "quit"),
        ]
    } else {
        vec![
            ("↑↓/jk", "select"),
            ("⏎", "details"),
            ("g", "jump"),
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
    for drop in ["r", "a", "p", "v", "x", "g"] {
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

/// One bordered box per agent, coloured by [`Health`]; the selected one has a
/// heavy border. The box titles carry the worktree name, the label the Herdr
/// sidebar shows, so a box can be matched to its worktree at a glance.
fn draw_boxes(
    frame: &mut Frame,
    area: Rect,
    view: &ViewState,
    selected: Option<usize>,
    ctx: &DrawContext,
) {
    let rows = &view.rows;
    if rows.is_empty() {
        let block = framed(titled("Agents", Style::new()), Tone::Gray);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        frame.render_widget(
            Paragraph::new(fit(
                "No sandboxed agents. Press ctrl+b, shift+a in a project pane to start one.",
                inner.width as usize,
                false,
            )),
            inner,
        );
        return;
    }
    let selected = selected.unwrap_or(0);
    let visible = (area.height / BOX_HEIGHT).max(1) as usize;
    let first = selected
        .saturating_sub(visible - 1)
        .min(rows.len().saturating_sub(visible));
    for (slot, index) in (first..rows.len().min(first + visible)).enumerate() {
        let top = slot as u16 * BOX_HEIGHT;
        let cell = band(area, top, BOX_HEIGHT);
        if cell.height < 3 {
            break;
        }
        draw_box(frame, cell, &rows[index], index == selected, ctx);
    }
    if first > 0 || first + visible < rows.len() {
        // A scroll note at the right end of the status line above the list.
        let note = format!(" {}/{} ", selected + 1, rows.len());
        let width = note.chars().count() as u16;
        if area.width > width + 2 && area.y > 0 {
            let corner = Rect {
                x: area.x + area.width - width - 1,
                y: area.y - 1,
                width,
                height: 1,
            };
            frame.render_widget(Paragraph::new(span(note, color_of(Tone::Gray))), corner);
        }
    }
}

fn draw_box(frame: &mut Frame, area: Rect, item: &Row, is_selected: bool, ctx: &DrawContext) {
    let state = health(item);
    let tone = color_of(state.tone());
    let (name, repo) = worktree_name(&item.local_path);
    // The selected box: a filled title label, a tinted background, brighter text.
    let label = if is_selected {
        Style::new()
            .fg(Color::Black)
            .bg(state_color(state))
            .add_modifier(Modifier::BOLD)
    } else {
        bold(tone)
    };
    let mut title = vec![
        span(if is_selected { " ▶ " } else { "● " }, if is_selected { label } else { tone }),
        span(name, label),
    ];
    if is_selected {
        title.push(span(" ", label));
    }
    if let Some(repo) = repo {
        title.push(span(
            format!("{}· {repo}", if is_selected { "" } else { " " }),
            color_of(Tone::Gray),
        ));
    }
    title.push(span(" ", Style::new()));
    let border = if is_selected {
        BorderType::Thick
    } else {
        BorderType::Rounded
    };
    let mut block = Block::new()
        .borders(Borders::ALL)
        .border_type(border)
        .border_style(if is_selected { bold(tone) } else { tone })
        .title(Line::from(title))
        .title(
            Line::from(vec![span(format!(" {} ", state.word()), bold(tone))]).right_aligned(),
        )
        .padding(Padding::horizontal(1));
    if is_selected {
        block = block.style(Style::new().bg(Color::Indexed(236)));
    }
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let inner_width = inner.width as usize;

    let pane = if item.pane_exists == Some(false) {
        format!("{} ✗ gone", item.pane_id)
    } else {
        item.pane_id.clone()
    };
    let (nono_text, nono_tone) = session_cell(item);
    let (verified_text, verified_style) = match item.verified {
        None => ("not verified".to_string(), color_of(Tone::Gray)),
        Some(true) => ("✔ verified".to_string(), color_of(Tone::Green)),
        Some(false) => ("✖ FAILED".to_string(), bold(color_of(Tone::Red))),
    };
    let short_session = format!("…{}", item.session_name.rsplit('-').next().unwrap_or(""));
    let dim = if is_selected {
        Style::new().fg(Color::White)
    } else {
        color_of(Tone::Gray)
    };
    let sep = || span("  ·  ", dim);
    let mut first_line = vec![
        span(item.agent_kind.clone(), Style::new()),
        sep(),
        span(
            pane,
            if item.pane_exists == Some(false) {
                color_of(Tone::Gray)
            } else {
                Style::new()
            },
        ),
        sep(),
        span(nono_text, color_of(nono_tone)),
        sep(),
        span(verified_text, verified_style),
    ];
    if item.lifecycle_state == "failed" {
        first_line.push(sep());
        first_line.push(span("launch failed", color_of(Tone::Red)));
    }
    first_line.push(sep());
    first_line.push(span(short_session, dim));
    let directory = fit(&short_path(&item.local_path, &ctx.home), inner_width, true);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(first_line),
            Line::from(span(directory.trim_end().to_string(), dim)),
        ]),
        inner,
    );
}

/// The selected agent's two sandboxes with their processes, the data path
/// between them, and the details, in a popup over the list.
fn draw_detail(frame: &mut Frame, area: Rect, row: &Row, home: &str) {
    let state = health(row);
    let (name, repo) = worktree_name(&row.local_path);
    let title = format!(
        "{name}{} · {}",
        repo.map_or_else(String::new, |repo| format!(" · {repo}")),
        state.word()
    );
    let block = framed(titled(&title, color_of(state.tone())), state.tone());
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    let top = std::cell::Cell::new(0u16);
    let mut take = |lines: usize| -> Rect {
        let start = top.get().min(inner.height);
        let taken = (lines as u16).min(inner.height - start);
        top.set(start + taken);
        band(inner, start, taken)
    };
    let width = inner.width as usize;
    let mut detail_height = 9usize;
    let mut sandbox_height = inner.height as i64 - 1 - detail_height as i64;
    if sandbox_height < 5 {
        detail_height = 0;
        sandbox_height = inner.height as i64 - 1;
    }
    draw_flow(frame, take(1), row, width);
    draw_sandboxes(frame, &mut take, row, width, sandbox_height, home);
    if detail_height > 0 {
        let details_area = take(detail_height);
        draw_details(frame, details_area, row, home);
    }
}

/// The profiles view as a popup.
fn draw_profiles(frame: &mut Frame, area: Rect, view: &ViewState, row: Option<&Row>, home: &str) {
    let room = area.height.saturating_sub(2) as usize;
    let inner_width = area.width.saturating_sub(4) as usize;
    let body = profile_lines(view, row, inner_width, home);
    let shown: Vec<Line<'static>> = if body.len() <= room || room == 0 {
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
    let shown_area = Rect {
        height: (shown.len() as u16 + 2).min(area.height),
        ..area
    };
    frame.render_widget(Clear, shown_area);
    frame.render_widget(
        Paragraph::new(shown).block(framed(titled(&title_text, Style::new()), Tone::Yellow)),
        shown_area,
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
        .flat_map(|process| {
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
            let full = process.argv.join(" ");
            let mut lines = vec![Line::from(vec![
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
            ])];
            // The whole command line, as `ps` shows it, wrapped under the short form.
            let pad = prefix_len + 2;
            let room = inner_width.saturating_sub(pad).max(8);
            let chars: Vec<char> = full.chars().collect();
            let chunks: Vec<String> = chars
                .chunks(room)
                .map(|chunk| chunk.iter().collect())
                .collect();
            let shown = chunks.len().min(FULL_COMMAND_LINES);
            for (index, chunk) in chunks.iter().take(shown).enumerate() {
                let text = if index + 1 == shown && chunks.len() > shown {
                    fit(&format!("{chunk}…"), room, false)
                } else {
                    chunk.clone()
                };
                lines.push(Line::from(vec![
                    span(" ".repeat(pad - 2), Style::new()),
                    span(if index == 0 { "$ " } else { "  " }, color_of(Tone::Gray)),
                    span(text, color_of(Tone::Gray)),
                ]));
            }
            lines
        })
        .collect()
}

/// How many lines the whole command line of one process may take in a sandbox box.
const FULL_COMMAND_LINES: usize = 4;

/// A labelled list in a sandbox box: the label on the first line only, one
/// item per line, cut after `cap` items with a count.
fn listing(
    out: &mut Vec<Line<'static>>,
    label: &str,
    items: &[String],
    cap: usize,
    width: usize,
    paths: bool,
) {
    let value_width = width.saturating_sub(LABEL_WIDTH);
    for (index, item) in items.iter().take(cap).enumerate() {
        out.push(Line::from(vec![
            span(
                fit(if index == 0 { label } else { "" }, LABEL_WIDTH, false),
                color_of(Tone::Gray),
            ),
            span(fit(&format!("• {item}"), value_width, paths), Style::new()),
        ]));
    }
    if items.len() > cap {
        out.push(Line::from(vec![
            span(" ".repeat(LABEL_WIDTH.min(width)), Style::new()),
            span(
                fit(&format!("… {} more", items.len() - cap), value_width, false),
                color_of(Tone::Gray),
            ),
        ]));
    }
}

/// Width of the label column in a sandbox box.
const LABEL_WIDTH: usize = 11;

/// What a sandbox's profile allows: network and the hosts it may reach, the
/// directories it may write and read, and socket mediation.
fn policy_lines(
    summary: Option<&ProfileSummary>,
    client_side: bool,
    port: Option<u64>,
    width: usize,
    home: &str,
) -> Vec<Line<'static>> {
    let field = |label: &str, value: String, style: Style| -> Line<'static> {
        Line::from(vec![
            span(fit(label, LABEL_WIDTH, false), color_of(Tone::Gray)),
            span(fit(&value, width.saturating_sub(LABEL_WIDTH), false), style),
        ])
    };
    let Some(summary) = summary else {
        return vec![field(
            "profile",
            "nono could not resolve it; run doctor".into(),
            color_of(Tone::Red),
        )];
    };
    let mut out = vec![field(
        "profile",
        summary.name.clone().unwrap_or_else(|| "-".into()),
        bold(Style::new()),
    )];
    let hosts: Vec<String> = match summary.egress.as_str() {
        "blocked" => port
            .filter(|_| client_side)
            .map(|port| vec![format!("127.0.0.1:{port} (its server) only")])
            .unwrap_or_default(),
        "allowlist" if summary.allow_domains.iter().any(|domain| domain == "*") => {
            vec!["any host, through the nono proxy".to_string()]
        }
        "allowlist" => summary.allow_domains.clone(),
        _ => vec!["any host, direct connects".to_string()],
    };
    let (network, style) = match summary.egress.as_str() {
        "blocked" => ("blocked".to_string(), Style::new()),
        "allowlist" => (
            format!("nono proxy, {} allowed", summary.allow_domains.len()),
            Style::new(),
        ),
        _ => ("OPEN egress".to_string(), color_of(Tone::Red)),
    };
    out.push(field("network", network, style));
    listing(&mut out, "allowed", &hosts, 12, width, false);
    if summary.loopback {
        out.push(field("localhost", "REACHABLE".into(), color_of(Tone::Red)));
    }
    let mut writes = vec!["the workspace root".to_string()];
    writes.extend(summary.read_write_paths.iter().map(|path| short_path(path, home)));
    listing(&mut out, "writes", &writes, 6, width, true);
    let reads: Vec<String> = summary
        .read_only_paths
        .iter()
        .map(|path| short_path(path, home))
        .collect();
    listing(&mut out, "reads", &reads, 6, width, true);
    let mediated = summary.af_unix_mediation == "pathname";
    out.push(field(
        "sockets",
        if mediated {
            "Herdr, D-Bus, SSH and GPG agents blocked".to_string()
        } else {
            format!("AF_UNIX {}: host sockets reachable", summary.af_unix_mediation)
        },
        if mediated {
            Style::new()
        } else {
            color_of(Tone::Red)
        },
    ));
    out
}

/// The body of a sandbox box: its processes, then its policy.
fn side_lines(
    tree: &[TreeProcess],
    summary: Option<&ProfileSummary>,
    client_side: bool,
    port: Option<u64>,
    empty_text: &str,
    width: usize,
    home: &str,
) -> Vec<Line<'static>> {
    let heading = |text: &str| Line::from(span(text.to_string(), bold(Style::new())));
    let loose = tree.iter().filter(|process| !process.confined).count();
    let mut processes = vec![span("processes".to_string(), bold(Style::new()))];
    if !tree.is_empty() {
        processes.push(span(
            format!(
                "  {} · {}",
                tree.len(),
                if loose == 0 {
                    "all confined".to_string()
                } else {
                    format!("{loose} NOT confined")
                }
            ),
            color_of(if loose == 0 { Tone::Green } else { Tone::Red }),
        ));
    }
    let mut out = vec![Line::from(processes)];
    out.extend(process_lines(tree, width, client_side, empty_text));
    out.push(Line::default());
    out.push(heading("policy"));
    out.extend(policy_lines(summary, client_side, port, width, home));
    out
}

/// Draws the selected agent's client and server boxes side by side.
fn draw_sandboxes(
    frame: &mut Frame,
    take: &mut dyn FnMut(usize) -> Rect,
    row: &Row,
    width: usize,
    sandbox_height: i64,
    home: &str,
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
    let left_body = side_lines(
        &row.trees.client,
        row.network.client.as_ref(),
        true,
        port,
        left_empty,
        left_inner,
        home,
    );
    let right_body = if server {
        side_lines(
            &row.trees.server,
            row.network.server.as_ref(),
            false,
            port,
            right_empty,
            right_inner,
            home,
        )
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

    /// The key hint line: the second line of the screen.
    fn hints(lines: &[String]) -> &String {
        &lines[1]
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

    fn detail_view(rows: Vec<Row>) -> ViewState {
        let mut state = view(rows);
        state.detail = true;
        state
    }

    #[test]
    fn it_draws_one_box_per_agent_titled_by_the_worktree_with_the_hints_on_top() {
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
                && lines[0].ends_with("2 agents · 1 running · 1 stale · 1 FAILED · 00:00:00"),
            "{}",
            lines[0]
        );
        assert!(hints(&lines).contains("↑↓/jk  select   ⏎  details   g  jump   v  verify"), "{}", hints(&lines));
        assert!(hints(&lines).contains("q  quit"), "{}", hints(&lines));
        assert!(lines[2].contains("✔ pruned 1 mapping"), "{}", lines[2]);
        let has = |needle: &str| assert!(text.contains(needle), "missing {needle:?} in:\n{text}");
        has("┏ ▶ app");
        has(" running ");
        has("opencode  ·  w1:p1  ·  client+server+1 sh  ·  ✖ FAILED  ·  …abc123def456");
        has("~/projects/app");
        has("╭● x");
        has(" stale ");
        has("w1:p2 ✗ gone");
        assert!(!text.contains("┌─ Details"), "details are a popup: {text}");
    }

    #[test]
    fn a_worktree_box_shows_the_repository_above_it() {
        let mut rows = sample_rows();
        rows[0].local_path = "/home/u/.herdr/worktrees/herdr-nono-plugin/feat-tui".into();
        let (_, text) = screen(&view(rows), 110, 32);
        assert!(text.contains("▶ feat-tui · herdr-nono-plugin"), "{text}");
    }

    #[test]
    fn running_is_green_stopped_yellow_and_stale_red() {
        let mut rows = sample_rows();
        let mut stopped = rows[1].clone();
        stopped.pane_exists = Some(true);
        stopped.pane_id = "w1:p3".into();
        rows.push(stopped);
        let state = view(rows);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(110, 32)).unwrap();
        terminal.draw(|frame| draw(frame, &state, &ctx())).unwrap();
        let buffer = terminal.backend().buffer();
        // Box tops start at rows 3, 7 and 11; the corner is the first cell.
        assert_eq!(buffer[(0, 3)].fg, Color::Green);
        assert_eq!(buffer[(0, 7)].fg, Color::Red);
        assert_eq!(buffer[(0, 11)].fg, Color::Yellow);
        let (_, text) = screen(&state, 110, 32);
        assert!(text.contains(" stopped "), "{text}");
        assert!(
            screen(&state, 110, 32).0[0].contains("1 running · 1 stopped · 1 stale"),
            "{}",
            text
        );
    }

    #[test]
    fn the_hints_are_on_the_top_lines_even_when_the_pane_is_tall() {
        let (lines, _) = screen(&view(sample_rows()), 110, 60);
        assert!(hints(&lines).contains("q  quit"), "{}", lines[1]);
        assert!(lines[2].contains("refreshes every"), "{}", lines[2]);
    }

    #[test]
    fn a_prompt_replaces_the_status_and_the_selection_moves_the_heavy_border() {
        let mut state = view(sample_rows());
        state.selected = 1;
        state.prompt = Some("Forget 1 mapping? [y/N]".into());
        let (lines, text) = screen(&state, 110, 32);
        assert!(lines[2].contains("? Forget 1 mapping? [y/N]"), "{}", lines[2]);
        assert!(
            hints(&lines).contains("y  confirm") && hints(&lines).contains("n/esc  cancel"),
            "{}",
            hints(&lines)
        );
        assert!(text.contains("┏ ▶ x"), "{text}");
        assert!(text.contains("╭● app"), "{text}");
    }

    #[test]
    fn enter_opens_a_popup_with_both_sandboxes_their_processes_and_the_details() {
        let (lines, text) = screen(&detail_view(sample_rows()), 110, 40);
        assert!(
            lines.iter().all(|line| line.chars().count() <= 110),
            "nothing is wider than the terminal"
        );
        assert!(hints(&lines).contains("esc/⏎  close"), "{}", hints(&lines));
        let has = |needle: &str| assert!(text.contains(needle), "missing {needle:?} in:\n{text}");
        has("─ app · running ");
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
    }

    #[test]
    fn the_popup_lists_the_allowed_hosts_the_directories_and_the_sockets_of_each_sandbox() {
        let mut rows = sample_rows();
        rows[0].network = Networks {
            client: Some(profile_summary("herdr-opencode-client", "blocked")),
            server: Some(profile_summary("herdr-opencode-server", "allowlist")),
        };
        let (_, text) = screen(&detail_view(rows), 120, 44);
        for wanted in [
            "processes",
            "policy",
            "profile    herdr-opencode-client",
            "network    blocked",
            "allowed    • 127.0.0.1:4242 (its server) only",
            "network    nono proxy, 2 allowed",
            "allowed    • api.githubcopilot.com",
            "1 · all confined",
            "3 · 1 NOT confined",
            "• github.com",
            "writes     • the workspace root",
            "• a",
            "reads      • c",
            "sockets    Herdr, D-Bus, SSH and GPG agents blocked",
        ] {
            assert!(text.contains(wanted), "missing {wanted:?} in:\n{text}");
        }
    }

    #[test]
    fn every_process_shows_its_whole_command_line_wrapped_under_the_short_form() {
        let (_, text) = screen(&detail_view(sample_rows()), 240, 44);
        assert!(
            text.contains("$ /home/u/.opencode/bin/opencode --server http://127.0.0.1:4242"),
            "{text}"
        );
        assert!(
            text.contains("/usr/bin/node /home/u/projects/app/node_modules/.bin/vitest run"),
            "{text}"
        );
        let mut rows = sample_rows();
        rows[0].trees.client[0].argv = vec!["/bin/tool".into(), "x".repeat(400)];
        let (_, long) = screen(&detail_view(rows), 120, 44);
        assert!(long.contains("xxx…"), "a very long command is cut with …: {long}");
    }

    #[test]
    fn open_egress_is_called_out_in_the_popup() {
        let mut rows = sample_rows();
        rows[0].network.server = Some(summary("open", &[]));
        let (_, text) = screen(&detail_view(rows), 110, 40);
        assert!(
            text.contains("server · on :4242, OPEN egress + localhost"),
            "{text}"
        );
        assert!(text.contains("! localhost open"), "{text}");
    }

    #[test]
    fn the_popup_marks_the_server_side_magenta() {
        let state = detail_view(sample_rows());
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(110, 40)).unwrap();
        terminal.draw(|frame| draw(frame, &state, &ctx())).unwrap();
        let buffer = terminal.backend().buffer();
        let magenta = (0..40)
            .flat_map(|y| (0..110).map(move |x| (x, y)))
            .any(|(x, y)| buffer[(x, y)].fg == Color::Magenta);
        assert!(magenta, "the server side is magenta");
    }

    #[test]
    fn on_a_short_screen_the_popup_keeps_the_sandboxes_and_drops_the_details() {
        let (_, text) = screen(&detail_view(sample_rows()), 110, 17);
        assert!(text.contains("┌─ server"), "{text}");
        assert!(!text.contains("┌─ Details"), "{text}");
    }

    #[test]
    fn an_idle_agents_popup_says_so() {
        let mut state = detail_view(sample_rows());
        state.selected = 1;
        let (_, text) = screen(&state, 110, 40);
        assert!(text.contains("sandbox · network unknown"), "{text}");
        assert!(text.contains("not running"), "{text}");
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
    fn the_help_screen_lists_every_key_the_colours_and_the_herdr_chords() {
        let mut state = view(sample_rows());
        state.help = true;
        let (lines, text) = screen(&state, 110, 40);
        for wanted in [
            "┌─ Keys",
            "details: the client and server sandboxes",
            "jump to the agent's pane",
            "clean up all",
            "stale: sandboxes down and the pane is gone",
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
        let (_, short) = screen(&profiles_view(rows), 110, 16);
        assert!(short.contains("more lines; enlarge the pane"), "{short}");
    }

    #[test]
    fn a_long_list_scrolls_to_the_selected_box_and_counts() {
        let many: Vec<Row> = (1..=9)
            .map(|index| {
                let mut row = sample_rows().remove(1);
                row.pane_id = format!("w1:p{index}");
                row.pane_exists = Some(true);
                row.local_path = format!("/work/tree{index}");
                row.session_name = format!("herdr-opencode-00000000000{index}");
                row
            })
            .collect();
        let mut state = view(many);
        state.selected = 8;
        let (_, text) = screen(&state, 110, 24);
        assert!(text.contains("┏ ▶ tree9"), "{text}");
        assert!(!text.contains("tree1 "), "the first boxes scrolled away: {text}");
        assert!(text.contains(" 9/9"), "{text}");
        state.selected = 0;
        let (_, text) = screen(&state, 110, 24);
        assert!(text.contains("┏ ▶ tree1"), "{text}");
        assert!(!text.contains("tree7"), "only five boxes fit: {text}");
        assert!(text.contains(" 1/9"), "{text}");
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
        let (_, text) = screen(&detail_view(rows), 110, 32);
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

}

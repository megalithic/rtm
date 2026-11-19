//! Drawing. No stock widgets: the page is one tint, the preview a soft card with half-block
//! edges, the highlight a pill. Hierarchy comes from weight, dimness and spacing, not boxes.
use super::{
    motion,
    state::{App, Item, ItemKind, Mode, Note},
    theme::{self, AQUA, BLUE, CARD, FAINT, GREEN, PAGE, PURPLE, RAISED, RED, YELLOW},
};
use crate::{
    git::Colour,
    model::{Kind, Row},
};
use ratatui::{
    buffer::Buffer,
    layout::{Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};
use unicode_width::UnicodeWidthStr;

pub const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const SELECT_MS: u64 = 140;
const ENTER_MS: u64 = 180;
const PREVIEW_MIN_WIDTH: u16 = 100;
const KILL_TINT: Color = Color::Rgb(0x4a, 0x33, 0x36);

#[derive(Clone, Debug)]
pub enum Hit {
    Item(usize),
    Url(String),
}

pub struct Preview {
    pub key: String,
    pub text: Result<Text<'static>, String>,
    pub at: Instant,
}

/// What the screen remembers between frames: scroll position, transitions, click targets.
#[derive(Default)]
pub struct View {
    pub offset: usize,
    pub hits: Vec<(Rect, Hit)>,
    pub spinner: usize,
    pub has_spinner: bool,
    /// The previous highlight fading out while the new one fades in.
    pub selection: Option<(Option<usize>, Instant)>,
    pub entered: HashMap<String, Instant>,
    pub preview: Option<Preview>,
    /// When the highlight moved to a row whose preview is not here yet.
    pub stale_since: Option<Instant>,
    pub content: Rect,
}

impl View {
    pub fn animating(&self, now: Instant) -> bool {
        self.selection
            .is_some_and(|(_, at)| now < at + Duration::from_millis(SELECT_MS))
            || self
                .entered
                .values()
                .any(|at| now < *at + Duration::from_millis(ENTER_MS))
            || self
                .stale_since
                .is_some_and(|at| now < at + Duration::from_millis(SELECT_MS))
    }

    pub fn moved(&mut self, from: Option<usize>, now: Instant) {
        self.selection = Some((from, now));
    }
}

fn fill(buf: &mut Buffer, area: Rect, bg: Color) {
    let area = area.intersection(buf.area);
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let cell = &mut buf[(x, y)];
            cell.reset();
            cell.set_bg(bg);
        }
    }
}

fn set(buf: &mut Buffer, x: u16, y: u16, symbol: &str, fg: Color) {
    if let Some(cell) = buf.cell_mut((x, y)) {
        cell.set_symbol(symbol).set_fg(fg);
    }
}

/// Writes a line clipped to `max` columns; spans keep the background already there.
fn put(buf: &mut Buffer, x: u16, y: u16, line: &Line, max: u16) -> u16 {
    if !buf.area.contains(Position::new(x, y)) || max == 0 {
        return x;
    }
    let max = max.min(buf.area.right() - x);
    let end = buf.set_line(x, y, line, max).0;
    // Clipped text ends in an ellipsis rather than a cut word.
    if line.width() > usize::from(max)
        && let Some(cell) = buf.cell_mut((x + max - 1, y))
    {
        cell.set_symbol("…");
    }
    end
}

/// A soft card: half-block top and bottom edges and quadrant corners.
fn card(buf: &mut Buffer, r: Rect, color: Color) {
    if r.width < 2 || r.height < 2 {
        return;
    }
    fill(buf, Rect::new(r.x, r.y + 1, r.width, r.height - 2), color);
    let (top, bottom) = (r.top(), r.bottom() - 1);
    for x in r.left()..r.right() {
        let (left, right) = (x == r.left(), x == r.right() - 1);
        let (t, b) = match (left, right) {
            (true, _) => ("▗", "▝"),
            (_, true) => ("▖", "▘"),
            _ => ("▄", "▀"),
        };
        set(buf, x, top, t, color);
        set(buf, x, bottom, b, color);
    }
}

/// A one-row pill whose caps blend into the page.
fn pill(buf: &mut Buffer, r: Rect, color: Color) {
    if r.width < 3 {
        return;
    }
    for x in r.left() + 1..r.right() - 1 {
        if let Some(cell) = buf.cell_mut((x, r.y)) {
            cell.set_bg(color);
        }
    }
    set(buf, r.left(), r.y, "▐", color);
    set(buf, r.right() - 1, r.y, "▌", color);
}

pub fn age(now: u64, then: u64) -> String {
    if then == 0 {
        return String::new();
    }
    let seconds = now.saturating_sub(then) / 1000;
    match seconds {
        0..60 => "now".into(),
        60..3600 => format!("{}m", seconds / 60),
        3600..86_400 => format!("{}h", seconds / 3600),
        _ => format!("{}d", seconds / 86_400),
    }
}

/// The glyph, its colour, and a word only where it matters.
fn status(row: &Row, spinner: &'static str) -> (&'static str, Color, Option<&'static str>) {
    match row.state.as_str() {
        "blocked" => ("●", YELLOW, Some("needs you")),
        "working" => (spinner, AQUA, None),
        "done" if row.attention => ("●", GREEN, None),
        "unknown" if !row.metadata.errors.is_empty() => ("!", RED, Some("unreachable")),
        _ => ("·", FAINT, None),
    }
}

fn highlight(text: &str, hits: &[u32], base: Style) -> Vec<Span<'static>> {
    if hits.is_empty() {
        return vec![Span::styled(text.to_owned(), base)];
    }
    text.chars()
        .enumerate()
        .map(|(index, c)| {
            let style = if hits.contains(&(index as u32)) {
                base.fg(YELLOW).add_modifier(Modifier::BOLD)
            } else {
                base
            };
            Span::styled(c.to_string(), style)
        })
        .collect()
}

fn repo_of(row: &Row) -> Option<String> {
    let path = row.metadata.worktree.as_deref()?;
    let main = path.split("/.worktrees/").next()?;
    Some(main.rsplit('/').next()?.to_owned())
}

/// Name spans: a Rex worktree's repo prefix steps back so the distinct part reads first;
/// under a project header only the distinct part shows.
fn name_spans(row: &Row, hits: &[u32], selected: bool, short: Option<&str>) -> Vec<Span<'static>> {
    let strong = if selected {
        theme::text().add_modifier(Modifier::BOLD)
    } else {
        theme::text()
    };
    if let Some(short) = short {
        return vec![Span::styled(short.to_owned(), strong)];
    }
    let prefix = (row.kind == Kind::Rex)
        .then(|| repo_of(row))
        .flatten()
        .map(|repo| format!("{repo}-"))
        .filter(|prefix| row.name.starts_with(prefix.as_str()) && row.name.len() > prefix.len());
    match prefix {
        Some(prefix) if hits.is_empty() => vec![
            Span::styled(prefix.clone(), theme::faint()),
            Span::styled(row.name[prefix.len()..].to_owned(), strong),
        ],
        _ => highlight(&row.name, hits, strong),
    }
}

/// The port column's width (`:4002`), kept even when a row has none so PRs and ports align.
const PORT_WIDTH: usize = 5;

/// Quiet right-hand metadata in columns: branch (when not already in the name), PR, port, age.
fn meta(row: &Row, now: u64) -> (Vec<Span<'static>>, Option<(String, usize, usize)>) {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let gap = |spans: &mut Vec<Span<'static>>| {
        if !spans.is_empty() {
            spans.push(Span::raw("  "));
        }
    };
    if let Some(branch) = &row.metadata.branch
        && !row.name.ends_with(branch.as_str())
        && branch != "main"
        && branch != "master"
    {
        gap(&mut spans);
        spans.push(Span::styled(format!("⎇ {branch}"), theme::faint()));
    }
    let mut pr_at = None;
    if let Some(pr) = &row.metadata.pr {
        gap(&mut spans);
        let colour = match pr.colour {
            Colour::Green => GREEN,
            Colour::Blue => BLUE,
            Colour::Red => RED,
        };
        let label = format!("#{}", pr.number);
        let start: usize = spans.iter().map(|span| span.width()).sum();
        pr_at = Some((pr.url.clone(), start, label.width()));
        spans.push(Span::styled(label, Style::new().fg(colour)));
    }
    let ports: Vec<Span<'static>> = if row.kind == Kind::Ultimate {
        row.metadata
            .port
            .map(|port| Span::styled(format!(":{port}"), theme::faint()))
            .into_iter()
            .collect()
    } else {
        row.metadata
            .apps
            .iter()
            .map(|app| Span::styled(format!(":{}", app.port), theme::muted()))
            .collect()
    };
    if ports.is_empty() {
        if !spans.is_empty() {
            spans.push(Span::raw(" ".repeat(PORT_WIDTH + 2)));
        }
    } else {
        for port in ports {
            gap(&mut spans);
            spans.push(Span::styled(
                format!("{:>PORT_WIDTH$}", port.content),
                port.style,
            ));
        }
    }
    let when = age(now, row.last_activity);
    gap(&mut spans);
    spans.push(Span::styled(format!("{when:>3}"), theme::faint()));
    (spans, pr_at)
}

fn width(spans: &[Span]) -> u16 {
    spans.iter().map(|span| span.width() as u16).sum()
}

struct Frame<'a> {
    buf: &'a mut Buffer,
    app: &'a App,
    view: &'a mut View,
    now: Instant,
    clock: u64,
}

pub fn draw(buf: &mut Buffer, app: &App, view: &mut View, now: Instant, clock: u64) {
    let area = buf.area;
    fill(buf, area, PAGE);
    view.hits.clear();
    view.has_spinner = false;
    if area.width < 30 || area.height < 8 {
        put(
            buf,
            area.x + 1,
            area.y,
            &Line::styled("make this larger", theme::faint()),
            area.width,
        );
        return;
    }
    let inner = Rect::new(area.x + 2, area.y + 1, area.width - 4, area.height - 2);
    let mut frame = Frame {
        buf,
        app,
        view,
        now,
        clock,
    };
    let top = Rect::new(inner.x, inner.y, inner.width, 1);
    let footer = Rect::new(inner.x, inner.bottom() - 1, inner.width, 1);
    let body = Rect::new(
        inner.x,
        inner.y + 2,
        inner.width,
        inner.height.saturating_sub(4),
    );
    let (list, preview) = if inner.width >= PREVIEW_MIN_WIDTH {
        let list = (inner.width * 9 / 20).clamp(48, 76);
        (
            Rect::new(body.x, body.y, list, body.height),
            Some(Rect::new(
                body.x + list + 2,
                body.y.saturating_sub(1),
                body.width - list - 2,
                body.height + 1,
            )),
        )
    } else {
        (body, None)
    };
    frame.header(top);
    frame.list(list);
    if let Some(preview) = preview {
        frame.preview(preview);
    }
    frame.footer(footer);
}

impl Frame<'_> {
    fn spinner(&mut self) -> &'static str {
        self.view.has_spinner = true;
        SPINNER[self.view.spinner % SPINNER.len()]
    }

    fn header(&mut self, r: Rect) {
        match &self.app.mode {
            Mode::Create {
                repo,
                base,
                input,
                busy,
                error,
                ..
            } => {
                let mut spans = vec![
                    Span::styled("new worktree in ", theme::muted()),
                    Span::styled(repo.clone(), theme::text().add_modifier(Modifier::BOLD)),
                ];
                if let Some(base) = base {
                    spans.push(Span::styled(" off ", theme::muted()));
                    spans.push(Span::styled(base.clone(), Style::new().fg(PURPLE)));
                }
                spans.push(Span::styled("  ›  ", theme::faint()));
                let at = put(self.buf, r.x, r.y, &Line::from(spans), r.width);
                let end = if input.is_empty() {
                    put(
                        self.buf,
                        at,
                        r.y,
                        &Line::styled("name", theme::faint()),
                        r.right() - at,
                    );
                    at
                } else {
                    put(
                        self.buf,
                        at,
                        r.y,
                        &Line::styled(input.clone(), theme::text()),
                        r.right() - at,
                    )
                };
                if *busy {
                    let frame = self.spinner();
                    put(
                        self.buf,
                        end + 2,
                        r.y,
                        &Line::styled(format!("{frame} creating"), Style::new().fg(AQUA)),
                        r.right().saturating_sub(end + 2),
                    );
                } else if let Some(cell) = self.buf.cell_mut((end, r.y)) {
                    cell.set_symbol("▏").set_fg(YELLOW);
                }
                if let Some(error) = error {
                    put(
                        self.buf,
                        r.x,
                        r.y + 1,
                        &Line::styled(format!("✕ {error}"), Style::new().fg(RED)),
                        r.width,
                    );
                }
            }
            _ => {
                let query = &self.app.query;
                let mut spans = vec![Span::styled("⌕  ", theme::faint())];
                if query.is_empty() {
                    spans.push(Span::styled("search", theme::faint()));
                } else {
                    spans.push(Span::styled(query.clone(), theme::text()));
                }
                let end = put(self.buf, r.x, r.y, &Line::from(spans), r.width);
                if !query.is_empty()
                    && let Some(cell) = self.buf.cell_mut((end, r.y))
                {
                    cell.set_symbol("▏").set_fg(YELLOW);
                }
                let summary = self.summary();
                let w = width(&summary.spans);
                put(self.buf, r.right().saturating_sub(w), r.y, &summary, w);
            }
        }
    }

    /// "2 need you · 3 working": the one thing to notice, top right.
    fn summary(&mut self) -> Line<'static> {
        let rows = || {
            self.app
                .model
                .groups
                .iter()
                .flat_map(|group| &group.rows)
                .flat_map(|row| std::iter::once(row).chain(&row.children))
        };
        let blocked = rows()
            // The Inbox repeats its conversations' questions: count each question once.
            .filter(|row| {
                row.state == "blocked" && row.children.is_empty() && row.kind != Kind::Inbox
            })
            .count();
        let working = rows()
            .filter(|row| row.state == "working" && row.children.is_empty())
            .count();
        let mut spans = Vec::new();
        if blocked > 0 {
            spans.push(Span::styled(
                format!("{blocked} need{} you", if blocked == 1 { "s" } else { "" }),
                Style::new().fg(YELLOW),
            ));
        }
        if working > 0 {
            if !spans.is_empty() {
                spans.push(Span::styled("  ·  ", theme::faint()));
            }
            spans.push(Span::styled(format!("{working} working"), theme::faint()));
        }
        Line::from(spans)
    }

    fn list(&mut self, r: Rect) {
        let items = &self.app.items;
        if items.is_empty() {
            let text = if self.app.query.is_empty() {
                "nothing running"
            } else {
                "no matches"
            };
            put(
                self.buf,
                r.x + 2,
                r.y,
                &Line::styled(text, theme::faint()),
                r.width,
            );
            return;
        }
        let height = usize::from(r.height);
        let selected = self.app.selected.unwrap_or(0);
        // Keep two rows of context around the highlight; show a group's header when at its top.
        let margin = 2.min(height / 4);
        if selected < self.view.offset + margin {
            self.view.offset = selected.saturating_sub(margin);
            while self.view.offset > 0 && !items[self.view.offset - 1].selectable() {
                self.view.offset -= 1;
                if matches!(items[self.view.offset].kind, ItemKind::Header { .. }) {
                    break;
                }
            }
        } else if selected + margin + 1 > self.view.offset + height {
            self.view.offset = (selected + margin + 1).saturating_sub(height);
        }
        self.view.offset = self.view.offset.min(items.len().saturating_sub(height));
        let offset = self.view.offset;
        for (line, index) in (offset..items.len().min(offset + height)).enumerate() {
            let y = r.y + line as u16;
            let row_rect = Rect::new(r.x, y, r.width, 1);
            self.item(index, row_rect);
        }
        if offset > 0 {
            set(self.buf, r.right() - 1, r.y, "↑", FAINT);
        }
        if offset + height < items.len() {
            set(self.buf, r.right() - 1, r.bottom() - 1, "↓", FAINT);
        }
    }

    fn highlight_colour(&self, index: usize) -> Option<Color> {
        let raised = if matches!(self.app.mode, Mode::Kill { .. }) {
            KILL_TINT
        } else {
            RAISED
        };
        let t = self.view.selection.map_or(1.0, |(_, at)| {
            motion::progress(at, Duration::from_millis(SELECT_MS), self.now)
        });
        if self.app.selected == Some(index) {
            Some(theme::blend(PAGE, raised, t))
        } else if self
            .view
            .selection
            .is_some_and(|(from, _)| from == Some(index))
            && t < 1.0
        {
            Some(theme::blend(raised, PAGE, t))
        } else {
            None
        }
    }

    fn item(&mut self, index: usize, r: Rect) {
        let item: &Item = &self.app.items[index];
        let selected = self.app.selected == Some(index);
        if let Some(colour) = self.highlight_colour(index) {
            pill(self.buf, r, colour);
        }
        let x = r.x + 2;
        let w = r.width.saturating_sub(4);
        match item.kind {
            ItemKind::Header { group, attention } => {
                let name = match self.app.model.groups[group].kind {
                    Kind::Ultimate => "Ultimate",
                    _ => "Rex",
                };
                let mut spans = vec![Span::styled(
                    name,
                    theme::muted().add_modifier(Modifier::BOLD),
                )];
                if attention > 0 {
                    spans.push(Span::styled(format!("  {attention}"), theme::faint()));
                }
                put(self.buf, x, r.y, &Line::from(spans), w);
            }
            ItemKind::Gap => {}
            ItemKind::Project { ref name } => {
                put(
                    self.buf,
                    x + 2,
                    r.y,
                    &Line::styled(name.clone(), theme::text().add_modifier(Modifier::BOLD)),
                    w.saturating_sub(2),
                );
            }
            ItemKind::More { count, .. } => {
                put(
                    self.buf,
                    x + 4,
                    r.y,
                    &Line::styled(format!("+{count} more"), theme::faint()),
                    w,
                );
            }
            ItemKind::Row { .. } | ItemKind::Child { .. } => {
                let Some((row, parent)) = self.app.row(item) else {
                    return;
                };
                self.view.hits.push((r, Hit::Item(index)));
                let child = parent.is_some();
                let spinner = if row.state == "working" {
                    self.spinner()
                } else {
                    ""
                };
                let (glyph, colour, word) = status(row, spinner);
                let indent = if child { 3 } else { 0 } + if item.nested { 2 } else { 0 };
                let mut left = vec![Span::styled(glyph, Style::new().fg(colour)), Span::raw(" ")];
                if child {
                    let base = if selected {
                        theme::text()
                    } else {
                        theme::muted()
                    };
                    left.extend(highlight(&row.name, &item.hits, base));
                    if row.kind == Kind::Inbox {
                        // The count is the row's point; amber only when a question waits.
                        let count = if row.attention {
                            Style::new().fg(YELLOW)
                        } else {
                            theme::faint()
                        };
                        left.push(Span::styled(
                            format!("  {}", row.metadata.agent_count),
                            count,
                        ));
                    }
                } else {
                    left.extend(name_spans(row, &item.hits, selected, item.short.as_deref()));
                }
                if let Some(word) = word {
                    left.push(Span::styled(format!("  {word}"), Style::new().fg(colour)));
                }
                let (right, pr) = if child {
                    (
                        vec![Span::styled(
                            format!("{:>3}", age(self.clock, row.last_activity)),
                            theme::faint(),
                        )],
                        None,
                    )
                } else {
                    meta(row, self.clock)
                };
                let right_w = width(&right);
                let right_x = (x + w).saturating_sub(right_w);
                let left_max = right_x.saturating_sub(x + indent + 2);
                if let Some((text, hits)) = &item.detail {
                    // The match is in the conversation or agent text: show where, in place of metadata.
                    let end = put(self.buf, x + indent, r.y, &Line::from(left), left_max);
                    let snippet = snippet(text, hits, usize::from((x + w).saturating_sub(end + 3)));
                    put(
                        self.buf,
                        end + 3,
                        r.y,
                        &snippet,
                        (x + w).saturating_sub(end + 3),
                    );
                } else {
                    put(self.buf, x + indent, r.y, &Line::from(left), left_max);
                    put(self.buf, right_x, r.y, &Line::from(right), right_w);
                    if let Some((url, start, len)) = pr {
                        self.view.hits.push((
                            Rect::new(right_x + start as u16, r.y, len as u16, 1),
                            Hit::Url(url),
                        ));
                    }
                }
                if let Some(at) = self.view.entered.get(&item.key) {
                    let alpha = motion::progress(*at, Duration::from_millis(ENTER_MS), self.now);
                    motion::fade_cells(self.buf, r, alpha);
                }
            }
        }
    }

    fn preview(&mut self, r: Rect) {
        card(self.buf, r, CARD);
        let inner = Rect::new(
            r.x + 3,
            r.y + 1,
            r.width.saturating_sub(6),
            r.height.saturating_sub(2),
        );
        let Some((row, parent)) = self.app.current() else {
            self.view.content = Rect::default();
            return;
        };
        // Header: who and where, available at once from the model.
        let (glyph, colour, word) = status(
            row,
            if row.state == "working" {
                self.spinner()
            } else {
                ""
            },
        );
        let mut title = vec![
            Span::styled(glyph, Style::new().fg(colour)),
            Span::raw(" "),
            Span::styled(row.name.clone(), theme::text().add_modifier(Modifier::BOLD)),
        ];
        if let Some(word) = word {
            title.push(Span::styled(format!("  {word}"), Style::new().fg(colour)));
        }
        put(self.buf, inner.x, inner.y, &Line::from(title), inner.width);
        let mut facts: Vec<String> = Vec::new();
        let home = dirs::home_dir().map(|home| home.to_string_lossy().into_owned());
        let tilde = |path: &str| match &home {
            Some(home) if path.starts_with(home.as_str()) => format!("~{}", &path[home.len()..]),
            _ => path.to_owned(),
        };
        if let Some(parent) = parent {
            facts.push(format!("in {}", parent.name));
        }
        if let Some(path) = row
            .metadata
            .worktree
            .as_deref()
            .or(row.metadata.project.as_deref())
        {
            facts.push(tilde(path));
        }
        if let Some(pr) = &row.metadata.pr {
            facts.push(pr.url.trim_start_matches("https://github.com/").to_owned());
        }
        if row.metadata.agent_count > 1 && parent.is_none() {
            facts.push(format!("{} agents", row.metadata.agent_count));
        }
        put(
            self.buf,
            inner.x + 2,
            inner.y + 1,
            &Line::styled(facts.join("  ·  "), theme::faint()),
            inner.width.saturating_sub(2),
        );
        let content = Rect::new(
            inner.x,
            inner.y + 3,
            inner.width,
            inner.height.saturating_sub(3),
        );
        self.view.content = content;
        let key = &row.key;
        let Some(preview) = self.view.preview.as_ref() else {
            put(
                self.buf,
                content.x,
                content.bottom().saturating_sub(1),
                &Line::styled("…", theme::faint()),
                1,
            );
            return;
        };
        let stale = &preview.key != key;
        match &preview.text {
            Ok(text) => {
                let ultimate =
                    matches!(row.kind, Kind::Ultimate | Kind::Conversation | Kind::Inbox);
                let lines = if ultimate {
                    wrap(text, content.width)
                } else {
                    text.lines.clone()
                };
                let height = usize::from(content.height);
                let scroll = usize::from(self.app.scroll).min(lines.len().saturating_sub(height));
                let end = lines.len() - scroll;
                let start = end.saturating_sub(height);
                // Anchored to the bottom: the newest output sits just above the card's edge.
                // The Inbox is a list read from the top, oldest first.
                let top = if row.kind == Kind::Inbox {
                    content.y
                } else {
                    content.bottom() - (end - start) as u16
                };
                for (offset, line) in lines[start..end].iter().enumerate() {
                    let line = Line::from(
                        line.spans
                            .iter()
                            .map(|span| {
                                // Markdown headings style the whole line, not their spans.
                                let mut style = line.style.patch(span.style);
                                if style.bg == Some(Color::Reset) {
                                    style.bg = None;
                                }
                                Span::styled(span.content.clone(), style)
                            })
                            .collect::<Vec<_>>(),
                    );
                    put(
                        self.buf,
                        content.x,
                        top + offset as u16,
                        &line,
                        content.width,
                    );
                }
                if scroll > 0 {
                    let label = format!(" ↓ {scroll} ");
                    put(
                        self.buf,
                        content.right().saturating_sub(label.width() as u16),
                        content.bottom() - 1,
                        &Line::styled(label, theme::faint().bg(RAISED)),
                        content.width,
                    );
                }
            }
            Err(error) => {
                put(
                    self.buf,
                    content.x,
                    content.bottom().saturating_sub(1),
                    &Line::styled(format!("no preview: {error}"), theme::faint()),
                    content.width,
                );
            }
        }
        if stale {
            // The old content steps back while the new one loads; it then fades in over it.
            let since = self.view.stale_since.unwrap_or(self.now);
            let t = motion::progress(since, Duration::from_millis(SELECT_MS), self.now);
            motion::fade_cells(self.buf, content, 1.0 - 0.7 * t);
        }
    }

    fn footer(&mut self, r: Rect) {
        let key = |k: &str| Span::styled(k.to_owned(), theme::muted());
        let label = |l: &str| Span::styled(format!(" {l}     "), theme::faint());
        let line = match (&self.app.mode, &self.app.note) {
            (Mode::Kill { name, .. }, _) => Line::from(vec![
                Span::styled("kill ", Style::new().fg(RED)),
                Span::styled(name.clone(), theme::text().add_modifier(Modifier::BOLD)),
                Span::styled("?", Style::new().fg(RED)),
                Span::styled("   processes stop, the worktree stays     ", theme::faint()),
                key("y"),
                label("kill"),
                key("any key"),
                label("cancel"),
            ]),
            (
                Mode::Create {
                    base,
                    branch,
                    busy: false,
                    ..
                },
                _,
            ) => {
                let mut spans = vec![key("↵"), label("create")];
                match (base, branch) {
                    (Some(_), _) => {
                        spans.extend([key("⇧^t"), label("off the default branch")]);
                    }
                    (None, Some(branch)) => {
                        spans.extend([key("⇧^t"), label(&format!("off {branch}"))]);
                    }
                    (None, None) => {}
                }
                spans.extend([key("esc"), label("cancel")]);
                Line::from(spans)
            }
            (Mode::Create { .. }, _) => Line::from(vec![key("rwt new"), label("running")]),
            (Mode::List, Some(Note::Error(text))) => {
                Line::styled(format!("✕ {text}"), Style::new().fg(RED))
            }
            (Mode::List, Some(Note::Info(text))) => Line::styled(text.clone(), theme::muted()),
            (Mode::List, None) => {
                let inbox = self
                    .app
                    .current()
                    .is_some_and(|(row, _)| row.kind == Kind::Inbox);
                let mut spans = vec![key("↵"), label(if inbox { "open inbox" } else { "go" })];
                if !self.app.query.is_empty() {
                    spans.extend([key("esc"), label("clear")]);
                } else if let Some((row, parent)) = self.app.current() {
                    let rex = matches!(row.kind, Kind::Rex | Kind::Agent);
                    let app = !row.metadata.apps.is_empty()
                        || parent.is_some_and(|parent| !parent.metadata.apps.is_empty());
                    if self.app.scroll > 0 {
                        spans.extend([key("^f"), label("newer")]);
                    } else {
                        spans.extend([key("^b"), label("scroll")]);
                    }
                    if app {
                        spans.extend([key("^o"), label("tidewave")]);
                    }
                    if rex || row.metadata.worktree.is_some() {
                        spans.extend([key("^t"), label("new worktree")]);
                    }
                    if rex {
                        spans.extend([key("^k"), label("kill")]);
                    }
                    spans.extend([key("esc"), label("close")]);
                }
                Line::from(spans)
            }
        };
        put(self.buf, r.x + 2, r.y, &line, r.width.saturating_sub(2));
    }
}

/// Up to `max` columns around the first match.
fn snippet(text: &str, hits: &[u32], max: usize) -> Line<'static> {
    let chars: Vec<char> = text
        .chars()
        .map(|c| if c == '\n' { ' ' } else { c })
        .collect();
    let first = hits.first().copied().unwrap_or_default() as usize;
    let start = first.saturating_sub(max / 4);
    let mut spans = Vec::new();
    if start > 0 {
        spans.push(Span::styled("…", theme::faint()));
    }
    for (index, c) in chars
        .iter()
        .enumerate()
        .skip(start)
        .take(max.saturating_sub(2))
    {
        let style = if hits.contains(&(index as u32)) {
            Style::new().fg(YELLOW)
        } else {
            theme::faint()
        };
        spans.push(Span::styled(c.to_string(), style));
    }
    Line::from(spans)
}

/// Word-wraps single-style lines (conversation prose); styled lines are clipped instead.
pub fn wrap(text: &Text<'static>, width: u16) -> Vec<Line<'static>> {
    let width = usize::from(width.max(10));
    let mut out = Vec::new();
    for line in &text.lines {
        let tool = line
            .spans
            .first()
            .is_some_and(|span| span.content == super::preview::TOOL_MARK);
        if line.width() <= width || tool {
            out.push(line.clone());
            continue;
        }
        // Styled words: (text, style), each word keeping the space that followed it.
        let mut words: Vec<(String, Style)> = Vec::new();
        for span in &line.spans {
            let style = line.style.patch(span.style);
            for word in span.content.split_inclusive(' ') {
                words.push((word.to_owned(), style));
            }
        }
        // A list item or quote continues under its text, not under its marker.
        let lead: String = line
            .spans
            .iter()
            .flat_map(|span| span.content.chars())
            .take_while(|c| {
                matches!(c, ' ' | '-' | '*' | '>' | '•') || c.is_ascii_digit() || *c == '.'
            })
            .collect();
        let indent = " ".repeat(lead.width().min(width / 2));
        let mut current: Vec<Span<'static>> = Vec::new();
        let mut used = 0;
        // A finished row drops its trailing space, so it fits the width exactly.
        let finish = |spans: &mut Vec<Span<'static>>| {
            let mut spans = std::mem::take(spans);
            if let Some(last) = spans.last_mut() {
                last.content = last.content.trim_end().to_owned().into();
            }
            Line::from(spans)
        };
        for (word, style) in words {
            let mut word = word;
            if used > 0 && used + word.trim_end().width() > width {
                out.push(finish(&mut current));
                current.push(Span::raw(indent.clone()));
                used = indent.width();
            }
            while used + word.trim_end().width() > width {
                let room = width.saturating_sub(used).max(1);
                let cut: String = word.chars().take(room).collect();
                word = word.chars().skip(room).collect();
                current.push(Span::styled(cut, style));
                out.push(finish(&mut current));
                current.push(Span::raw(indent.clone()));
                used = indent.width();
            }
            used += word.width();
            current.push(Span::styled(word, style));
        }
        out.push(finish(&mut current));
    }
    out
}

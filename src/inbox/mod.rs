//! Full-tab, palette-only inbox. HTTP and process work never runs on the drawing thread.
pub mod actions;
pub mod data;
pub mod resume;

use crate::{
    pick::actions::{self as jump, Io},
    ultimate,
};
use anyhow::Result;
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers, KeyboardEnhancementFlags, MouseButton, MouseEventKind,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use data::{Row, Section};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::Rect,
    style::{Color, Modifier, Style},
    widgets::Paragraph,
};
use std::{
    io::{self, Write},
    sync::mpsc::{Sender, channel},
    thread,
    time::Duration,
};

struct System;
impl Io for System {
    fn http(
        &self,
        method: &str,
        url: &str,
        headers: &[(&str, &str)],
        body: Option<&str>,
    ) -> Result<String> {
        jump::System.http(method, url, headers, body)
    }
    fn run(&self, program: &str, args: &[&str], cwd: Option<&str>) -> Result<String> {
        let mut command = std::process::Command::new(program);
        command
            .args(args)
            .env_remove("OSC7577_TOKEN")
            .stdin(std::process::Stdio::null());
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        let out = command.output()?;
        anyhow::ensure!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}
struct Screen(Terminal<CrosstermBackend<io::Stdout>>);
impl Screen {
    fn new() -> Result<Self> {
        enable_raw_mode()?;
        let mut screen = Self(Terminal::new(CrosstermBackend::new(io::stdout()))?);
        execute!(
            screen.0.backend_mut(),
            EnterAlternateScreen,
            EnableMouseCapture,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES),
            crossterm::cursor::Hide
        )?;
        Ok(screen)
    }
}
impl Drop for Screen {
    fn drop(&mut self) {
        let _ = execute!(
            self.0.backend_mut(),
            PopKeyboardEnhancementFlags,
            DisableMouseCapture,
            LeaveAlternateScreen,
            crossterm::cursor::Show
        );
        let _ = disable_raw_mode();
    }
}
enum Message {
    Input(Event),
    Loaded(Result<Vec<Row>, String>),
    Acted(Result<(), String>, Option<Box<Row>>),
    Split(Result<Option<String>, String>),
}
#[derive(Default)]
enum Mode {
    #[default]
    Normal,
    Answer(Vec<String>),
    Dismiss,
}
#[derive(Default)]
struct App {
    rows: Vec<Row>,
    selected: usize,
    note: String,
    mode: Mode,
    loading: bool,
    busy: bool,
    previous: Option<String>,
    hits: Vec<(Rect, Hit)>,
    offset: usize,
}
#[derive(Clone)]
enum Hit {
    Row(usize),
    Key(char),
}
fn age(at: u64) -> String {
    let min = crate::now_ms().saturating_sub(at) / 60_000;
    if min < 60 {
        format!("{min} min ago")
    } else if min < 1440 {
        format!("{} h ago", min / 60)
    } else {
        format!("{} d ago", min / 1440)
    }
}
impl App {
    fn buttons(&self) -> Vec<(char, String)> {
        match &self.mode {
            Mode::Normal => [
                ('↑', "Up"),
                ('↓', "Down"),
                ('e', "Go to it"),
                ('d', "Done"),
                ('a', "Answer"),
                ('x', "Dismiss"),
                ('o', "Split"),
                ('r', "Refresh"),
                ('q', "Quit"),
            ]
            .into_iter()
            .map(|(k, s)| {
                (
                    k,
                    format!(
                        "{} {s}",
                        if k == 'e' {
                            "enter".into()
                        } else {
                            k.to_string()
                        }
                    ),
                )
            })
            .collect(),
            Mode::Dismiss => vec![('y', "y Dismiss?".into()), ('n', "n Cancel".into())],
            Mode::Answer(options) => options
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    (
                        char::from_digit((i + 1) as u32, 10)
                            .unwrap_or(char::from_u32(0xE000 + i as u32).unwrap()),
                        format!("{} {s}", i + 1),
                    )
                })
                .collect(),
        }
    }
    fn draw(&mut self, screen: &mut Screen) -> Result<()> {
        self.hits.clear();
        screen.0.draw(|f| {
            let area = f.area();
            let width = area.width;
            let height = area.height;
            let text = Style::default().fg(Color::Reset).bg(Color::Reset);
            let muted = text.fg(Color::DarkGray);
            let accent = text.fg(Color::Cyan).add_modifier(Modifier::BOLD);
            let mut line = |y: u16, s: String, style: Style| {
                if y < height {
                    f.render_widget(Paragraph::new(s).style(style), Rect::new(0, y, width, 1));
                }
            };
            let count = |section| {
                self.rows
                    .iter()
                    .filter(|r| r.section == section && !r.error && r.kind() != "empty")
                    .count()
            };
            line(
                0,
                format!(
                    "{} waiting on you · {} working · {} quiet",
                    count(Section::Waiting),
                    count(Section::Working),
                    count(Section::Quiet)
                ),
                accent,
            );
            let mut lines = Vec::new();
            for section in [Section::Waiting, Section::Working, Section::Quiet] {
                lines.push((section.title().to_owned(), None));
                let mut host = "";
                let mut any = false;
                for (i, r) in self
                    .rows
                    .iter()
                    .enumerate()
                    .filter(|(_, r)| r.section == section)
                {
                    any = true;
                    if host != r.host {
                        host = &r.host;
                        lines.push((format!("  {host}"), None));
                    }
                    let prefix = format!(
                        "  {} {} · [{}] ",
                        if i == self.selected { "›" } else { " " },
                        r.project,
                        r.kind()
                    );
                    let suffix = format!(" · {}", age(r.at));
                    let room = usize::from(width).saturating_sub(
                        unicode_width::UnicodeWidthStr::width(prefix.as_str())
                            + unicode_width::UnicodeWidthStr::width(suffix.as_str()),
                    );
                    let mut title = String::new();
                    let mut used = 0;
                    for c in r.title.replace(['\n', '\r'], " ").chars() {
                        let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
                        if used + w > room {
                            break;
                        }
                        title.push(c);
                        used += w;
                    }
                    lines.push((format!("{prefix}{title}{suffix}"), Some(i)));
                }
                if !any {
                    lines.push(("  —".into(), None));
                }
            }
            let buttons = self.buttons();
            let mut footer_lines = 1u16;
            let mut used = 0;
            for (_, s) in &buttons {
                let w = (s.chars().count() as u16 + 2).min(width);
                if used + w > width {
                    footer_lines += 1;
                    used = 0;
                }
                used += w + 1;
            }
            let room = height.saturating_sub(5 + footer_lines) as usize;
            if self.selected == 0 {
                self.offset = 0;
            }
            if let Some(pos) = lines.iter().position(|(_, i)| *i == Some(self.selected)) {
                if pos < self.offset {
                    self.offset = pos;
                }
                if room > 0 && pos >= self.offset + room {
                    self.offset = pos + 1 - room;
                }
            }
            for (n, (s, i)) in lines.into_iter().skip(self.offset).take(room).enumerate() {
                let y = n as u16 + 2;
                let style = if i == Some(self.selected) {
                    text.add_modifier(Modifier::REVERSED)
                } else if i.is_none() {
                    accent
                } else {
                    text
                };
                line(y, s, style);
                if let Some(i) = i {
                    self.hits.push((Rect::new(0, y, width, 1), Hit::Row(i)));
                }
            }
            let bottom = height.saturating_sub(3 + footer_lines);
            if let Some(r) = self.rows.get(self.selected) {
                line(
                    bottom,
                    format!("{} · {} · {} · {}", r.host, r.project, r.kind(), age(r.at)),
                    muted,
                );
                if let Some(e) = &r.entry {
                    line(
                        bottom + 1,
                        e.questions
                            .first()
                            .map(|q| {
                                format!(
                                    "{} {}",
                                    q.text,
                                    if q.kind == "confirm" {
                                        "Yes / No".into()
                                    } else {
                                        q.options
                                            .iter()
                                            .map(|o| o.label.as_str())
                                            .collect::<Vec<_>>()
                                            .join(" / ")
                                    }
                                )
                            })
                            .unwrap_or_else(|| e.text.lines().next().unwrap_or_default().into()),
                        text,
                    );
                }
            }
            line(
                bottom + 2,
                if self.loading {
                    "Loading…".into()
                } else if self.busy {
                    "Working…".into()
                } else {
                    self.note.clone()
                },
                text.fg(Color::Yellow),
            );
            let (mut x, mut y) = (0, height.saturating_sub(footer_lines));
            for (k, s) in buttons {
                let w = (s.chars().count() as u16 + 2).min(width);
                if x + w > width {
                    x = 0;
                    y += 1;
                }
                if y >= height {
                    break;
                }
                let rect = Rect::new(x, y, w, 1);
                f.render_widget(
                    Paragraph::new(format!(" {s} ")).style(text.add_modifier(Modifier::REVERSED)),
                    rect,
                );
                self.hits.push((rect, Hit::Key(k)));
                x += w + 1;
            }
        })?;
        Ok(())
    }
    fn key(&mut self, key: KeyEvent, tx: &Sender<Message>, refresh: &Sender<()>) -> bool {
        let c = match key.code {
            KeyCode::Char(c) => c,
            KeyCode::Enter => 'e',
            KeyCode::Esc => {
                if matches!(self.mode, Mode::Normal) {
                    'q'
                } else {
                    'n'
                }
            }
            KeyCode::Up => '↑',
            KeyCode::Down => '↓',
            _ => return false,
        };
        if !matches!(self.mode, Mode::Normal) {
            let mode = std::mem::take(&mut self.mode);
            match mode {
                Mode::Dismiss if c == 'y' => self.act("dismiss", None, tx),
                Mode::Answer(options) => {
                    if let Some(option) = c
                        .to_digit(10)
                        .and_then(|n| n.checked_sub(1))
                        .or_else(|| (c as u32).checked_sub(0xE000))
                        .and_then(|i| options.get(i as usize))
                    {
                        self.act("answer", Some(option.clone()), tx);
                    }
                }
                _ => {}
            }
            return false;
        }
        match c {
            'q' => return true,
            '↑' | 'k' => self.selected = self.selected.saturating_sub(1),
            '↓' | 'j' => {
                self.selected = (self.selected + 1).min(self.rows.len().saturating_sub(1))
            }
            'n' if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.selected = (self.selected + 1).min(self.rows.len().saturating_sub(1))
            }
            'p' if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.selected = self.selected.saturating_sub(1)
            }
            'r' => {
                let _ = refresh.send(());
            }
            'd' => self.act("done", None, tx),
            'a' => {
                if let Some((_, options)) = self
                    .rows
                    .get(self.selected)
                    .and_then(|r| r.entry.as_ref())
                    .and_then(|e| e.answerable())
                {
                    self.mode = Mode::Answer(options);
                } else {
                    self.note = "answer this one in Ultimate (enter)".into();
                }
            }
            'x' => {
                if self
                    .rows
                    .get(self.selected)
                    .is_some_and(|r| r.kind() == "question")
                {
                    self.mode = Mode::Dismiss;
                }
            }
            'e' | 'o' if !self.busy => {
                if let Some(row) = self
                    .rows
                    .get(self.selected)
                    .filter(|r| !r.error && r.kind() != "empty")
                    .cloned()
                {
                    self.busy = true;
                    let tx = tx.clone();
                    let mut previous = self.previous.clone();
                    thread::spawn(move || {
                        if c == 'o' {
                            let result = actions::split(
                                &System,
                                std::env::var("REX_BLOCK").ok().as_deref(),
                                &mut previous,
                                &row,
                            )
                            .map(|()| previous)
                            .map_err(|e| format!("{e:#}"));
                            let _ = tx.send(Message::Split(result));
                        } else {
                            let result = (|| {
                                actions::guard(&ultimate::user_home()?, &row.url)?;
                                if let Some(e) = &row.entry {
                                    jump::jump_inbox(&System, &row.url, &e.base.key)
                                } else {
                                    jump::jump_ultimate(
                                        &System,
                                        &row.url,
                                        row.conversation.unwrap(),
                                    )
                                }
                            })()
                            .map_err(|e| format!("{e:#}"));
                            let _ = tx.send(Message::Acted(result, None));
                        }
                    });
                }
            }
            _ => {}
        }
        false
    }
    fn act(&mut self, operation: &'static str, option: Option<String>, tx: &Sender<Message>) {
        if self.busy {
            return;
        }
        let Some(row) = self.rows.get(self.selected).cloned() else {
            return;
        };
        if let Err(e) = actions::request(&row, operation, option.as_deref()) {
            self.note = e.to_string();
            return;
        }
        self.busy = true;
        let restore = if operation == "done" {
            self.rows.remove(self.selected);
            Some(Box::new(row.clone()))
        } else {
            None
        };
        let tx = tx.clone();
        thread::spawn(move || {
            let result =
                (|| actions::write(&ultimate::user_home()?, &row, operation, option.as_deref()))()
                    .map_err(|e| format!("{e:#}"));
            let _ = tx.send(Message::Acted(result, restore));
        });
    }
}
pub fn run(mut keys: Vec<KeyEvent>) -> Result<()> {
    if let Ok(token) = std::env::var("OSC7577_TOKEN") {
        print!(
            "{}",
            resume::report(std::process::id(), crate::now_ms() / 1000, &token)
        );
        io::stdout().flush()?;
    }
    let mut screen = Screen::new()?;
    let mut app = App {
        loading: true,
        ..App::default()
    };
    app.draw(&mut screen)?;
    let home = ultimate::user_home()?;
    let (tx, rx) = channel();
    let (refresh, refresh_rx) = channel();
    {
        let tx = tx.clone();
        thread::spawn(move || {
            loop {
                let result = data::load(&home).map_err(|e| format!("{e:#}"));
                if tx.send(Message::Loaded(result)).is_err() {
                    break;
                }
                let _ = refresh_rx.recv_timeout(Duration::from_secs(10));
            }
        });
    }
    {
        let tx = tx.clone();
        thread::spawn(move || {
            while let Ok(event) = event::read() {
                if tx.send(Message::Input(event)).is_err() {
                    break;
                }
            }
        });
    }
    loop {
        match rx.recv()? {
            Message::Loaded(result) => {
                app.loading = false;
                match result {
                    Ok(rows) if !app.busy && matches!(app.mode, Mode::Normal) => {
                        let key = app.rows.get(app.selected).map(|r| (r.url.clone(), r.key()));
                        app.selected = key
                            .and_then(|key| {
                                rows.iter().position(|r| (r.url.clone(), r.key()) == key)
                            })
                            .unwrap_or(app.selected)
                            .min(rows.len().saturating_sub(1));
                        app.rows = rows;
                    }
                    Ok(_) => {}
                    Err(e) => app.note = e,
                }
                // Demo keys need live rows; the first frame still shows Loading immediately.
                for key in std::mem::take(&mut keys) {
                    if app.key(key, &tx, &refresh) {
                        return Ok(());
                    }
                }
            }
            Message::Acted(result, restore) => {
                app.busy = false;
                match result {
                    Ok(()) => app.note = "Done".into(),
                    Err(e) => {
                        app.note = e;
                        if let Some(row) = restore {
                            app.rows.push(*row);
                            data::sort(&mut app.rows, &crate::needs::host_name());
                        }
                    }
                }
                let _ = refresh.send(());
            }
            Message::Split(result) => {
                app.busy = false;
                match result {
                    Ok(previous) => app.previous = previous,
                    Err(e) => app.note = e,
                }
            }
            Message::Input(Event::Key(key)) if key.kind != KeyEventKind::Release => {
                if app.key(key, &tx, &refresh) {
                    break;
                }
            }
            Message::Input(Event::Mouse(mouse)) => match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some((_, hit)) = app
                        .hits
                        .iter()
                        .find(|(r, _)| r.contains((mouse.column, mouse.row).into()))
                        .cloned()
                    {
                        match hit {
                            Hit::Row(i) => {
                                app.selected = i;
                                app.mode = Mode::Normal;
                            }
                            Hit::Key(c) => {
                                if app.key(
                                    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
                                    &tx,
                                    &refresh,
                                ) {
                                    break;
                                }
                            }
                        }
                    }
                }
                MouseEventKind::ScrollUp => app.selected = app.selected.saturating_sub(1),
                MouseEventKind::ScrollDown => {
                    app.selected = (app.selected + 1).min(app.rows.len().saturating_sub(1))
                }
                _ => {}
            },
            _ => {}
        }
        app.draw(&mut screen)?;
    }
    Ok(())
}

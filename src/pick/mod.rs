//! `rtm pick`: the launcher TUI. The first frame comes from the cached model; a background
//! thread refreshes it. The loop sleeps until input, data or an animation frame is due.
pub mod actions;
pub mod motion;
pub mod preview;
pub mod state;
pub mod theme;
pub mod ui;

use crate::{
    cache,
    model::{Group, JumpTarget, Kind, Model, PreviewSource},
    seen, snapshot,
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
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    buffer::Buffer,
    layout::{Position, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
};
use state::{App, Command, Mode, Note};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    io::{self, Stdout, Write},
    path::PathBuf,
    sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel},
    thread,
    time::{Duration, Instant},
};
use tachyonfx::EffectManager;

const DEBOUNCE: Duration = Duration::from_millis(60);
const FRAME: Duration = Duration::from_millis(16);
const SPIN: Duration = Duration::from_millis(100);
const REFRESH: Duration = Duration::from_secs(3);

enum Message {
    Input(Event),
    Model(Box<Model>),
    Created(Result<(), String>),
    Killed(String, Result<(), String>),
    Wake,
}

pub struct Options {
    pub client: Option<String>,
    pub block: Option<String>,
    /// Keys pressed before the first frame, e.g. "ctrl+k" (screenshots, tests).
    pub keys: Vec<KeyEvent>,
    pub bench: bool,
}

fn model_path() -> Result<PathBuf> {
    Ok(cache::root()?.join("model.json"))
}

fn empty() -> Model {
    Model {
        groups: vec![
            Group {
                kind: Kind::Ultimate,
                rows: Vec::new(),
            },
            Group {
                kind: Kind::Rex,
                rows: Vec::new(),
            },
        ],
    }
}

/// "ctrl+shift+t", "enter", "x": the subset of key names the debug flag accepts.
pub fn parse_key(name: &str) -> Option<KeyEvent> {
    let mut modifiers = KeyModifiers::NONE;
    let mut parts: Vec<&str> = name.split('+').collect();
    let last = parts.pop()?;
    for part in parts {
        modifiers |= match part {
            "ctrl" => KeyModifiers::CONTROL,
            "shift" => KeyModifiers::SHIFT,
            "alt" => KeyModifiers::ALT,
            _ => return None,
        };
    }
    let code = match last {
        "enter" => KeyCode::Enter,
        "esc" => KeyCode::Esc,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "backspace" => KeyCode::Backspace,
        "space" => KeyCode::Char(' '),
        _ if last.chars().count() == 1 => KeyCode::Char(last.chars().next()?),
        _ => return None,
    };
    Some(KeyEvent::new(code, modifiers))
}

struct Screen {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl Screen {
    fn new() -> Result<Self> {
        enable_raw_mode()?;
        let mut out = io::stdout();
        execute!(
            out,
            EnterAlternateScreen,
            EnableMouseCapture,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES),
            crossterm::cursor::Hide,
        )?;
        Ok(Self {
            terminal: Terminal::new(CrosstermBackend::new(out))?,
        })
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        let _ = execute!(
            self.terminal.backend_mut(),
            PopKeyboardEnhancementFlags,
            DisableMouseCapture,
            LeaveAlternateScreen,
            crossterm::cursor::Show,
        );
        let _ = disable_raw_mode();
    }
}

fn spawn_input(tx: Sender<Message>) {
    thread::spawn(move || {
        while let Ok(event) = event::read() {
            if tx.send(Message::Input(event)).is_err() {
                return;
            }
        }
    });
}

/// Rex can record when a session was last looked at (a `rtm_seen` action); merge it in.
fn merge_rex_seen() {
    let Ok(output) = std::process::Command::new(actions::rex_bin())
        .args(["--autostart=false", "do", "rtm_seen"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
    else {
        return;
    };
    if !output.status.success() {
        return;
    }
    let Ok(values) = serde_json::from_slice::<BTreeMap<String, u64>>(&output.stdout) else {
        return;
    };
    let Ok(mut store) = seen::path().and_then(seen::Store::open) else {
        return;
    };
    for (session, at) in values {
        let key = format!("rex:{session}");
        if store.get(&key) < at {
            let _ = store.mark_at(&key, at);
        }
    }
}

fn spawn_refresh(tx: Sender<Message>, now: Receiver<()>) {
    thread::spawn(move || {
        merge_rex_seen();
        loop {
            if let Ok(snapshot) = snapshot::load(false) {
                if let Ok(path) = model_path() {
                    let _ = cache::write(&path, &snapshot.model);
                }
                if tx.send(Message::Model(Box::new(snapshot.model))).is_err() {
                    return;
                }
            }
            match now.recv_timeout(REFRESH) {
                Ok(()) | Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    });
}

fn preview_interval(kind: &PreviewSource) -> Duration {
    match kind {
        PreviewSource::Rex { .. } => Duration::from_millis(250),
        PreviewSource::Ultimate { .. } => Duration::from_secs(1),
    }
}

struct Loop {
    app: App,
    view: ui::View,
    effects: EffectManager<u8>,
    client: Option<String>,
    here: Option<String>,
    previews: Sender<preview::Request>,
    responses: Receiver<preview::Response>,
    cache: HashMap<String, (ratatui::text::Text<'static>, Instant)>,
    seq: u64,
    asked: Option<(String, Instant)>,
    due: Option<Instant>,
    took: Duration,
    tx: Sender<Message>,
    refresh_now: Sender<()>,
    last_frame: Instant,
    spun: Instant,
    known: HashSet<String>,
    first_model: bool,
}

impl Loop {
    fn current_preview(&self) -> Option<(String, PreviewSource, Option<String>)> {
        let (row, parent) = self.app.current()?;
        let source = row.preview.clone()?;
        let session = match (&row.jump, parent.map(|parent| &parent.jump)) {
            (JumpTarget::Rex { session, .. }, _) => Some(session.clone()),
            (_, Some(JumpTarget::Rex { session, .. })) => Some(session.clone()),
            _ => None,
        };
        Some((row.key.clone(), source, session))
    }

    /// Movement waits out the debounce; staying put refreshes on the source's interval.
    fn schedule_preview(&mut self, now: Instant, moved: bool) {
        let Some((key, ..)) = self.current_preview() else {
            self.due = None;
            return;
        };
        if moved {
            if let Some((text, at)) = self.cache.get(&key) {
                // Seen before: show it at once, then refresh it.
                self.view.preview = Some(ui::Preview {
                    key: key.clone(),
                    text: Ok(text.clone()),
                    at: *at,
                });
                self.view.stale_since = None;
            } else if self.view.preview.as_ref().is_some_and(|p| p.key != key) {
                self.view.stale_since.get_or_insert(now);
            }
            self.due = Some(now + DEBOUNCE);
        }
    }

    fn ask_preview(&mut self, now: Instant) {
        let Some((key, source, session)) = self.current_preview() else {
            return;
        };
        self.seq += 1;
        self.asked = Some((key.clone(), now));
        self.due = None;
        let _ = self.previews.send(preview::Request {
            seq: self.seq,
            key,
            source,
            session,
        });
    }

    fn take_previews(&mut self, now: Instant) {
        while let Ok(response) = self.responses.try_recv() {
            if response.seq != self.seq {
                continue;
            }
            self.asked = None;
            self.took = response.took;
            let current = self.current_preview();
            let Some((key, source, _)) = current else {
                continue;
            };
            if response.key != key {
                continue;
            }
            let changed = self
                .view
                .preview
                .as_ref()
                .is_none_or(|preview| preview.key != key || self.view.stale_since.is_some());
            if let Ok(text) = &response.text {
                self.cache.insert(key.clone(), (text.clone(), now));
            }
            self.view.preview = Some(ui::Preview {
                key,
                text: response.text,
                at: now,
            });
            self.view.stale_since = None;
            if changed && self.view.content.width > 0 {
                self.effects
                    .add_unique_effect(1, motion::reveal(self.view.content, 160));
            }
            // A slow capture (huge scrollback) refreshes less often instead of hogging a core.
            self.due = Some(now + preview_interval(&source).max(self.took * 3));
        }
    }

    fn set_model(&mut self, model: Model, now: Instant) {
        let before = self.app.selected_item().map(|item| item.key.clone());
        self.app.set_model(model);
        self.mark_entered(now);
        let after = self.app.selected_item().map(|item| item.key.clone());
        if before != after {
            self.schedule_preview(now, true);
        }
        self.first_model = false;
    }

    /// Rows that were not on screen before fade in.
    fn mark_entered(&mut self, now: Instant) {
        let keys: HashSet<String> = self.app.items.iter().map(|item| item.key.clone()).collect();
        if !self.first_model {
            for key in keys.difference(&self.known) {
                self.view.entered.insert(key.clone(), now);
            }
        }
        self.view
            .entered
            .retain(|_, at| now < *at + Duration::from_millis(400));
        self.known = keys;
    }

    fn draw(&mut self, terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
        let now = Instant::now();
        let elapsed = now - self.last_frame;
        self.last_frame = now;
        let clock = crate::now_ms();
        let Self {
            app, view, effects, ..
        } = self;
        terminal.draw(|frame| {
            let area = frame.area();
            let buf = frame.buffer_mut();
            ui::draw(buf, app, view, now, clock);
            effects.process_effects(elapsed.into(), buf, area);
        })?;
        Ok(())
    }

    /// Runs a command; returns true when rtm should exit.
    fn command(&mut self, command: Command) -> bool {
        match command {
            Command::None => false,
            Command::Quit => true,
            Command::Jump { row, seen: keys } => {
                match actions::jump(&actions::System, self.client.as_deref(), &row) {
                    Ok(()) => {
                        if let Ok(mut store) = seen::path().and_then(seen::Store::open) {
                            for key in keys {
                                let _ = store.mark(&key);
                            }
                        }
                        true
                    }
                    Err(error) => {
                        self.app.note = Some(Note::Error(format!("{error:#}")));
                        false
                    }
                }
            }
            Command::OpenUrl(url) => {
                if let Err(error) = actions::System.run_open(&url) {
                    self.app.note = Some(Note::Error(format!("{error:#}")));
                }
                false
            }
            Command::Tidewave(url) => {
                let base = url.trim_end_matches("/tidewave/connect").to_owned();
                match actions::focus_or_open(&actions::System, &format!("{base}/tidewave"), &url) {
                    Ok(()) => true,
                    Err(error) => {
                        self.app.note = Some(Note::Error(format!("{error:#}")));
                        false
                    }
                }
            }
            Command::Kill { session, name } => {
                if self.here.as_deref() == Some(session.as_str()) {
                    return match actions::kill_detached(&session) {
                        Ok(()) => true,
                        Err(error) => {
                            self.app.note = Some(Note::Error(format!("{error:#}")));
                            false
                        }
                    };
                }
                let tx = self.tx.clone();
                thread::spawn(move || {
                    let result = actions::kill(&actions::System, &session)
                        .map_err(|error| format!("{error:#}"));
                    let _ = tx.send(Message::Killed(name, result));
                });
                false
            }
            Command::Create { path, name, base } => {
                let tx = self.tx.clone();
                thread::spawn(move || {
                    let result = main_checkout(&path)
                        .and_then(|repo| {
                            actions::create(&actions::System, &repo, &name, base.as_deref())
                        })
                        .map_err(|error| format!("{error:#}"));
                    let _ = tx.send(Message::Created(result));
                });
                false
            }
        }
    }

    fn mouse(&mut self, event: crossterm::event::MouseEvent, now: Instant) -> Command {
        let at = Position::new(event.column, event.row);
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let hit = self
                    .view
                    .hits
                    .iter()
                    .rev()
                    .find(|(rect, _)| rect.contains(at))
                    .map(|(_, hit)| hit.clone());
                match hit {
                    Some(ui::Hit::Url(url)) => return Command::OpenUrl(url),
                    Some(ui::Hit::Item(index)) if self.app.selected == Some(index) => {
                        return self
                            .app
                            .key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
                    }
                    Some(ui::Hit::Item(index)) => {
                        let from = self.app.selected;
                        self.app.select(index);
                        self.view.moved(from, now);
                        self.schedule_preview(now, true);
                    }
                    None => {}
                }
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let up = event.kind == MouseEventKind::ScrollUp;
                if self.view.content.contains(at) {
                    self.app.scroll = if up {
                        self.app.scroll.saturating_add(3)
                    } else {
                        self.app.scroll.saturating_sub(3)
                    };
                } else {
                    let code = if up { KeyCode::Up } else { KeyCode::Down };
                    return self.key(KeyEvent::new(code, KeyModifiers::NONE), now);
                }
            }
            _ => {}
        }
        Command::None
    }

    fn key(&mut self, key: KeyEvent, now: Instant) -> Command {
        let from = self.app.selected;
        let query = self.app.query.clone();
        let command = self.app.key(key);
        if self.app.query != query {
            self.mark_entered(now);
        }
        if self.app.selected != from || self.app.query != query {
            self.view.moved(from, now);
            self.schedule_preview(now, true);
        }
        command
    }

    fn next_deadline(&self, now: Instant) -> Option<Instant> {
        next_wake(
            self.due,
            self.asked.is_some(),
            (self.effects.is_running() || self.view.animating(now)).then_some(now + FRAME),
            (self.view.has_spinner || matches!(self.app.mode, Mode::Create { busy: true, .. }))
                .then_some(self.spun + SPIN),
        )
    }
}

/// When the loop must wake without input. A due preview only counts while no request is in
/// flight: until its response arrives (which wakes the loop itself) the deadline cannot be
/// acted on, and an expired one would otherwise mean zero-timeout waits and endless redraws.
pub fn next_wake(
    preview_due: Option<Instant>,
    in_flight: bool,
    frame: Option<Instant>,
    spin: Option<Instant>,
) -> Option<Instant> {
    [preview_due.filter(|_| !in_flight), frame, spin]
        .into_iter()
        .flatten()
        .min()
}

impl actions::System {
    fn run_open(&self, url: &str) -> Result<String> {
        actions::Io::run(self, "open", &[url], None)
    }
}

/// `rwt new` runs from the repo's main checkout so it bases on the default branch.
fn main_checkout(path: &str) -> Result<String> {
    Ok(crate::git::worktree(path)?.repo)
}

pub fn run(options: Options) -> Result<()> {
    let start = Instant::now();
    let model = model_path()
        .ok()
        .and_then(|path| cache::read::<Model>(&path).ok().flatten())
        .unwrap_or_else(empty);
    let mut screen = Screen::new()?;
    let (tx, rx) = channel();
    spawn_input(tx.clone());
    let (wake_tx, wake_rx) = channel::<()>();
    {
        let tx = tx.clone();
        thread::spawn(move || {
            while wake_rx.recv().is_ok() {
                if tx.send(Message::Wake).is_err() {
                    return;
                }
            }
        });
    }
    let (previews, responses) = preview::spawn(wake_tx);
    let (refresh_now, refresh_rx) = channel();
    let mut looper = Loop {
        app: App::new(model),
        view: ui::View::default(),
        effects: EffectManager::default(),
        client: options.client,
        here: std::env::var("REX_SESSION").ok(),
        previews,
        responses,
        cache: HashMap::new(),
        seq: 0,
        asked: None,
        due: None,
        took: Duration::ZERO,
        tx: tx.clone(),
        refresh_now,
        last_frame: start,
        spun: start,
        known: HashSet::new(),
        first_model: true,
    };
    let _ = options.block;
    looper.mark_entered(start);
    let now = Instant::now();
    for key in options.keys {
        let command = looper.key(key, now);
        looper.command(command);
    }
    looper.ask_preview(now);
    looper.effects.add_effect(motion::open(180));
    looper.draw(&mut screen.terminal)?;
    let first_frame = start.elapsed();
    log_first_frame(first_frame);
    if options.bench {
        drop(screen);
        println!("first frame: {:.1} ms", first_frame.as_secs_f64() * 1000.0);
        return Ok(());
    }
    spawn_refresh(tx, refresh_rx);
    loop {
        let now = Instant::now();
        let message = match looper.next_deadline(now) {
            Some(deadline) => match rx.recv_timeout(deadline.saturating_duration_since(now)) {
                Ok(message) => Some(message),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
            },
            None => Some(rx.recv()?),
        };
        let now = Instant::now();
        let mut quit = false;
        // Drain whatever else arrived, so a burst of keys is one frame.
        let mut messages: Vec<Message> = message.into_iter().collect();
        while let Ok(more) = rx.try_recv() {
            messages.push(more);
        }
        for message in messages {
            let command = match message {
                Message::Input(Event::Key(key)) if key.kind != KeyEventKind::Release => {
                    looper.key(key, now)
                }
                Message::Input(Event::Mouse(mouse)) => looper.mouse(mouse, now),
                Message::Input(_) | Message::Wake => Command::None,
                Message::Model(model) => {
                    looper.set_model(*model, now);
                    Command::None
                }
                Message::Created(result) => {
                    let ok = result.is_ok();
                    looper.app.created(result);
                    if ok {
                        return Ok(());
                    }
                    Command::None
                }
                Message::Killed(name, result) => {
                    looper.app.note = Some(match result {
                        Ok(()) => Note::Info(format!("stopped {name}")),
                        Err(error) => Note::Error(error),
                    });
                    let _ = looper.refresh_now.send(());
                    Command::None
                }
            };
            quit |= looper.command(command);
        }
        if quit {
            return Ok(());
        }
        looper.take_previews(now);
        if looper.due.is_some_and(|due| due <= now) && looper.asked.is_none() {
            looper.ask_preview(now);
        }
        if now >= looper.spun + SPIN {
            looper.view.spinner += 1;
            looper.spun = now;
        }
        looper.draw(&mut screen.terminal)?;
    }
}

fn log_first_frame(elapsed: Duration) {
    if let Ok(root) = cache::root()
        && let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(root.join("pick.log"))
    {
        let _ = writeln!(
            file,
            "{} first_frame_ms={:.2}",
            crate::now_ms(),
            elapsed.as_secs_f64() * 1000.0
        );
    }
}

/// `rtm kill <session> [client]`: the same question in a small layer of its own.
pub fn kill_prompt(session: &str, keys: Vec<KeyEvent>) -> Result<()> {
    let name = model_path()
        .ok()
        .and_then(|path| cache::read::<Model>(&path).ok().flatten())
        .and_then(|model| {
            model
                .groups
                .iter()
                .flat_map(|group| &group.rows)
                .find(|row| matches!(&row.jump, JumpTarget::Rex { session: s, .. } if s == session))
                .map(|row| row.name.clone())
        })
        .unwrap_or_else(|| {
            session
                .trim_start_matches("session:")
                .chars()
                .take(8)
                .collect()
        });
    let mut screen = Screen::new()?;
    let start = Instant::now();
    let mut effects: EffectManager<u8> = EffectManager::default();
    effects.add_effect(motion::open(160));
    let (tx, rx) = channel();
    spawn_input(tx);
    let mut pending: std::collections::VecDeque<KeyEvent> = keys.into();
    let mut last = start;
    loop {
        let now = Instant::now();
        let elapsed = now - last;
        last = now;
        screen.terminal.draw(|frame| {
            let area = frame.area();
            let buf = frame.buffer_mut();
            draw_kill(buf, area, &name);
            effects.process_effects(elapsed.into(), buf, area);
        })?;
        let key = if let Some(key) = pending.pop_front() {
            Some(key)
        } else {
            let timeout = if effects.is_running() {
                FRAME
            } else {
                Duration::from_secs(3600)
            };
            match rx.recv_timeout(timeout) {
                Ok(Message::Input(Event::Key(key))) if key.kind != KeyEventKind::Release => {
                    Some(key)
                }
                Ok(_) | Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
            }
        };
        if let Some(key) = key {
            if key.code == KeyCode::Char('y') && key.modifiers.is_empty() {
                drop(screen);
                // Usually the session this prompt runs in: detach so the kill outlives it.
                return actions::kill_detached(session);
            }
            return Ok(());
        }
    }
}

/// The question, centred, with its hint two rows below; tighter when short, alone when
/// shorter still. Everything is clipped to `area`.
pub fn draw_kill(buf: &mut Buffer, area: Rect, name: &str) {
    let area = area.intersection(buf.area);
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let cell = &mut buf[(x, y)];
            cell.reset();
            cell.set_bg(theme::PAGE);
        }
    }
    let gap = match area.height {
        0 => return,
        1 => None,
        2 => Some(1),
        _ => Some(2),
    };
    let block = gap.map_or(1, |gap| gap + 1);
    let y = area.y + (area.height - block) / 2;
    // A long name loses its middle, so the "?" and the name's distinct end stay readable.
    let room = usize::from(
        area.width
            .saturating_sub(if area.width > 8 { 4 } else { 0 }),
    )
    .saturating_sub("kill ?".len());
    let chars: Vec<char> = name.chars().collect();
    let name: String = if chars.len() > room && room >= 5 {
        let tail = (room - 1) * 2 / 3;
        let head = room - 1 - tail;
        chars[..head]
            .iter()
            .chain(['…'].iter())
            .chain(&chars[chars.len() - tail..])
            .collect()
    } else {
        name.to_owned()
    };
    let question = Line::from(vec![
        Span::styled("kill ", Style::new().fg(theme::RED)),
        Span::styled(name, theme::text().add_modifier(Modifier::BOLD)),
        Span::styled("?", Style::new().fg(theme::RED)),
    ]);
    let hint = Line::from(vec![
        Span::styled("y", theme::muted()),
        Span::styled(" kill     ", theme::faint()),
        Span::styled("any key", theme::muted()),
        Span::styled(" cancel", theme::faint()),
    ]);
    let margin = if area.width > 8 { 4 } else { 0 };
    let lines = [(0, Some(question)), (gap.unwrap_or(0), gap.map(|_| hint))];
    for (offset, line) in lines {
        let Some(line) = line else { continue };
        let width = (line.width() as u16).min(area.width.saturating_sub(margin));
        if width == 0 {
            continue;
        }
        let x = area.x + area.width.saturating_sub(width) / 2;
        buf.set_line(x, y + offset, &line, width);
    }
}

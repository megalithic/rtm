//! The picker's pure state: what is listed, what is selected, and what a key asks for.
//! No terminal, process or network access, so every key path is testable.
use crate::{
    model::{JumpTarget, Model, Row},
    search,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ItemKind {
    Header {
        group: usize,
        attention: usize,
    },
    Gap,
    Row {
        group: usize,
        row: usize,
    },
    Child {
        group: usize,
        row: usize,
        child: usize,
    },
    More {
        group: usize,
        row: usize,
        count: usize,
    },
}

#[derive(Clone, Debug)]
pub struct Item {
    pub kind: ItemKind,
    pub key: String,
    /// Matched characters of the name (Unicode scalar offsets).
    pub hits: Vec<u32>,
    /// When the match is not in the name: the text that matched and its offsets.
    pub detail: Option<(String, Vec<u32>)>,
}

impl Item {
    fn plain(kind: ItemKind, key: String) -> Self {
        Self {
            kind,
            key,
            hits: Vec::new(),
            detail: None,
        }
    }

    pub fn selectable(&self) -> bool {
        matches!(self.kind, ItemKind::Row { .. } | ItemKind::Child { .. })
    }
}

fn push_row(items: &mut Vec<Item>, group: usize, index: usize, row: &Row) {
    items.push(Item::plain(
        ItemKind::Row { group, row: index },
        row.key.clone(),
    ));
    for (child, sub) in row.children.iter().enumerate() {
        items.push(Item::plain(
            ItemKind::Child {
                group,
                row: index,
                child,
            },
            sub.key.clone(),
        ));
    }
    if row.metadata.hidden > 0 {
        items.push(Item::plain(
            ItemKind::More {
                group,
                row: index,
                count: row.metadata.hidden,
            },
            format!("{}/more", row.key),
        ));
    }
}

fn split(matches: &[search::TextMatch]) -> (Vec<u32>, Option<(String, Vec<u32>)>) {
    let name = matches.iter().find(|found| found.field == "name");
    let detail = match name {
        Some(_) => None,
        None => matches
            .iter()
            .max_by_key(|found| found.score)
            .map(|found| (found.text.clone(), found.indices.clone())),
    };
    (
        name.map(|found| found.indices.clone()).unwrap_or_default(),
        detail,
    )
}

/// Grouped (Ultimate, then Rex; attention first, a gap, then the rest) without a query;
/// one ranked list with a query, where a matching sub-row keeps its parent visible.
pub fn items(model: &Model, query: &str) -> Vec<Item> {
    let mut items = Vec::new();
    if query.trim().is_empty() {
        for (group, rows) in model.groups.iter().enumerate() {
            if rows.rows.is_empty() {
                continue;
            }
            if !items.is_empty() {
                items.push(Item::plain(ItemKind::Gap, format!("gap:{group}")));
            }
            let attention = rows.rows.iter().filter(|row| row.attention).count();
            items.push(Item::plain(
                ItemKind::Header { group, attention },
                format!("group:{group}"),
            ));
            for (index, row) in rows.rows.iter().enumerate() {
                if index > 0 && rows.rows[index - 1].attention && !row.attention {
                    items.push(Item::plain(ItemKind::Gap, format!("rest:{group}")));
                }
                push_row(&mut items, group, index, row);
            }
        }
        return items;
    }
    for hit in search::search(model, query) {
        let row = &model.groups[hit.group].rows[hit.row];
        let (hits, detail) = split(&hit.matches);
        items.push(Item {
            kind: ItemKind::Row {
                group: hit.group,
                row: hit.row,
            },
            key: row.key.clone(),
            hits,
            detail,
        });
        for found in &hit.children {
            let (hits, detail) = split(&found.matches);
            items.push(Item {
                kind: ItemKind::Child {
                    group: hit.group,
                    row: hit.row,
                    child: found.child,
                },
                key: row.children[found.child].key.clone(),
                hits,
                detail,
            });
        }
    }
    items
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    List,
    /// `kill <name>?`: y confirms, any other key cancels.
    Kill {
        session: String,
        name: String,
    },
    /// The inline "new worktree" input; `base` is set for ctrl+shift+t.
    Create {
        path: String,
        repo: String,
        base: Option<String>,
        /// The highlighted row's branch: ctrl+shift+t switches `base` between it and none.
        branch: Option<String>,
        input: String,
        busy: bool,
        error: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Note {
    Info(String),
    Error(String),
}

#[derive(Clone, Debug)]
pub enum Command {
    None,
    Quit,
    /// `seen` lists every key to mark as viewed (a sub-row marks its parent too).
    Jump {
        row: Box<Row>,
        seen: Vec<String>,
    },
    OpenUrl(String),
    Tidewave(String),
    Kill {
        session: String,
        name: String,
    },
    Create {
        path: String,
        name: String,
        base: Option<String>,
    },
}

pub struct App {
    pub model: Model,
    pub query: String,
    pub items: Vec<Item>,
    pub selected: Option<usize>,
    pub mode: Mode,
    pub note: Option<Note>,
    /// Lines scrolled up from the bottom of the preview.
    pub scroll: u16,
}

fn ctrl(key: &KeyEvent, letter: char) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::SHIFT)
        && key.code == KeyCode::Char(letter)
}

fn ctrl_shift(key: &KeyEvent, letter: char) -> bool {
    key.modifiers
        .contains(KeyModifiers::CONTROL | KeyModifiers::SHIFT)
        && matches!(key.code, KeyCode::Char(c) if c.eq_ignore_ascii_case(&letter))
}

fn repo_name(path: &str) -> String {
    let main = path.split("/.worktrees/").next().unwrap_or(path);
    main.rsplit('/').next().unwrap_or(main).to_owned()
}

impl App {
    pub fn new(model: Model) -> Self {
        let mut app = Self {
            items: items(&model, ""),
            model,
            query: String::new(),
            selected: None,
            mode: Mode::List,
            note: None,
            scroll: 0,
        };
        app.selected = app.first();
        app
    }

    fn first(&self) -> Option<usize> {
        self.items.iter().position(Item::selectable)
    }

    pub fn selected_item(&self) -> Option<&Item> {
        self.items.get(self.selected?)
    }

    /// The row an item shows, and its parent for a sub-row.
    pub fn row(&self, item: &Item) -> Option<(&Row, Option<&Row>)> {
        match item.kind {
            ItemKind::Row { group, row } => {
                Some((self.model.groups.get(group)?.rows.get(row)?, None))
            }
            ItemKind::Child { group, row, child } => {
                let parent = self.model.groups.get(group)?.rows.get(row)?;
                Some((parent.children.get(child)?, Some(parent)))
            }
            _ => None,
        }
    }

    pub fn current(&self) -> Option<(&Row, Option<&Row>)> {
        self.row(self.selected_item()?)
    }

    /// A live refresh replaces the model but keeps the highlighted row by key.
    pub fn set_model(&mut self, model: Model) {
        let key = self.selected_item().map(|item| item.key.clone());
        self.model = model;
        self.items = items(&self.model, &self.query);
        self.selected = key
            .and_then(|key| self.items.iter().position(|item| item.key == key))
            .or_else(|| self.first());
    }

    fn requery(&mut self) {
        self.items = items(&self.model, &self.query);
        self.selected = self.first();
        self.scroll = 0;
    }

    fn step(&mut self, forward: bool) {
        let Some(start) = self.selected else {
            self.selected = self.first();
            return;
        };
        let mut index = start;
        loop {
            index = if forward {
                index + 1
            } else if let Some(previous) = index.checked_sub(1) {
                previous
            } else {
                return;
            };
            match self.items.get(index) {
                None => return,
                Some(item) if item.selectable() => {
                    self.selected = Some(index);
                    self.scroll = 0;
                    return;
                }
                Some(_) => {}
            }
        }
    }

    pub fn select(&mut self, index: usize) {
        if self.items.get(index).is_some_and(Item::selectable) {
            self.selected = Some(index);
            self.scroll = 0;
        }
    }

    pub fn key(&mut self, key: KeyEvent) -> Command {
        match &mut self.mode {
            Mode::Kill { session, name } => {
                let command = if key.code == KeyCode::Char('y') && key.modifiers.is_empty() {
                    self.note = Some(Note::Info(format!("killing {name}…")));
                    Command::Kill {
                        session: session.clone(),
                        name: name.clone(),
                    }
                } else {
                    Command::None
                };
                self.mode = Mode::List;
                command
            }
            Mode::Create {
                path,
                base,
                branch,
                input,
                busy,
                error,
                ..
            } => {
                if *busy {
                    return Command::None;
                }
                if ctrl_shift(&key, 't') {
                    *base = match base {
                        Some(_) => None,
                        None => branch.clone(),
                    };
                    return Command::None;
                }
                match key.code {
                    KeyCode::Esc => self.mode = Mode::List,
                    KeyCode::Enter if !input.trim().is_empty() => {
                        *busy = true;
                        *error = None;
                        return Command::Create {
                            path: path.clone(),
                            name: input.trim().to_owned(),
                            base: base.clone(),
                        };
                    }
                    KeyCode::Backspace => {
                        input.pop();
                    }
                    KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        input.clear()
                    }
                    KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        input.push(c);
                        *error = None;
                    }
                    _ => {}
                }
                Command::None
            }
            Mode::List => self.list_key(key),
        }
    }

    fn list_key(&mut self, key: KeyEvent) -> Command {
        self.note = None;
        if ctrl_shift(&key, 't') {
            return self.create(true);
        }
        match key.code {
            KeyCode::Esc if !self.query.is_empty() => {
                self.query.clear();
                self.requery();
            }
            KeyCode::Esc => return Command::Quit,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return Command::Quit;
            }
            KeyCode::Enter => return self.jump(),
            KeyCode::Up => self.step(false),
            KeyCode::Down => self.step(true),
            _ if ctrl(&key, 'p') => self.step(false),
            _ if ctrl(&key, 'n') => self.step(true),
            _ if ctrl(&key, 'b') => self.scroll = self.scroll.saturating_add(10),
            _ if ctrl(&key, 'f') => self.scroll = self.scroll.saturating_sub(10),
            _ if ctrl(&key, 'u') => {
                self.query.clear();
                self.requery();
            }
            _ if ctrl(&key, 'o') => return self.tidewave(),
            _ if ctrl(&key, 't') => return self.create(false),
            _ if ctrl(&key, 'k') => self.kill(),
            KeyCode::Backspace => {
                if self.query.pop().is_some() {
                    self.requery();
                }
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.query.push(c);
                self.requery();
            }
            _ => {}
        }
        Command::None
    }

    fn jump(&self) -> Command {
        let Some((row, parent)) = self.current() else {
            return Command::None;
        };
        let mut seen = vec![row.key.clone()];
        if let Some(parent) = parent {
            seen.push(parent.key.clone());
        }
        // A Rex agent sub-row's key is "rex:<session>/<block>"; the session key is what
        // drives attention, so a parent row and its sub-rows share it.
        Command::Jump {
            row: Box::new(row.clone()),
            seen,
        }
    }

    fn tidewave(&mut self) -> Command {
        let app = self.current().and_then(|(row, parent)| {
            row.metadata
                .apps
                .first()
                .or_else(|| parent?.metadata.apps.first())
                .cloned()
        });
        match app {
            Some(app) => Command::Tidewave(format!(
                "{}/tidewave/connect",
                app.origin
                    .unwrap_or_else(|| format!("http://localhost:{}", app.port))
                    .trim_end_matches('/')
            )),
            None => {
                self.note = Some(Note::Info("no Phoenix app runs here".into()));
                Command::None
            }
        }
    }

    fn create(&mut self, off_branch: bool) -> Command {
        let Some((row, parent)) = self.current() else {
            return Command::None;
        };
        let pick = |row: &Row| {
            row.metadata
                .worktree
                .clone()
                .or_else(|| row.metadata.project.clone())
        };
        let Some(path) = pick(row).or_else(|| parent.and_then(pick)) else {
            self.note = Some(Note::Info("no git checkout here".into()));
            return Command::None;
        };
        let branch = row
            .metadata
            .branch
            .clone()
            .or_else(|| parent?.metadata.branch.clone());
        if off_branch && branch.is_none() {
            self.note = Some(Note::Info("no branch to branch off".into()));
            return Command::None;
        }
        let base = branch.clone().filter(|_| off_branch);
        self.mode = Mode::Create {
            repo: repo_name(&path),
            path,
            base,
            branch,
            input: String::new(),
            busy: false,
            error: None,
        };
        Command::None
    }

    fn kill(&mut self) {
        let target = self.current().and_then(|(row, parent)| match &row.jump {
            JumpTarget::Rex { session, .. } => {
                Some((session.clone(), parent.unwrap_or(row).name.clone()))
            }
            JumpTarget::Ultimate { .. } => None,
        });
        match target {
            Some((session, name)) => self.mode = Mode::Kill { session, name },
            None => self.note = Some(Note::Info("only Rex sessions can be killed".into())),
        }
    }

    /// Result of a `Create` command: on failure the input stays for another try.
    pub fn created(&mut self, result: Result<(), String>) {
        if let Mode::Create { busy, error, .. } = &mut self.mode {
            *busy = false;
            match result {
                Ok(()) => self.mode = Mode::List,
                Err(message) => *error = Some(message),
            }
        }
    }
}
